//! Draft edits must never affect the public catalog or runtime after a restart.
//! All writes use a disposable database, not the local site's pricing epochs.
use okapi::{console, gateway};
use okapi_domain::{GroupCode, ModelCode, TokenUsage, UserId};
use okapi_pricing::{CalcContext, RatioFp};
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn drafts_stay_private_until_publication_including_reload_and_legacy_snapshots() {
    okapi_store::test_support::assert_isolated();
    let url = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let admin = okapi_store::connect_pg(&url).await.unwrap();
    let db = format!("okapi_publication_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{db}\"")))
        .execute(&admin)
        .await
        .unwrap();
    let fresh = format!("{}/{db}", url.rsplit_once('/').unwrap().0);
    let result = std::panic::AssertUnwindSafe(check_publication(&fresh, &redis))
        .catch_unwind()
        .await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE \"{db}\" WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    if let Err(err) = result {
        std::panic::resume_unwind(err);
    }
}

use futures::FutureExt as _;

#[allow(clippy::too_many_lines)]
async fn check_publication(database: &str, redis: &str) {
    let state = gateway::build_state(database, redis, "publication-test", None, None)
        .await
        .unwrap();
    let pg = &state.pg;
    let actor = okapi_store::provision::create_user(pg, "publisher")
        .await
        .unwrap();
    let first =
        okapi_store::provision::create_model_ratio(pg, "published-model", "1.25", "2", "0.5")
            .await
            .unwrap();
    sqlx::query("UPDATE models SET vendor='Published Vendor' WHERE id=$1")
        .bind(first)
        .execute(pg)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let app = console::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    for path in ["/api/pricing", "/api/pricing/models", "/api/pricing/groups"] {
        let empty = get(&client, &root, path).await;
        assert_eq!(empty["total"], 0);
        assert_eq!(empty["pricing_epoch"], 0);
    }
    assert!(
        !gateway::pricing_loader::load_pricebook(pg)
            .await
            .unwrap()
            .has_model(&ModelCode::from("published-model"))
    );

    let mut snapshot = serde_json::to_value(
        okapi_store::pricing::load_pricing_source_rows(pg)
            .await
            .unwrap(),
    )
    .unwrap();
    // Historical publications predate these fields; defaults must remain usable.
    for model in snapshot["models"].as_array_mut().unwrap() {
        model.as_object_mut().unwrap().remove("modality_ratios");
    }
    let epoch = okapi_store::admin::publish_epoch(pg, actor, &snapshot)
        .await
        .unwrap();
    assert_eq!(epoch, 1);
    assert!(gateway::refresh_pricebook_if_newer(&state).await.unwrap());
    assert_eq!(charge(&state.pricebook.load()), 2_500_000);

    // Change an existing price, base, group ratio and introduce a draft-only model/group.
    sqlx::query("UPDATE model_pricing SET model_ratio=9,cache_ratio=0.1,modality_ratios='{\"audio_cache_read\":\"0.25\"}' WHERE model_id=$1")
        .bind(first).execute(pg).await.unwrap();
    sqlx::query("UPDATE price_groups SET group_ratio=3 WHERE group_code='default'")
        .execute(pg)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO price_groups(group_code,group_ratio,self_select) VALUES('draft-group',4,true)",
    )
    .execute(pg)
    .await
    .unwrap();
    okapi_store::provision::create_model_ratio(pg, "draft-model", "2", "1", "1")
        .await
        .unwrap();
    sqlx::query("UPDATE models SET vendor='Draft Vendor' WHERE model_name='draft-model'")
        .execute(pg)
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings(key,value) VALUES($1,'9000000') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
        .bind(gateway::pricing_loader::BASE_PRICE_SETTING).execute(pg).await.unwrap();
    for path in ["/api/pricing", "/api/pricing/models"] {
        let page = get(&client, &root, path).await;
        assert_eq!(page["total"], 1);
        assert_eq!(page["pricing_epoch"], epoch);
        assert_eq!(page["models"][0]["model_ratio"], "1.250000");
        assert_eq!(page["models"][0]["cache_ratio"], "0.500000");
        assert_eq!(page["models"][0]["modality_ratios"], json!({}));
        assert_eq!(page["models"][0]["base_price_per_1m_micro"], 2_000_000);
        assert!(!page.to_string().contains("draft-model"));
        assert!(!page.to_string().contains("Draft Vendor"));
        for query in [
            "model=draft-model",
            "q=draft-model",
            "vendor=Draft%20Vendor",
        ] {
            let filtered = format!("{path}?{query}");
            assert_eq!(get(&client, &root, &filtered).await["total"], 0);
            let head = client
                .head(format!("{root}{filtered}"))
                .send()
                .await
                .unwrap();
            assert_eq!(head.status(), 200);
            assert_eq!(head.headers()["x-total-count"], "0");
        }
    }
    let group = get(&client, &root, "/api/pricing/groups?code=default").await;
    assert_eq!(group["groups"][0]["ratio"], "1.000000");
    assert_eq!(
        get(&client, &root, "/api/pricing/groups?code=draft-group").await["total"],
        0
    );
    assert!(!gateway::refresh_pricebook_if_newer(&state).await.unwrap());
    let restarted = gateway::build_state(database, redis, "publication-restart", None, None)
        .await
        .unwrap();
    assert_eq!(charge(&restarted.pricebook.load()), 2_500_000);
    assert_eq!(restarted.pricebook.epoch(), epoch);
    assert!(
        !restarted
            .pricebook
            .load()
            .has_model(&ModelCode::from("draft-model"))
    );
    assert!(
        !restarted
            .pricebook
            .load()
            .has_group(&GroupCode::from("draft-group"))
    );

    let mut snapshot = serde_json::to_value(
        okapi_store::pricing::load_pricing_source_rows(pg)
            .await
            .unwrap(),
    )
    .unwrap();
    snapshot["base_price_per_1m_micro"] = json!(9_000_000);
    let next = okapi_store::admin::publish_epoch(pg, actor, &snapshot)
        .await
        .unwrap();
    assert!(gateway::refresh_pricebook_if_newer(&state).await.unwrap());
    assert_eq!(charge(&state.pricebook.load()), 243_000_000);
    let published = get(&client, &root, "/api/pricing?model=draft-model").await;
    assert_eq!(published["total"], 1);
    assert_eq!(published["pricing_epoch"], next);
    assert_eq!(published["models"][0]["model_ratio"], "2.000000");
    assert_eq!(published["models"][0]["base_price_per_1m_micro"], 9_000_000);
    assert_eq!(
        get(&client, &root, "/api/pricing/groups?code=default").await["groups"][0]["ratio"],
        "3.000000"
    );
    assert_eq!(
        get(&client, &root, "/api/pricing/groups?code=draft-group").await["total"],
        1
    );
    let loaded = gateway::pricing_loader::load_pricebook(pg).await.unwrap();
    assert_eq!(loaded.epoch(), next);
    assert_eq!(charge(&loaded), 243_000_000);
    assert!(loaded.has_model(&ModelCode::from("draft-model")));

    // A broken publication fails closed instead of falling back to draft tables.
    okapi_store::admin::publish_epoch(pg, actor, &json!({}))
        .await
        .unwrap();
    assert!(gateway::pricing_loader::load_pricebook(pg).await.is_err());
    assert!(gateway::refresh_pricebook_if_newer(&state).await.is_err());
    assert_eq!(
        state.pricebook.epoch(),
        next,
        "bad publication retains previous runtime book"
    );
    assert_eq!(
        client
            .get(format!("{root}/api/pricing"))
            .send()
            .await
            .unwrap()
            .status(),
        500
    );
    server.abort();
    restarted.pg.close().await;
    pg.close().await;
}

async fn get(client: &reqwest::Client, root: &str, path: &str) -> Value {
    client
        .get(format!("{root}{path}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn charge(book: &okapi_pricing::PriceBook) -> i64 {
    let context = CalcContext {
        user: UserId::new(1),
        model: ModelCode::from("published-model"),
        group: GroupCode::from("default"),
        user_multiplier: RatioFp::ONE,
        monthly_tokens: 0,
        monthly_spend_micro: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        utc_offset_seconds: 0,
        surge_active: false,
        service_tier: None,
    };
    let usage = TokenUsage {
        prompt_tokens: 1_000_000,
        ..TokenUsage::default()
    };
    okapi_pricing::calculate(book, &context, usage)
        .unwrap()
        .amount
        .as_micros()
}
