//! 门户第二批端点验收：/api/pricing 公开价格（无鉴权）+ /api/me/logs
//! 账单明细（含 pricing_snapshot，own 隔离）。依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestEnv { pg, addr }
}

async fn mk_user(pg: &PgPool) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(pg, &format!("pp-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-pp-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(pg, user_id, &key_hash, "sk-okapi-pp")
        .await
        .unwrap();
    (user_id, token)
}

/// 公开价格页：无鉴权可访问，含倍率与分组；不泄漏渠道信息。
#[tokio::test]
async fn public_pricing_no_auth() {
    let env = setup().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("pub-{}", &suffix[..12]);
    okapi_store::provision::create_model_ratio(&env.pg, &model, "1.25", "4", "0.5")
        .await
        .unwrap();

    let body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/pricing", env.addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == model.as_str())
        .expect("新模型必须出现在公开价格页");
    assert_eq!(entry["model_ratio"], "1.250000");
    assert_eq!(entry["completion_ratio"], "4.000000");
    assert!(entry.get("api_base").is_none(), "不得泄漏渠道信息");
    assert!(body["groups"].is_array());
}

/// 用量日志：own 隔离 + snapshot 透出（账单解释器数据源）。
#[tokio::test]
async fn me_logs_with_snapshot_own_scope() {
    let env = setup().await;
    let (user_id, token) = mk_user(&env.pg).await;
    let (other_id, other_token) = mk_user(&env.pg).await;

    let request_id = Uuid::new_v4();
    sqlx::query!(
        r#"INSERT INTO billing_records
           (request_id, log_type, user_id, api_key_id, group_code, model_name, status,
            prompt_tokens, cached_tokens, completion_tokens, amount_micro,
            original_amount_micro, discount_micro, pricing_snapshot)
           VALUES ($1, 2, $2, 1, 'default', 'm-logs', 20, 100, 40, 20, 200, 240, 40,
                   '{"mode":"ratio","model_ratio":"1","group":"default","group_ratio":"1","user_multiplier":"1","rules":[{"code":"night","kind":"time","multiplier":"0.8"}]}')"#,
        request_id,
        user_id
    )
    .execute(&env.pg)
    .await
    .unwrap();
    // 另一个用户的记录（不得可见）
    sqlx::query!(
        r#"INSERT INTO billing_records
           (request_id, log_type, user_id, api_key_id, group_code, model_name, status,
            prompt_tokens, completion_tokens, amount_micro, original_amount_micro)
           VALUES ($1, 2, $2, 1, 'default', 'm-other', 20, 1, 1, 1, 1)"#,
        Uuid::new_v4(),
        other_id
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/me/logs", env.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1, "只见自己的记录");
    let row = &data[0];
    assert_eq!(row["model"], "m-logs");
    assert_eq!(row["amount_micro"], 200);
    assert_eq!(row["original_amount_micro"], 240);
    assert_eq!(row["discount_micro"], 40);
    assert_eq!(row["usage"]["cached_tokens"], 40);
    assert_eq!(row["pricing_snapshot"]["rules"][0]["code"], "night");
    assert_eq!(
        row["pricing_snapshot"]["rules"][0]["multiplier"], "0.8",
        "夜间折扣必须在快照里可解释（DESIGN §3 账单可解释性）"
    );

    let other_body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/me/logs", env.addr))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        other_body["data"].as_array().unwrap().len(),
        1,
        "对方同样只见自己"
    );
    assert_eq!(other_body["data"][0]["model"], "m-other");

    // 无鉴权 401
    let unauthorized = reqwest::Client::new()
        .get(format!("http://{}/api/me/logs", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    let _ = json!({});
}
