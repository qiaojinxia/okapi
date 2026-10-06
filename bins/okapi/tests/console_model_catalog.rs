//! Public model catalog: server-side filters and bounded pages, without credentials.
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{net::SocketAddr, time::Duration};
use uuid::Uuid;

#[path = "support/model_catalog_bounds.rs"]
mod bounds;
#[path = "support/catalog_visibility.rs"]
mod catalog_visibility;
#[path = "support/model_catalog_facets.rs"]
mod facets;
#[path = "support/model_catalog_ingress.rs"]
mod ingress_parity;
#[path = "support/model_catalog_pagination.rs"]
mod pagination;
#[path = "support/model_catalog_routes.rs"]
mod route_filters;

struct Env {
    pg: PgPool,
    addr: SocketAddr,
    prefix: String,
    models: Vec<String>,
    client: reqwest::Client,
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let state = gateway::build_state(&database, &redis, "catalog-test", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, console::router(state)).await.unwrap() });
    let prefix = format!("catalog-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let mut models = Vec::new();
    for i in 0..25 {
        let model = format!("{prefix}-{i:02}");
        okapi_store::provision::create_model_ratio(&pg, &model, "1.25", "2", "0.5")
            .await
            .unwrap();
        let vendor = match i {
            0..=7 => Some("OpenAI"),
            8..=14 => Some(" openai "),
            15..=23 => Some("Anthropic"),
            _ => None,
        };
        sqlx::query("UPDATE models SET vendor=$2,display_name=$3,sort_order=$4,capabilities=$5 WHERE model_name=$1")
            .bind(&model).bind(vendor).bind(format!("Display {prefix} {i}"))
            .bind(i / 5).bind(json!({"vision":i%2==0,"tools":false,"audio":"yes","private_note":"hidden-capability-note"}))
            .execute(&pg).await.unwrap();
        models.push(model);
    }
    let disabled = format!("{prefix}-disabled");
    okapi_store::provision::create_model_ratio(&pg, &disabled, "1", "1", "1")
        .await
        .unwrap();
    sqlx::query("UPDATE models SET status=2 WHERE model_name=$1")
        .bind(&disabled)
        .execute(&pg)
        .await
        .unwrap();
    sqlx::query("INSERT INTO models(model_name) VALUES($1)")
        .bind(format!("{prefix}-unpriced"))
        .execute(&pg)
        .await
        .unwrap();
    let env = Env {
        pg,
        addr,
        prefix,
        models,
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
    };
    env.publish().await;
    env
}

impl Env {
    // Fixtures must publish explicitly, just like an administrator. GET never
    // publishes: unpublished visibility is covered by pricing_publication.rs.
    async fn publish(&self) {
        let actor =
            okapi_store::provision::create_user(&self.pg, &format!("pub-{}", Uuid::new_v4()))
                .await
                .unwrap();
        let rows = okapi_store::pricing::load_pricing_source_rows(&self.pg)
            .await
            .unwrap();
        let snapshot = serde_json::to_value(rows).unwrap();
        okapi_store::admin::publish_epoch(&self.pg, actor, &snapshot)
            .await
            .unwrap();
    }
    fn url(&self, path: &str, query: &[(&str, &str)]) -> reqwest::Url {
        let mut url = reqwest::Url::parse(&format!("http://{}{path}", self.addr)).unwrap();
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        url
    }
    async fn get(&self, query: &[(&str, &str)]) -> Value {
        let started = std::time::Instant::now();
        let response = self
            .client
            .get(self.url("/api/pricing/models", query))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = response.bytes().await.unwrap();
        if query.is_empty() {
            eprintln!(
                "CATALOG_PAGE bytes={} elapsed_ms={}",
                bytes.len(),
                started.elapsed().as_millis()
            );
        }
        serde_json::from_slice(&bytes).unwrap()
    }
}

#[tokio::test]
async fn public_model_catalog_defaults_to_twenty() {
    let env = setup().await;
    let page = env.get(&[]).await;
    assert_eq!(page["models"].as_array().unwrap().len(), 20);
    assert_eq!(page["limit"], 20);
    assert_eq!(page["offset"], 0);
    assert!(page["total"].as_i64().unwrap() >= 25);
    assert_eq!(page["has_more"], true);
    assert_eq!(page["next_offset"], 20);

    let filtered = env.get(&[("q", &env.prefix)]).await;
    assert_eq!(filtered["models"].as_array().unwrap().len(), 20);
    assert_eq!(filtered["models"][0]["model"], env.models[0]);
    assert_eq!(filtered["total"], 25);
    let enabled: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM models WHERE model_name=ANY($1)")
        .bind(&env.models)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(enabled, 25);
}

