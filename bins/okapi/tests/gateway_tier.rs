//! service_tier 价格轴验收（DESIGN §3-4.5，Sub2API 0.1.179/180 对齐）：
//! flex 半价 + 快照档位 / 只降不升 / 未配置模型不受影响。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

/// mock 上游：响应报告的 service_tier 由请求 body 的 x_mock_tier 字段控制
/// （gateway 原样透传 body，自定义字段可达）。
async fn mock_chat(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap_or_default();
    let tier = req
        .get("x_mock_tier")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let mut resp = json!({
        "id":"cmpl","object":"chat.completion","model":"m",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":20,
                 "prompt_tokens_details":{"cached_tokens":0}}
    });
    if !tier.is_empty() {
        resp["service_tier"] = json!(tier);
    }
    axum::Json(resp).into_response()
}

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(tier_ratios: Option<Value>) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("tier-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-tier-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-tier")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    if let Some(ratios) = tier_ratios {
        sqlx::query!(
            r#"UPDATE model_pricing SET tier_ratios = $2
               WHERE model_id = (SELECT id FROM models WHERE model_name = $1)"#,
            model,
            ratios
        )
        .execute(&pg)
        .await
        .unwrap();
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    let router = Router::new().route("/v1/chat/completions", post(mock_chat));
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    okapi_store::provision::create_channel(
        &pg,
        &format!("tier-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        true,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(50_000_000))
        .await
        .unwrap();
    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        gateway: addr,
        token,
        user_id,
        model,
    }
}

/// 发起请求：requested_tier 进 body（service_tier），mock_tier 经透传头控制上游响应报告值。
async fn chat(env: &TestEnv, requested_tier: Option<&str>, mock_tier: Option<&str>) {
    let client = reqwest::Client::new();
    let mut body = json!({
        "model": env.model,
        "messages": [{"role": "user", "content": "hi"}]
    });
    if let Some(t) = requested_tier {
        body["service_tier"] = json!(t);
    }
    if let Some(t) = mock_tier {
        body["x_mock_tier"] = json!(t);
    }
    let resp = client
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();
}

async fn wait_committed(pg: &PgPool, user_id: i64) -> (i64, Option<Value>) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT amount_micro, pricing_snapshot FROM billing_records
               WHERE user_id = $1 AND status = 20"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.amount_micro, r.pricing_snapshot);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("未等到 committed 记录");
}

/// flex 半价：请求+响应皆 flex → 金额减半，快照记档位与倍率。
#[tokio::test]
async fn flex_halves_amount_and_snapshots() {
    let env = setup(Some(json!({"flex": "0.5", "priority": "2.0"}))).await;
    chat(&env, Some("flex"), Some("flex")).await;
    let (amount, snapshot) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(amount, 120, "1.0 基线 240 micro × flex 0.5 = 120");
    let snap = snapshot.expect("应有快照");
    assert_eq!(snap["service_tier"], "flex");
    // ratio 序列化为 JSON 数字（ser_decimal）
    assert!(
        (snap["tier_ratio"].as_f64().unwrap() - 0.5).abs() < 1e-9,
        "快照应记档位倍率：{snap}"
    );
}

/// 只降不升：请求 flex、上游报 priority → 仍按 flex 计。
#[tokio::test]
async fn tier_never_upgrades() {
    let env = setup(Some(json!({"flex": "0.5", "priority": "2.0"}))).await;
    chat(&env, Some("flex"), Some("priority")).await;
    let (amount, snapshot) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(amount, 120, "上游报更贵档位不得升价");
    assert_eq!(snapshot.unwrap()["service_tier"], "flex");
}

/// 上游降档：请求 priority、上游报 default（无 tier 字段）→ 按 default 1.0 计。
#[tokio::test]
async fn tier_downgrades_to_reported() {
    let env = setup(Some(json!({"flex": "0.5", "priority": "2.0"}))).await;
    chat(&env, Some("priority"), None).await;
    let (amount, snapshot) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(amount, 240, "上游未按 priority 跑：不为未享受的档位付费");
    let snap = snapshot.unwrap();
    assert!(snap.get("service_tier").is_none(), "default 档不记快照字段");
}

/// 未配置模型：请求带 flex 不受影响（原价、快照无档位字段）。
#[tokio::test]
async fn unconfigured_model_ignores_tier() {
    let env = setup(None).await;
    chat(&env, Some("flex"), Some("flex")).await;
    let (amount, snapshot) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(amount, 240);
    assert!(snapshot.unwrap().get("service_tier").is_none());
}
