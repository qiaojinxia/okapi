//! M3 /v1/images/generations 验收：per_call × n 计费、media_units 入快照、
//! 失败退款。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_images(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert!(req["model"].as_str().unwrap().starts_with("img-"));
    let n = usize::try_from(req["n"].as_u64().unwrap_or(1)).unwrap_or(1);
    axum::Json(json!({
        "created": 1_700_000_000,
        "data": (0..n).map(|_| json!({"url": "https://img.example/x.png"})).collect::<Vec<_>>()
    }))
    .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new().route("/v1/images/generations", post(mock_images));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(balance_micro: i64) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("img-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-test-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();
    // per_call $0.04/张 = 40_000 micro
    let model_id = sqlx::query_scalar!(
        r#"INSERT INTO models (model_name) VALUES ($1) RETURNING id"#,
        model
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    sqlx::query!(
        r#"INSERT INTO model_pricing (model_id, pricing_mode, per_call_price_micro)
           VALUES ($1, 'per_call', 40000)"#,
        model_id
    )
    .execute(&pg)
    .await
    .unwrap();

    let mock = spawn_mock().await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("img-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(balance_micro))
        .await
        .unwrap();
    let ledger = state.ledger.clone();

    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        ledger,
        gateway: addr,
        token,
        user_id,
        model,
    }
}

/// 3 张图：per_call 40_000 × 3 = 120_000；media_units 入快照。
#[tokio::test]
async fn images_bill_per_call_times_n() {
    let env = setup(1_000_000).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/images/generations", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "prompt": "a cat", "n": 3}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["data"].as_array().unwrap().len(), 3, "响应原样透传");

    let mut rec = None;
    for _ in 0..50 {
        rec = sqlx::query!(
            r#"SELECT amount_micro, pricing_snapshot FROM billing_records
               WHERE user_id = $1 AND log_type = 2"#,
            env.user_id
        )
        .fetch_optional(&env.pg)
        .await
        .unwrap();
        if rec.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let rec = rec.expect("必须记账");
    assert_eq!(rec.amount_micro, 120_000, "$0.04 × 3");
    let snapshot = rec.pricing_snapshot.expect("必须携带快照");
    assert_eq!(snapshot["media_units"], 3, "乘数必须可解释");
    assert_eq!(snapshot["mode"], "per_call");

    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 1_000_000 - 120_000);
}

/// 余额不足以覆盖 n 张：reserve 拒绝，分文未动。
#[tokio::test]
async fn images_insufficient_for_batch() {
    // 余额 300_000 < n=10 × 40_000
    let env = setup(300_000).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/images/generations", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "prompt": "a cat", "n": 10}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "insufficient_quota");
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 300_000, "拒绝时分文未动");
}