fn names(page: &Value) -> Vec<String> {
    page["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["model"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn catalog_pages_and_vendor_facets_apply_filters_before_slicing() {
    let env = setup().await;
    let mut collected = Vec::new();
    for offset in [0, 7, 14, 21] {
        let page = env
            .get(&[
                ("q", &env.prefix),
                ("limit", "7"),
                ("offset", &offset.to_string()),
            ])
            .await;
        assert_eq!(page["total"], 25);
        assert_eq!(page["has_more"], offset != 21);
        collected.extend(names(&page));
    }
    assert_eq!(
        collected, env.models,
        "stable pages cannot duplicate or omit models"
    );
    let filtered = env
        .get(&[
            ("q", &env.prefix),
            ("vendor", " OPENAI "),
            ("capability", "vision"),
            ("limit", "2"),
        ])
        .await;
    assert_eq!(filtered["total"], 8);
    assert_eq!(
        names(&filtered),
        vec![env.models[0].clone(), env.models[2].clone()]
    );
    assert_eq!(
        filtered["vendors"],
        json!([
            {"vendor":"anthropic","count":4}, {"vendor":"openai","count":8}, {"vendor":null,"count":1}
        ])
    );
    assert_eq!(filtered["models"][0]["model_ratio"], "1.250000");
    assert_eq!(
        filtered["models"][0]["capabilities"],
        json!({"vision":true,"tools":false})
    );
    assert!(!filtered.to_string().contains("hidden-capability-note"));
    let unknown = env.get(&[("q", &env.prefix), ("vendor", "")]).await;
    assert_eq!(names(&unknown), vec![env.models[24].clone()]);
    let beyond = env
        .get(&[("q", &env.prefix), ("offset", "9223372036854775807")])
        .await;
    assert_eq!(beyond["total"], 25);
    assert_eq!(beyond["models"], json!([]));
    assert_eq!(beyond["next_offset"], Value::Null);
    let capped = env
        .get(&[("q", &env.prefix), ("limit", "9223372036854775807")])
        .await;
    assert_eq!(capped["limit"], 100);
    assert_eq!(names(&capped), env.models);
    assert_eq!(
        env.get(&[("q", &env.prefix), ("capability", "audio")])
            .await["total"],
        0
    );
}

#[tokio::test]
async fn catalog_search_is_literal_and_model_lookup_is_exact() {
    let env = setup().await;
    let special = format!("{}-literal%_\\", env.prefix);
    let decoy = format!("{}-literalAA", env.prefix);
    for model in [&special, &decoy] {
        okapi_store::provision::create_model_ratio(&env.pg, model, "1", "1", "1")
            .await
            .unwrap();
    }
    env.publish().await;
    assert_eq!(
        names(&env.get(&[("q", &special)]).await),
        vec![special.clone()]
    );
    assert_eq!(names(&env.get(&[("model", &special)]).await), vec![special]);
    assert_eq!(
        env.get(&[("model", &env.models[0].to_uppercase())]).await["total"],
        0
    );
    assert_eq!(
        env.get(&[("q", &format!("DISPLAY {}", env.prefix.to_uppercase()))])
            .await["total"],
        25
    );
    assert_eq!(env.get(&[("q", "' OR 1=1 --")]).await["total"], 0);
}

#[tokio::test]
async fn catalog_head_and_legacy_opt_in_keep_page_metadata_consistent() {
    let env = setup().await;
    // Scope group metadata too: other parallel fixtures may add public groups
    // between the two independent snapshots being compared.
    let args = [
        ("q", env.prefix.as_str()),
        ("group_q", env.prefix.as_str()),
        ("limit", "3"),
        ("offset", "20"),
    ];
    let url = env.url("/api/pricing/models", &args);
    let response = env.client.get(url.clone()).send().await.unwrap();
    assert_eq!(response.headers()["x-total-count"], "25");
    assert_eq!(response.headers()["x-page-limit"], "3");
    assert_eq!(response.headers()["x-page-offset"], "20");
    let page: Value = response.json().await.unwrap();
    let head = env.client.head(url).send().await.unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["x-total-count"], "25");
    assert_eq!(head.headers()["x-page-limit"], "3");
    assert_eq!(head.headers()["x-page-offset"], "20");
    assert!(head.headers().get("content-length").is_none());
    assert!(head.bytes().await.unwrap().is_empty());
    let old: Value = env
        .client
        .get(env.url("/api/pricing", &args))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(old, page);
    let only_flag: Value = env
        .client
        .get(env.url("/api/pricing", &[("paged", "true")]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(only_flag["models"].as_array().unwrap().len(), 20);
}

#[tokio::test]
async fn catalog_rejects_invalid_queries_for_get_and_head() {
    let env = setup().await;
    for (key, value) in [
        ("limit", "0"),
        ("limit", "-1"),
        ("offset", "-1"),
        ("limit", "twenty"),
        ("offset", "9223372036854775808"),
        ("paged", "maybe"),
        ("capability", "private_note"),
        ("endpoint", "/admin/channels"),
        ("q", &"x".repeat(257)),
        ("group", &"x".repeat(33)),
        ("vendor", &"x".repeat(129)),
        ("model", &"x".repeat(257)),
    ] {
        let url = env.url("/api/pricing/models", &[(key, value)]);
        let get = env.client.get(url.clone()).send().await.unwrap();
        assert_eq!(get.status(), 400, "{key}={value}");
        let body: Value = get.json().await.unwrap();
        assert_eq!(body["error"]["code"], "bad_request");
        let head = env.client.head(url).send().await.unwrap();
        assert_eq!(head.status(), 400);
        assert!(head.bytes().await.unwrap().is_empty());
    }
}
