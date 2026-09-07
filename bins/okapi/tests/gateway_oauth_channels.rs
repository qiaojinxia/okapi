//! 自用订阅凭证（IMPLEMENTATION §11.38）：控制面 OAuth 登录建渠道 → 网关按 Bearer + 文档化头发请求
//! → 到期惰性刷新（四步锁、refresh 轮转回写、并发单飞）→ invalid_grant 进 invalid。
//! mock 同时扮演授权服务器（token 端点）与两家上游。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use okapi_store::credential::OAuthCredential;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

#[derive(Clone)]
struct Mock {
    token_calls: Arc<AtomicUsize>,
    /// 下一次刷新是否回 invalid_grant。
    reject_refresh: Arc<std::sync::atomic::AtomicBool>,
}

/// token 端点：换码回 access-1 / refresh-1；刷新回 access-N（N 为第几次调用）并轮转 refresh。
async fn mock_token(
    State(st): State<Mock>,
    headers: axum::http::HeaderMap,
    body: String,
) -> axum::response::Response {
    let n = st.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
    let ct = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    // Anthropic 与 Codex 刷新都是 JSON；Codex 换码是表单
    let grant = if ct.starts_with("application/json") {
        let v: Value = serde_json::from_str(&body).unwrap();
        v["grant_type"].as_str().unwrap_or_default().to_owned()
    } else {
        body.split('&')
            .find_map(|kv| kv.strip_prefix("grant_type="))
            .unwrap_or_default()
            .to_owned()
    };
    if grant == "refresh_token" && st.reject_refresh.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant", "error_description": "revoked"})),
        )
            .into_response();
    }
    // id_token 只在换码时给（Codex 从中取 account_id）
    let id_token = format!(
        "{}.{}.sig",
        base64url(br#"{"alg":"RS256"}"#),
        base64url(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-okapi"}}"#)
    );
    axum::Json(json!({
        "access_token": format!("access-{n}"),
        "refresh_token": format!("refresh-{n}"),
        "expires_in": 28800,
        "token_type": "Bearer",
        "id_token": id_token,
    }))
    .into_response()
}

fn base64url(input: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

/// Anthropic 订阅上游：断言 Bearer / oauth beta / 系统首句 / 无 x-api-key，回一条 Messages JSON。
async fn mock_messages(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        auth.starts_with("Bearer access-"),
        "订阅 token 走 Bearer：{auth}"
    );
    assert!(headers.get("x-api-key").is_none(), "不得再带 x-api-key");
    let beta = headers
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        beta.split(',').any(|b| b.trim() == "oauth-2025-04-20"),
        "beta 头：{beta}"
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        req["system"][0]["text"], "You are Claude Code, Anthropic's official CLI for Claude.",
        "系统首句须前置"
    );
    (
        [(
            "access-token-seen",
            auth.trim_start_matches("Bearer ").to_owned(),
        )],
        axum::Json(
            json!({"id":"msg_o","type":"message","role":"assistant","model":"claude-x",
            "content":[{"type":"text","text":"Hello max"}],"stop_reason":"end_turn",
            "usage":{"input_tokens":100,"output_tokens":50}}),
        ),
    )
        .into_response()
}

/// Codex 订阅上游：断言 chatgpt-account-id / originator / store=false，回 Responses 对象。
async fn mock_codex_responses(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(auth.starts_with("Bearer access-"));
    assert_eq!(
        headers
            .get("chatgpt-account-id")
            .and_then(|v| v.to_str().ok()),
        Some("acct-okapi"),
        "account id 来自 id_token claim"
    );
    assert_eq!(
        headers.get("originator").and_then(|v| v.to_str().ok()),
        Some("codex_cli_rs")
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["store"], false, "Codex 后端不持久化：store 强制 false");
    axum::Json(json!({
        "id": "resp_o", "object": "response", "status": "completed", "model": "gpt-5",
        "output": [{"type": "message", "id": "m1", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "Hello codex", "annotations": []}]}],
        "usage": {"input_tokens": 100, "output_tokens": 50, "total_tokens": 150}
    }))
    .into_response()
}

