//! Isolated database: never change a shared/live site's global pricing settings.
use okapi::{console, gateway};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[tokio::test]
async fn base_price_requires_publish_and_survives_reload() {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let admin_pool = okapi_store::connect_pg(&url).await.unwrap();
    let db = format!("okapi_base_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{db}\"")))
        .execute(&admin_pool)
        .await
        .unwrap();
    let fresh = format!("{}/{db}", url.rsplit_once('/').unwrap().0);
    let state = gateway::build_state(&fresh, &redis, "base-test", None, None)
        .await
        .unwrap();
    let tokens = [
        create_token(&state.pg, 100).await,
        create_token(&state.pg, 1).await,
    ];
    okapi_store::provision::create_model_ratio(&state.pg, "base-test-model", "1", "4", "0.5")
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}", listener.local_addr().unwrap());
    let app = console::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    validate_setting(&client, &root, &tokens).await;
    let saved = response_json(save_base(&client, &root, &tokens[0], 3_000_000)).await;
    assert_eq!(saved["requires_publish"], true);
    assert_eq!(loaded_base(&state.pg).await, 2_000_000);
    let preview = response_json(
        client
            .get(format!("{root}/admin/models"))
            .bearer_auth(&tokens[0]),
    )
    .await;
    assert_eq!(preview["base_price_per_1m_micro"], 3_000_000);
    assert_eq!(preview["published_base_price_per_1m_micro"], 2_000_000);
    let public = response_json(client.get(format!("{root}/api/pricing"))).await;
    assert_eq!(public["models"], json!([]), "未发布模型不得进入广场");
    assert_eq!(public["pricing_epoch"], 0);
    let outdated_publish = client
        .post(format!(
            "{root}/admin/pricing/publish?expected_base_per_1m_micro=2000000"
        ))
        .bearer_auth(&tokens[0])
        .send()
        .await
        .unwrap();
    assert_eq!(outdated_publish.status(), 400);
    assert_eq!(
        outdated_publish.json::<Value>().await.unwrap()["error"]["param"],
        "pricing_base_changed"
    );
    let published = client
        .post(format!("{root}/admin/pricing/publish"))
        .bearer_auth(&tokens[0])
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(published["epoch"].as_i64().unwrap() > 0);
    assert!(gateway::refresh_pricebook_if_newer(&state).await.unwrap());
    assert_eq!(state.pricebook.load().base_price_per_1m_micro(), 3_000_000);
    assert_eq!(loaded_base(&state.pg).await, 3_000_000);
    let public = response_json(client.get(format!("{root}/api/pricing"))).await;
    assert_eq!(public["models"][0]["base_price_per_1m_micro"], 3_000_000);
    // A later draft cannot leak into startup or the public catalog.
    save_base(&client, &root, &tokens[0], 9_000_000)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(loaded_base(&state.pg).await, 3_000_000);
    server.abort();
    state.pg.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE \"{db}\" WITH (FORCE)"
    )))
    .execute(&admin_pool)
    .await
    .unwrap();
}

async fn create_token(pg: &sqlx::PgPool, role: i32) -> String {
    let id = okapi_store::provision::create_user(pg, &format!("base-{role}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(id)
        .execute(pg)
        .await
        .unwrap();
    let token = format!("fixture-{}", Uuid::new_v4());
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    okapi_store::provision::create_api_key(pg, id, &hash, "fixture")
        .await
        .unwrap();
    token
}

async fn validate_setting(client: &reqwest::Client, root: &str, tokens: &[String; 2]) {
    let key = gateway::pricing_loader::BASE_PRICE_SETTING;
    let default = client
        .get(format!("{root}/admin/settings"))
        .bearer_auth(&tokens[0])
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let row = default["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap();
    assert_eq!(row["value"], 2_000_000);
    assert_eq!(row["published_value"], 2_000_000);
    for value in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!(null),
        json!("3"),
        json!(1_000_000_000_001_i64),
    ] {
        let response = client
            .post(format!("{root}/admin/settings"))
            .bearer_auth(&tokens[0])
            .json(&json!({"key":key,"value":value}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let denied = client
        .post(format!("{root}/admin/settings"))
        .bearer_auth(&tokens[1])
        .json(&json!({"key":key,"value":3_000_000}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
}

fn save_base(
    client: &reqwest::Client,
    root: &str,
    token: &str,
    value: i64,
) -> reqwest::RequestBuilder {
    client
        .post(format!("{root}/admin/settings"))
        .bearer_auth(token)
        .json(&json!({"key":gateway::pricing_loader::BASE_PRICE_SETTING,"value":value}))
}

async fn response_json(request: reqwest::RequestBuilder) -> Value {
    request
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn loaded_base(pg: &sqlx::PgPool) -> i64 {
    gateway::pricing_loader::load_pricebook(pg)
        .await
        .unwrap()
        .base_price_per_1m_micro()
}
