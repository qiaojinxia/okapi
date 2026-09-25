//! M3 custom_pass 验收（§4.4 #1454）：任意路径透传、路径白名单、
//! per_call 预扣/结算、上游失败退款。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_tool(headers: axum::http::HeaderMap) -> axum::response::Response {
    assert_eq!(
        headers.get("x-api-key").and_then(|v| v.to_str().ok()),
        Some("mock-credential"),
        "凭证头按渠道 settings 注入"
    );
    axum::Json(json!({"ok": true, "tool": "result"})).into_response()
}

/// 回显上游收到的一切：方法、查询串、Content-Type、请求体，以及是否带了客户端的 Authorization。
async fn mock_echo(
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    axum::Json(json!({
        "method": method.as_str(),
        "query": uri.query().unwrap_or(""),
        "content_type": h("content-type"),
        "x_api_key": h("x-api-key"),
        "authorization": h("authorization"),
        "body": String::from_utf8_lossy(&body),
    }))
    .into_response()
}

async fn mock_boom() -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"error": "boom"})),
    )
        .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/tool", post(mock_tool).get(mock_tool))
        .route("/ok/echo", post(mock_echo))
        .route("/ok/boom", get(mock_boom));
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
    channel_id: i64,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    let suffix = Uuid::new_v4().simple().to_string();
    let billing_model = format!("pass-{}", &suffix[..12]);

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
    // per_call 计费模型：$0.005/次 = 5000 micro
    let model_id = sqlx::query_scalar!(
        r#"INSERT INTO models (model_name) VALUES ($1) RETURNING id"#,
        billing_model
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    sqlx::query!(
        r#"INSERT INTO model_pricing (model_id, pricing_mode, per_call_price_micro)
           VALUES ($1, 'per_call', 5000)"#,
        model_id
    )
    .execute(&pg)
    .await
    .unwrap();

    let mock = spawn_mock().await;
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("pass-{suffix}"),
        "custom_pass",
        &format!("http://{mock}"),
        "mock-credential",
        &[],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query!(
        "UPDATE channels SET settings = $2 WHERE id = $1",
        channel_id,
        json!({
            "allowed_paths": ["/ok"],
            "billing_model": billing_model,
            "auth_header": "x-api-key",
            "auth_scheme": "",
        })
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_000_000))
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
        channel_id,
    }
}

async fn pass_get(env: &TestEnv, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "http://{}/pass/{}/{}",
            env.gateway, env.channel_id, path
        ))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap()
}

async fn record_of(pg: &PgPool, user_id: i64, log_type: i16) -> Option<(i16, i64)> {
    sqlx::query!(
        r#"SELECT status, amount_micro FROM billing_records
           WHERE user_id = $1 AND log_type = $2"#,
        user_id,
        log_type
    )
    .fetch_optional(pg)
    .await
    .unwrap()
    .map(|r| (r.status, r.amount_micro))
}

/// 透传成功：响应原样 + per_call 计费 + 余额扣减。
#[tokio::test]
async fn pass_through_bills_per_call() {
    let env = setup().await;
    let resp = pass_get(&env, "ok/tool").await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body, json!({"ok": true, "tool": "result"}), "响应必须原样");

    for _ in 0..50 {
        if record_of(&env.pg, env.user_id, 2).await.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (status, amount) = record_of(&env.pg, env.user_id, 2).await.expect("必须记账");
    assert_eq!(status, 20);
    assert_eq!(amount, 5000, "per_call $0.005");
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 1_000_000 - 5000);
}

/// 白名单外路径：403 且分文未动。
#[tokio::test]
async fn path_outside_allowlist_rejected_free() {
    let env = setup().await;
    let resp = pass_get(&env, "deny/secret").await;
    assert_eq!(resp.status(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "permission_denied");
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 1_000_000, "白名单拒绝不得计费");
}

/// 上游 5xx：错误透传 + 全额退款 + 失败记账。
#[tokio::test]
async fn upstream_failure_refunds() {
    let env = setup().await;
    let resp = pass_get(&env, "ok/boom").await;
    assert_eq!(resp.status(), 500, "上游错误状态原样");

    for _ in 0..50 {
        if record_of(&env.pg, env.user_id, 5).await.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (status, amount) = record_of(&env.pg, env.user_id, 5)
        .await
        .expect("失败必须留痕");
    assert_eq!(status, 40, "failed");
    assert_eq!(amount, 0);
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 1_000_000, "失败必须全额退款");
}

/// POST 透传：方法、查询串、Content-Type、请求体原样到上游，上游响应原样回客户端，照常按次计费；
/// 客户端的 Okapi token 不得随请求转发给上游（上游凭证由渠道配置另行注入）。
///
/// 路由是 `any(...)`，但此前的用例全走 GET——逐接口端到端探针按 POST 探时全绿：
/// 请求体转发从没被任何用例验过，而调各类 API 时 POST 才是常态。
#[tokio::test]
async fn post_forwards_method_query_and_body_verbatim() {
    let env = setup().await;
    let payload = r#"{"q":"weather","n":2}"#;
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/pass/{}/ok/echo?lang=zh&page=2",
            env.gateway, env.channel_id
        ))
        .bearer_auth(&env.token)
        .header("content-type", "application/json")
        .body(payload)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let echo: Value = resp.json().await.unwrap();
    assert_eq!(echo["method"], "POST");
    assert_eq!(echo["query"], "lang=zh&page=2", "查询串必须原样转发");
    assert_eq!(echo["content_type"], "application/json");
    assert_eq!(echo["body"], payload, "请求体必须逐字节原样转发");
    assert_eq!(
        echo["x_api_key"], "mock-credential",
        "上游凭证按渠道 settings 注入"
    );
    assert!(
        !echo["authorization"]
            .as_str()
            .is_some_and(|a| a.contains(&env.token)),
        "客户端的 Okapi token 泄露给了上游：{echo}"
    );

    for _ in 0..50 {
        if record_of(&env.pg, env.user_id, 2).await.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (status, amount) = record_of(&env.pg, env.user_id, 2).await.expect("必须记账");
    assert_eq!(status, 20);
    assert_eq!(amount, 5000, "per_call $0.005");
}