async fn spawn_mock(mock: Mock) -> SocketAddr {
    let router = Router::new()
        .route("/token", post(mock_token))
        .route("/v1/messages", post(mock_messages))
        .route("/codex/responses", post(mock_codex_responses))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Env {
    pg: PgPool,
    state: gateway::state::AppState,
    gateway: SocketAddr,
    console: SocketAddr,
    mock: SocketAddr,
    mock_state: Mock,
    admin_token: String,
    user_token: String,
    user_id: i64,
    model: String,
}

// 三个服务 + 两个用户的装配放同一视野
#[allow(clippy::too_many_lines)]
async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    // 测试 mock 在 127.0.0.1：token_url 覆写要过 SSRF 闸，放开私网
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('ssrf_policy', '{"allow_http": true, "allow_private": true}')
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#
    )
    .execute(&pg)
    .await
    .unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-oa-{}", &suffix[..10]);
    let admin_id = okapi_store::provision::create_user(&pg, &format!("oa-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-oa-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-oa-adm")
        .await
        .unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("oa-u-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-oa-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-oa-u")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "500", "2", "0.5")
        .await
        .unwrap();

    let mock_state = Mock {
        token_calls: Arc::new(AtomicUsize::new(0)),
        reject_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let mock = spawn_mock(mock_state.clone()).await;

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let gw = gateway::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            gw.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let cs = console::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, cs).await.unwrap();
    });
    Env {
        pg,
        state,
        gateway: gateway_addr,
        console: console_addr,
        mock,
        mock_state,
        admin_token,
        user_token,
        user_id,
        model,
    }
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// 走控制面两步登录建渠道；返回 (channel_id, channel_key_id)。
async fn login_channel(env: &Env, provider: &str) -> (i64, i64) {
    let client = reqwest::Client::new();
    let started: Value = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"provider": provider}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let authorize = started["authorize_url"].as_str().unwrap();
    let state = started["state"].as_str().unwrap();
    // 授权 URL 形状：PKCE + 各家 client_id
    let url = reqwest::Url::parse(authorize).unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["state"], state);
    // 站长"贴回"的 code：Anthropic 形态 code#state / Codex 形态整个回调 URL
    let pasted = if provider == "anthropic_max" {
        format!("auth-code-1#{state}")
    } else {
        format!("http://localhost:1455/auth/callback?code=auth-code-1&state={state}")
    };
    let resp = client
        .post(format!(
            "http://{}/admin/channels/oauth/exchange",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .json(&json!({
            "state": state, "code": pasted,
            "name": format!("{provider}-{}", Uuid::new_v4().simple()),
            "models": [env.model],
            "token_url": format!("http://{}/token", env.mock),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    let channel_id = body["channel_id"].as_i64().unwrap();
    let key_id = body["channel_key_id"].as_i64().unwrap();
    // 上游地址指向 mock（登录默认写官方地址）
    let base = if provider == "anthropic_max" {
        format!("http://{}/v1", env.mock)
    } else {
        format!("http://{}/codex", env.mock)
    };
    sqlx::query!(
        r#"UPDATE channels SET api_base = $2 WHERE id = $1"#,
        channel_id,
        base
    )
    .execute(&env.pg)
    .await
    .unwrap();
    env.state.invalidate_routing_caches();
    (channel_id, key_id)
}

async fn read_cred(env: &Env, key_id: i64) -> OAuthCredential {
    let plain =
        okapi_store::admin::read_key_credential(&env.pg, key_id, env.state.master_key.as_deref())
            .await
            .unwrap()
            .unwrap();
    OAuthCredential::parse(&plain).expect("OAuth 形态凭证")
}

/// 把凭证到期时间改到过去，让下一请求触发刷新。
async fn expire_cred(env: &Env, key_id: i64) {
    let mut cred = read_cred(env, key_id).await;
    cred.expires_at = chrono::Utc::now().timestamp() - 10;
    okapi_store::admin::write_key_credential(
        &env.pg,
        key_id,
        &cred.to_plaintext(),
        env.state.master_key.as_deref(),
    )
    .await
    .unwrap();
    env.state.invalidate_routing_caches();
}

async fn chat(env: &Env) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({"model": env.model, "max_tokens": 64,
            "messages": [{"role": "user", "content": format!("q-{}", Uuid::new_v4())}]}))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn anthropic_max_login_request_refresh_and_invalidate() {
    let env = setup().await;
    let (_channel_id, key_id) = login_channel(&env, "anthropic_max").await;
    let cred = read_cred(&env, key_id).await;
    assert_eq!(cred.access_token, "access-1");
    assert_eq!(cred.refresh_token, "refresh-1");
    let kind = sqlx::query_scalar!(
        r#"SELECT credential_kind FROM channel_keys WHERE id = $1"#,
        key_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(kind, 1, "credential_kind = oauth_refresh");
    assert_eq!(
        env.mock_state.token_calls.load(Ordering::SeqCst),
        1,
        "换码一次"
    );

    // 请求：OpenAI 入口 → anthropic_max（mock 断言 Bearer / beta / 系统首句）
    let resp = chat(&env).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(
        resp.headers()
            .get("access-token-seen")
            .map(|v| v.to_str().unwrap()),
        None,
        "上游私有头不透出给客户端"
    );
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "Hello max");
    assert_eq!(
        env.mock_state.token_calls.load(Ordering::SeqCst),
        1,
        "未到期不刷新"
    );

    // 到期 → 并发两请求只刷一次，refresh 轮转回写
    expire_cred(&env, key_id).await;
    let (a, b) = tokio::join!(chat(&env), chat(&env));
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    assert_eq!(
        env.mock_state.token_calls.load(Ordering::SeqCst),
        2,
        "并发只刷一次（单飞 + Redis 锁）"
    );
    let cred = read_cred(&env, key_id).await;
    assert_eq!(cred.access_token, "access-2");
    assert_eq!(cred.refresh_token, "refresh-2", "refresh 轮转须回写");
    assert!(cred.expires_at > chrono::Utc::now().timestamp() + 3600);

    // Anthropic 入口透传同样可用
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .header("x-api-key", &env.user_token)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({"model": env.model, "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    // 刷新被拒（invalid_grant）→ key 进 invalid(6)，请求无候选
    env.mock_state.reject_refresh.store(true, Ordering::SeqCst);
    expire_cred(&env, key_id).await;
    let resp = chat(&env).await;
    assert_eq!(resp.status(), 502, "{}", resp.text().await.unwrap());
    let status = sqlx::query_scalar!(r#"SELECT status FROM channel_keys WHERE id = $1"#, key_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 6, "refresh token 失效 → invalid，仅人工重登可恢复");

    // 计费：三笔成功请求各 (100×1 + 50×2) × 500 × 2 = 200_000
    let amounts: Vec<i64> = sqlx::query_scalar!(
        r#"SELECT amount_micro FROM billing_records WHERE user_id = $1 AND log_type = 2 AND status = 20"#,
        env.user_id
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(amounts.len(), 4);
    assert!(amounts.iter().all(|a| *a == 200_000));
}

#[tokio::test]
async fn codex_login_routes_only_responses_ingress() {
    let env = setup().await;
    let (_channel_id, key_id) = login_channel(&env, "codex").await;
    let cred = read_cred(&env, key_id).await;
    assert_eq!(
        cred.account_id.as_deref(),
        Some("acct-okapi"),
        "account_id 取自 id_token claim"
    );

    // /v1/responses 入口 → Codex 后端（mock 断言 account id / originator / store=false）
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/responses", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({"model": env.model, "input": "hi", "store": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello codex");

    // chat 入口不路由 codex 渠道：该模型只有 codex 候选 → 503 无可用渠道
    let resp = chat(&env).await;
    assert_eq!(resp.status(), 503);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "no_available_channel");

    // embeddings 同样不路由
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({"model": env.model, "input": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 503);
}

/// 用过的 state 不能二次兑换；非 OAuth 协议 400。
#[tokio::test]
async fn oauth_state_is_single_use_and_provider_checked() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"provider": "openai"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["param"], "provider");

    login_channel(&env, "anthropic_max").await;
    let resp = client
        .post(format!(
            "http://{}/admin/channels/oauth/exchange",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .json(&json!({"state": "never-issued", "code": "x", "name": "n", "models": [env.model]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "oauth_state_invalid");
}
