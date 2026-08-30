//! M2 console 批次验收：权限点框架 / 定价发布热更闭环 / 渠道管理 / 入账对账。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    use std::fmt::Write as _;
    let chunks = [
        json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
        json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":0}}}),
    ];
    let mut body = String::new();
    for c in chunks {
        let _ = write!(body, "data: {c}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

struct Env {
    pg: PgPool,
    state: gateway::state::AppState,
    console: SocketAddr,
    gateway: SocketAddr,
    mock: SocketAddr,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('ssrf_policy', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        serde_json::json!({"allow_http": true, "allow_private": true})
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();

    let console_app = console::router(state.clone());
    let console_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = console_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(console_listener, console_app).await.unwrap();
    });

    let gw_app = gateway::router(state.clone());
    let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_addr = gw_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(gw_listener, gw_app).await.unwrap();
    });

    let mock_app = Router::new().route("/ok/v1/chat/completions", post(mock_ok));
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(mock_listener, mock_app).await.unwrap();
    });

    Env {
        pg,
        state,
        console: console_addr,
        gateway: gw_addr,
        mock: mock_addr,
    }
}

/// 建用户 + key，返回 (user_id, token)。
async fn mk_user(pg: &PgPool, role: i16, admin_role_id: Option<i64>) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(pg, &format!("c-{suffix}"))
        .await
        .unwrap();
    sqlx::query!(
        "UPDATE users SET role = $2, admin_role_id = $3 WHERE id = $1",
        user_id,
        role,
        admin_role_id
    )
    .execute(pg)
    .await
    .unwrap();
    let token = format!("sk-okapi-con-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(pg, user_id, &key_hash, "sk-okapi-con")
        .await
        .unwrap();
    (user_id, token)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn console_post(env: &Env, token: &str, path: &str, body: Value) -> reqwest::Response {
    client()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn console_get(env: &Env, token: &str, path: &str) -> reqwest::Response {
    client()
        .get(format!("http://{}{path}", env.console))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
}

/// 权限矩阵：普通用户全拒；自定义角色只放行集合内；默认 admin 全权；
/// 角色管理仅 super_admin（默认全权 admin 不可自我提权）。
#[tokio::test]
async fn permission_point_matrix() {
    let env = setup().await;
    let (_, super_token) = mk_user(&env.pg, 100, None).await;
    let (_, user_token) = mk_user(&env.pg, 1, None).await;
    let (_, full_admin_token) = mk_user(&env.pg, 10, None).await;

    // 普通用户：读也拒
    let r = console_get(&env, &user_token, "/admin/channels").await;
    assert_eq!(r.status(), 403);

    // super_admin 建只读角色
    let code = format!("readonly-{}", Uuid::new_v4().simple());
    let r = console_post(
        &env,
        &super_token,
        "/admin/roles",
        json!({"role_code": code, "display_name": "只读", "permissions": ["channel.read"]}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let role_id = r.json::<Value>().await.unwrap()["admin_role_id"]
        .as_i64()
        .unwrap();

    // 绑定只读角色的 admin：读行、写拒且 param 指出缺失权限点
    let (_, ro_token) = mk_user(&env.pg, 10, Some(role_id)).await;
    let r = console_get(&env, &ro_token, "/admin/channels").await;
    assert_eq!(r.status(), 200, "channel.read 应放行");
    let r = console_post(
        &env,
        &ro_token,
        "/admin/models",
        json!({"model_name": "x", "model_ratio": "1"}),
    )
    .await;
    assert_eq!(r.status(), 403);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["param"], "pricing.write");

    // 默认全权 admin：业务写放行，但角色管理被 super_admin 闸拦住（防自我提权）
    let r = console_get(&env, &full_admin_token, "/admin/channels").await;
    assert_eq!(r.status(), 200);
    let r = console_post(
        &env,
        &full_admin_token,
        "/admin/roles",
        json!({"role_code": "evil", "display_name": "x", "permissions": ["*"]}),
    )
    .await;
    assert_eq!(r.status(), 403, "默认全权 admin 不得管理角色");
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"]["param"], "super_admin_required");
}

/// 控制面端到端：建模型/渠道/入账 → 请求计费 → 改倍率+发布 → 热更后价格翻倍；
/// 渠道停用后 503；全程对账零差异；审计留痕。
#[tokio::test]
async fn pricing_publish_hot_reload_e2e() {
    let env = setup().await;
    let (super_id, super_token) = mk_user(&env.pg, 100, None).await;
    let (user_id, user_token) = mk_user(&env.pg, 1, None).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-con-{}", &suffix[..10]);

    // 建模型（ratio 1）与渠道
    let r = console_post(
        &env,
        &super_token,
        "/admin/models",
        json!({"model_name": model, "model_ratio": "1"}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let r = console_post(
        &env,
        &super_token,
        "/admin/channels",
        json!({
            "name": format!("con-{suffix}"),
            "api_base": format!("http://{}/ok/v1", env.mock),
            "credential": "mock",
            "models": [model],
        }),
    )
    .await;
    assert_eq!(r.status(), 200);
    let channel_id = r.json::<Value>().await.unwrap()["channel_id"]
        .as_i64()
        .unwrap();

    // 发布 epoch 并热更（模拟 30s 轮询）
    let r = console_post(&env, &super_token, "/admin/pricing/publish", json!({})).await;
    assert_eq!(r.status(), 200);
    gateway::refresh_pricebook_if_newer(&env.state)
        .await
        .unwrap();

    // 入账 $10
    let r = console_post(
        &env,
        &super_token,
        &format!("/admin/users/{user_id}/credit"),
        json!({"amount_micro": 10_000_000, "reason": "seed"}),
    )
    .await;
    assert_eq!(r.status(), 200);

    // 第一笔：ratio 1 → 240 micro
    let amount1 = chat_and_settle(&env, &user_token, &model).await;
    assert_eq!(amount1, 240);

    // 改倍率 2 → 发布 → 热更 → 价格翻倍
    let r = console_post(
        &env,
        &super_token,
        "/admin/models",
        json!({"model_name": model, "model_ratio": "2"}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let r = console_post(&env, &super_token, "/admin/pricing/publish", json!({})).await;
    assert_eq!(r.status(), 200);
    let swapped = gateway::refresh_pricebook_if_newer(&env.state)
        .await
        .unwrap();
    assert!(swapped, "发布后应热更");
    let amount2 = chat_and_settle(&env, &user_token, &model).await;
    assert_eq!(amount2, 480, "倍率 2 应精确翻倍");

    // 渠道停用 → 503
    let r = console_post(
        &env,
        &super_token,
        &format!("/admin/channels/{channel_id}/status"),
        json!({"status": 2}),
    )
    .await;
    assert_eq!(r.status(), 200);
    let resp = chat_raw(&env, &user_token, &model).await;
    assert_eq!(resp.status(), 503, "停用渠道后应无可用渠道");

    // 该用户对账零差异（入账双侧一致 + 消费双侧一致）
    let r = console_get(&env, &super_token, "/admin/reconciliation?limit=1000000").await;
    let body: Value = r.json().await.unwrap();
    let has_drift = body["drifts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["user_id"].as_i64() == Some(user_id));
    assert!(!has_drift, "console 入账 + 网关消费后必须对账干净");

    // 审计留痕
    let audits = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs WHERE actor = $1"#,
        format!("admin:{super_id}")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(audits >= 5, "写操作应全量审计，got {audits}");
}

async fn chat_raw(env: &Env, token: &str, model: &str) -> reqwest::Response {
    client()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(token)
        .json(&json!({
            "model": model, "stream": true, "max_tokens": 32,
            "messages": [{"role":"user","content":"hello console"}]
        }))
        .send()
        .await
        .unwrap()
}

async fn chat_and_settle(env: &Env, token: &str, model: &str) -> i64 {
    let resp = chat_raw(env, token, model).await;
    assert_eq!(resp.status(), 200);
    let request_id = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap();
    let _ = resp.text().await.unwrap();
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT amount_micro FROM billing_records WHERE request_id = $1"#,
            request_id
        )
        .fetch_optional(&env.pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return r.amount_micro;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("结算记录未出现");
}
