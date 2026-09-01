//! §11.1 生态兼容功能验收：用户×模型 RPM 限流、new-api 兼容余额端点、
//! client_ip CDN 头采集落列。依赖 .env（scripts/dev-deps.sh up）。

use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":20}
    }))
    .into_response()
}

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(model_rpm: Option<i64>) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    if let Some(rpm) = model_rpm {
        // 全局键：以合并写入保住其他并行用例的键值
        sqlx::query!(
            r#"INSERT INTO settings (key, value) VALUES ('model_rpm_limits', $1)
               ON CONFLICT (key) DO UPDATE SET
                   value = settings.value || EXCLUDED.value"#,
            json!({ model.clone(): rpm })
        )
        .execute(&pg)
        .await
        .unwrap();
    }
    let user_id = okapi_store::provision::create_user(&pg, &format!("cp-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-cp-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &hash, "sk-okapi-cp")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock_app = axum::Router::new().route("/v1/chat/completions", post(mock_ok));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_app).await.unwrap();
    });
    okapi_store::provision::create_channel(
        &pg,
        &format!("cp-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
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

async fn chat(env: &TestEnv, extra_headers: &[(&str, &str)]) -> u16 {
    let mut req = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "max_tokens": 16,
            "messages": [{"role":"user","content": format!("q-{}", Uuid::new_v4())}]}));
    for (k, v) in extra_headers {
        req = req.header(*k, *v);
    }
    req.send().await.unwrap().status().as_u16()
}

/// 模型级 RPM=2：第三笔 429 rate_limited(model_rpm)。
#[tokio::test]
async fn per_model_rpm_limit() {
    let env = setup(Some(2)).await;
    assert_eq!(chat(&env, &[]).await, 200);
    assert_eq!(chat(&env, &[]).await, 200);
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "max_tokens": 16,
            "messages": [{"role":"user","content":"x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["error"]["param"], "model_rpm");
}

/// new-api 兼容余额端点：hard_limit = 余额+已用，usage 为美分。
#[tokio::test]
async fn dashboard_billing_compat() {
    let env = setup(None).await;
    // 一笔消费（240 micro）后：余额 9_999_760，used 240
    assert_eq!(chat(&env, &[]).await, 200);
    for _ in 0..50 {
        let used = sqlx::query_scalar!(
            r#"SELECT COALESCE(SUM(used_micro),0)::bigint AS "u!" FROM api_keys WHERE user_id = $1"#,
            env.user_id
        )
        .fetch_one(&env.pg)
        .await
        .unwrap();
        if used == 240 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let sub: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/v1/dashboard/billing/subscription",
            env.gateway
        ))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sub["object"], "billing_subscription");
    // 总额度 = 10_000_000 micro = $10（分粒度；展示层数值比较）
    assert!((sub["hard_limit_usd"].as_f64().unwrap() - 10.0).abs() < 1e-9);

    let usage: Value = reqwest::Client::new()
        .get(format!("http://{}/v1/dashboard/billing/usage", env.gateway))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // used 240 micro = $0.00024 → 美分 0.02（分粒度截断到 0.02）
    assert_eq!(usage["object"], "list");
    assert!((usage["total_usage"].as_f64().unwrap() - 0.02).abs() < 1e-9);
}

/// client_ip：CDN 头按序取首个有效值并落 outbox payload。
#[tokio::test]
async fn client_ip_from_cdn_headers() {
    let env = setup(None).await;
    assert_eq!(
        chat(&env, &[("x-forwarded-for", "203.0.113.9, 10.0.0.1")]).await,
        200
    );
    let mut ip = None;
    for _ in 0..50 {
        ip = sqlx::query_scalar!(
            r#"SELECT payload->>'client_ip' FROM billing_outbox
               WHERE payload->>'user_id' = $1 ORDER BY id DESC LIMIT 1"#,
            env.user_id.to_string()
        )
        .fetch_optional(&env.pg)
        .await
        .unwrap()
        .flatten();
        if ip.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(ip.as_deref(), Some("203.0.113.9"), "XFF 取首个有效 IP");
}
