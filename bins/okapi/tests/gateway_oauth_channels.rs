//! 自用订阅凭证（IMPLEMENTATION §11.38）：控制面 OAuth 登录建渠道 → 网关按 Bearer + 文档化头发请求
//! → 到期惰性刷新（四步锁、refresh 轮转回写、并发单飞）→ invalid_grant 进 invalid。
//! mock 同时扮演授权服务器（token 端点）与两家上游。依赖 .env（scripts/dev-deps.sh up）。

#[path = "support/channel_creation.rs"]
mod channel_creation;
#[path = "support/client_profiles.rs"]
mod client_profiles;
#[path = "support/oauth_maintenance.rs"]
mod oauth_maintenance;
#[path = "support/programming_clients.rs"]
mod programming_clients;
#[path = "support/published_pricing.rs"]
mod published_pricing;
#[path = "support/token_imports.rs"]
mod token_imports;

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
    cli_mode: Arc<std::sync::atomic::AtomicBool>,
    token_calls: Arc<AtomicUsize>,
    codex_calls: Arc<AtomicUsize>,
    codex_not_found: Arc<std::sync::atomic::AtomicBool>,
    delay_refresh: Arc<std::sync::atomic::AtomicBool>,
    delay_second_exchange: Arc<std::sync::atomic::AtomicBool>,
    refresh_started: Arc<tokio::sync::Notify>,
    refresh_continue: Arc<tokio::sync::Notify>,
    account_id: Arc<std::sync::Mutex<String>>,
    api_rejection: Arc<AtomicUsize>,
    refresh_unavailable: Arc<std::sync::atomic::AtomicBool>,
    /// 下一次刷新是否回 invalid_grant。
    reject_refresh: Arc<std::sync::atomic::AtomicBool>,
    /// 上游最近一次收到的请求头（断言客户端身份头透传）。
    last_headers: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    /// Anthropic 上游最近一次收到的请求体（断言 mimic 的 system / metadata 重写）。
    last_body: Arc<std::sync::Mutex<Option<Value>>>,
}

impl Mock {
    fn record(&self, headers: &axum::http::HeaderMap) {
        *self.last_headers.lock().unwrap() = headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_owned(), v.to_owned()))
            })
            .collect();
    }

    fn seen(&self, name: &str) -> Option<String> {
        self.last_headers
            .lock()
            .unwrap()
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    fn remember_body(&self, body: &[u8]) {
        *self.last_body.lock().unwrap() = serde_json::from_slice(body).ok();
    }

    fn last_body(&self) -> Option<Value> {
        self.last_body.lock().unwrap().clone()
    }
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
    // Anthropic 与 Codex 刷新都是 JSON；Codex 换码是表单。client_id 分流两家：
    // Codex（client_id app_*）回 id_token；Anthropic 回 account.uuid —— 各自的真实形状。
    let (grant, client_id) = if ct.starts_with("application/json") {
        let v: Value = serde_json::from_str(&body).unwrap();
        (
            v["grant_type"].as_str().unwrap_or_default().to_owned(),
            v["client_id"].as_str().unwrap_or_default().to_owned(),
        )
    } else {
        let field = |name: &str| {
            body.split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
                .unwrap_or_default()
                .to_owned()
        };
        (field("grant_type"), field("client_id"))
    };
    if (grant == "refresh_token" && st.delay_refresh.load(Ordering::SeqCst))
        || (grant == "authorization_code"
            && n == 2
            && st.delay_second_exchange.load(Ordering::SeqCst))
    {
        st.refresh_started.notify_one();
        st.refresh_continue.notified().await;
    }
    if grant == "refresh_token" && st.refresh_unavailable.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error":"unavailable"})),
        )
            .into_response();
    }
    if grant == "refresh_token" && st.reject_refresh.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant", "error_description": "revoked"})),
        )
            .into_response();
    }
    let mut resp = json!({
        "access_token": format!("access-{n}"),
        "refresh_token": format!("refresh-{n}"),
        "expires_in": 28800,
        "token_type": "Bearer",
    });
    if client_id.starts_with("app_") {
        // Codex：id_token 只在换码时给（从中取 account_id）
        resp["id_token"] = json!(format!(
            "{}.{}.sig",
            base64url(br#"{"alg":"RS256"}"#),
            base64url(json!({"email":"codex@example.com","https://api.openai.com/auth":{"chatgpt_account_id":st.account_id.lock().unwrap().clone()}}).to_string().as_bytes())
        ));
    } else {
        resp["account"] = json!({"uuid": "11111111-2222-4333-8444-555555555555", "email_address": "max@example.com"});
    }
    axum::Json(resp).into_response()
}

fn base64url(input: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

/// Anthropic 订阅上游：断言 `?beta=true` / Bearer / 必备 beta / 系统首句 / 无 x-api-key，回一条 Messages JSON。
async fn mock_messages(
    State(st): State<Mock>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    st.record(&headers);
    st.remember_body(&body);
    assert_eq!(
        query.as_deref(),
        Some("beta=true"),
        "订阅路径 URL 带 ?beta=true"
    );
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if st.api_rejection.load(Ordering::SeqCst) == 2
        || (st.api_rejection.load(Ordering::SeqCst) == 1 && auth == "Bearer access-1")
    {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error":{"type":"authentication_error"}})),
        )
            .into_response();
    }
    assert!(
        auth.starts_with("Bearer access-"),
        "订阅 token 走 Bearer：{auth}"
    );
    assert!(headers.get("x-api-key").is_none(), "不得再带 x-api-key");
    assert_eq!(
        headers.get_all("anthropic-beta").iter().count(),
        1,
        "anthropic-beta 只能有一行（客户端的已合并进来）"
    );
    let beta = headers
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let betas: Vec<&str> = beta.split(',').map(str::trim).collect();
    for required in ["oauth-2025-04-20", "claude-code-20250219"] {
        assert!(betas.contains(&required), "beta 头缺 {required}：{beta}");
    }
    let req: Value = serde_json::from_slice(&body).unwrap();
    let identity_index =
        usize::from(req["system"][0]["text"].as_str().is_some_and(|text| {
            text.starts_with("x-anthropic-billing-header: cc_version=2.1.290.")
        }));
    assert_eq!(
        req["system"][identity_index]["text"],
        "You are Claude Code, Anthropic's official CLI for Claude.",
        "新版 billing 在 identity 前，旧版 identity 仍在首位"
    );
    if st.cli_mode.load(Ordering::SeqCst) && req["stream"] == true {
        return programming_clients::messages_stream(&req);
    }
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

/// Codex 订阅上游：断言 chatgpt-account-id / accept / 请求体整形（store=false、stream=true、
/// instructions 存在、system→developer、previous_response_id 保留），只回 SSE（该后端只有流式面）。
async fn mock_codex_responses(
    State(st): State<Mock>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    st.codex_calls.fetch_add(1, Ordering::SeqCst);
    st.record(&headers);
    if st.codex_not_found.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(
                json!({"error":{"type":"not_found_error","message":"no Responses endpoint"}}),
            ),
        )
            .into_response();
    }
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
        headers.get("accept").and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    if st.cli_mode.load(Ordering::SeqCst) {
        return programming_clients::responses_stream(&req);
    }
    assert_eq!(req["store"], false, "Codex 后端不持久化：store 强制 false");
    assert_eq!(req["stream"], true, "Codex 后端只有流式面");
    assert!(req["instructions"].is_string(), "instructions 键必须存在");
    if let Some(previous) = req.get("previous_response_id") {
        assert_eq!(previous, "resp_prev", "续聊 ID 不能静默丢失");
    }
    assert!(
        req["input"]
            .as_array()
            .is_none_or(|items| items.iter().all(|i| i["role"] != "system")),
        "system 角色改 developer"
    );
    let sse = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_o\",\"status\":\"in_progress\"}}\n\n",
        "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello codex\"}\n\n",
        "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello codex\",\"annotations\":[]}]}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_o\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"gpt-5\",\"output\":[],\"usage\":{\"input_tokens\":100,\"output_tokens\":50,\"total_tokens\":150}}}\n\n",
    );
    ([("content-type", "text/event-stream")], sse).into_response()
}

async fn mock_codex_socket(
    State(st): State<Mock>,
    headers: axum::http::HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> axum::response::Response {
    st.record(&headers);
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if st.api_rejection.load(Ordering::SeqCst) == 2
        || (st.api_rejection.load(Ordering::SeqCst) == 1 && auth == "Bearer access-1")
    {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    upgrade.on_upgrade(|mut socket| async move {
        while let Some(Ok(axum::extract::ws::Message::Text(_))) = socket.recv().await {
            let value = json!({"type":"response.completed","response":{"id":format!("resp_{}",Uuid::new_v4().simple()),
                "status":"completed", "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],
                "usage":{"input_tokens":100,"output_tokens":50}}});
            if socket.send(axum::extract::ws::Message::Text(value.to_string().into())).await.is_err() { break; }
        }
    }).into_response()
}

async fn spawn_mock(mock: Mock) -> SocketAddr {
    let router = Router::new()
        .route("/token", post(mock_token))
        .route("/v1/messages", post(mock_messages))
        .route(
            "/codex/responses",
            post(mock_codex_responses).get(mock_codex_socket),
        )
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
    okapi_store::test_support::assert_isolated();
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
        cli_mode: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        token_calls: Arc::new(AtomicUsize::new(0)),
        codex_calls: Arc::new(AtomicUsize::new(0)),
        codex_not_found: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        delay_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        delay_second_exchange: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        refresh_started: Arc::default(),
        refresh_continue: Arc::default(),
        account_id: Arc::new(std::sync::Mutex::new("acct-okapi".to_owned())),
        api_rejection: Arc::new(AtomicUsize::new(0)),
        refresh_unavailable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        reject_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        last_headers: Arc::default(),
        last_body: Arc::default(),
    };
    let mock = spawn_mock(mock_state.clone()).await;

    published_pricing::publish(&pg, admin_id).await;
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
// 登录 → 请求 → 刷新 → 作废是同一把凭证的生命周期，前一步的状态就是后一步的前提，拆开要逐段重建
#[allow(clippy::too_many_lines)]
async fn anthropic_max_login_request_refresh_and_invalidate() {
    let env = setup().await;
    let (_channel_id, key_id) = login_channel(&env, "anthropic_max").await;
    let cred = read_cred(&env, key_id).await;
    assert_eq!(cred.access_token, "access-1");
    assert_eq!(cred.refresh_token, "refresh-1");
    assert_eq!(
        cred.account_label.as_deref(),
        Some("max@example.com"),
        "换码响应的 account.email_address"
    );
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

    // Anthropic 入口透传同样可用；真实 Claude Code 客户端的身份头原样到上游，
    // 它自带的 beta 与必备项合并，鉴权头不受客户端影响
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", env.gateway))
        .header("x-api-key", &env.user_token)
        .header("anthropic-version", "2023-06-01")
        .header("user-agent", "claude-cli/9.9.9 (external, cli)")
        .header("x-app", "cli")
        .header("x-stainless-lang", "js")
        .header("anthropic-beta", "context-1m-2025-08-07")
        .json(&json!({"model": env.model, "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(
        env.mock_state.seen("user-agent").as_deref(),
        Some("claude-cli/9.9.9 (external, cli)"),
        "客户端 UA 原样透传"
    );
    assert_eq!(env.mock_state.seen("x-app").as_deref(), Some("cli"));
    assert_eq!(
        env.mock_state.seen("x-stainless-lang").as_deref(),
        Some("js")
    );
    let beta = env.mock_state.seen("anthropic-beta").unwrap();
    assert!(
        beta.contains("context-1m-2025-08-07") && beta.contains("oauth-2025-04-20"),
        "客户端 beta 与必备项合并：{beta}"
    );
    assert!(
        env.mock_state.seen("x-api-key").is_none(),
        "客户端给网关的 x-api-key（用户 token）绝不能透传到上游"
    );

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

async fn seed_codex_previous_response(env: &Env) {
    let api_key_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE key_hash=$1")
        .bind(hash(&env.user_token))
        .fetch_one(&env.pg)
        .await
        .unwrap();
    let candidates = okapi_store::channels::candidates_for_model(
        &env.pg,
        &env.model,
        &["default"],
        env.state.master_key.as_deref(),
    )
    .await
    .unwrap();
    let binding =
        gateway::sched_redis::response_affinity::ResponseBinding::from_candidate(&candidates[0]);
    env.state
        .sched
        .response_binding_set(env.user_id, api_key_id, "resp_prev", &binding)
        .await
        .unwrap();
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
    assert_eq!(
        cred.account_label.as_deref(),
        Some("codex@example.com"),
        "账号邮箱取自 id_token 的 email claim，供控制台展示"
    );

    seed_codex_previous_response(&env).await;

    // /v1/responses 非流式入口 → Codex 后端（mock 断言请求体整形，只回 SSE）→ 网关聚合回 JSON
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/responses", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({"model": env.model, "store": true, "previous_response_id": "resp_prev",
            "input": [{"role": "system", "content": "be terse"}, {"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["output"][0]["content"][0]["text"], "Hello codex",
        "终态事件 output 为空时用 output_item.done 拼回：{body}"
    );
    assert_eq!(body["usage"]["input_tokens"], 100);
    assert_eq!(
        env.mock_state.seen("originator").as_deref(),
        Some("codex_cli_rs"),
        "客户端没带 originator 时用缺省值"
    );

    // 真实 Codex CLI 的身份头透传；流式请求原样透出 SSE
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/responses", env.gateway))
        .bearer_auth(&env.user_token)
        .header("originator", "codex_vscode")
        .header("session_id", "sess-1")
        .header("user-agent", "codex_vscode/1.2.3 (Mac OS 15; arm64)")
        .json(&json!({"model": env.model, "input": "hi", "stream": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"))
    );
    let text = resp.text().await.unwrap();
    assert!(text.contains("response.completed"), "{text}");
    assert_eq!(
        env.mock_state.seen("originator").as_deref(),
        Some("codex_vscode")
    );
    assert_eq!(env.mock_state.seen("session_id").as_deref(), Some("sess-1"));
    assert_eq!(
        env.mock_state.seen("user-agent").as_deref(),
        Some("codex_vscode/1.2.3 (Mac OS 15; arm64)")
    );

    // 计费：两笔成功请求都从 usage 结算（非流式那笔的 usage 来自聚合出的终态事件）；
    // 流式结算在流结束后异步落库，轮询等待
    let mut settled = 0;
    for _ in 0..50 {
        settled = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM billing_records WHERE user_id = $1 AND log_type = 2 AND status = 20"#,
            env.user_id
        )
        .fetch_one(&env.pg)
        .await
        .unwrap();
        if settled == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(settled, 2, "两笔都应按 usage 结算");

    // chat 入口不路由 codex 渠道：明确告诉调用方改用 Responses，而非让它重试 503。
    let resp = chat(&env).await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unsupported_endpoint");
    assert!(
        body["error"]["param"]
            .as_str()
            .unwrap()
            .split(',')
            .any(|p| p == "/v1/responses")
    );
    assert!(body["error"]["request_id"].as_str().is_some());

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

#[tokio::test]
async fn codex_404_does_not_replay_or_fallback_to_chat() {
    let env = setup().await;
    login_channel(&env, "codex").await;
    env.mock_state.codex_not_found.store(true, Ordering::SeqCst);
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/responses", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({"model":env.model,"input":"hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404, "{}", response.text().await.unwrap());
    assert_eq!(env.mock_state.codex_calls.load(Ordering::SeqCst), 1);
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        10_000_000,
        "an unsupported endpoint must refund the hold"
    );
    assert!(
        env.state
            .ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty()
    );
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

/// token 端点（可被 `oauth_token_url` 覆写）不跟随重定向：两家的换码 / 刷新拿到 302 就按上游
/// 错误处理，重定向目标零请求——否则 SSRF 闸校验过的公网地址一跳就能把 refresh token 送进私网。
#[tokio::test]
async fn token_endpoint_redirect_is_not_followed() {
    let leaked = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route(
            "/token",
            post(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [(axum::http::header::LOCATION, "/leak")],
                )
            }),
        )
        .route(
            "/leak",
            post({
                let leaked = Arc::clone(&leaked);
                move || {
                    let leaked = Arc::clone(&leaked);
                    async move {
                        leaked.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({"access_token": "leaked", "expires_in": 3600}))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let http = okapi_providers::HttpPool::new().unwrap();
    let token_url = format!("http://{mock}/token");

    let is_302 = |r: Result<okapi_providers::oauth::Tokens, okapi_providers::UpstreamError>| {
        matches!(
            r,
            Err(okapi_providers::UpstreamError::Status { status: 302, .. })
        )
    };
    assert!(is_302(
        okapi_providers::oauth::anthropic_max::refresh(&http, &token_url, "rt", None).await
    ));
    assert!(is_302(
        okapi_providers::oauth::anthropic_max::exchange(&http, &token_url, "code", "v", None).await
    ));
    assert!(is_302(
        okapi_providers::oauth::codex::refresh(&http, &token_url, "rt", None).await
    ));
    assert!(is_302(
        okapi_providers::oauth::codex::exchange(&http, &token_url, "code", "v", None).await
    ));
    assert_eq!(leaked.load(Ordering::SeqCst), 0, "重定向目标不得被请求");
}

/// 最小 HTTP 正向代理：把绝对 URI 原样转发给目标，记下经手的每个 URI。
async fn spawn_recording_proxy(seen: Arc<std::sync::Mutex<Vec<String>>>) -> SocketAddr {
    let router = Router::new().fallback(move |req: axum::extract::Request| {
        let seen = Arc::clone(&seen);
        async move {
            let uri = req.uri().to_string();
            seen.lock().unwrap().push(uri.clone());
            let method = req.method().clone();
            let headers = req.headers().clone();
            let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                .await
                .unwrap_or_default();
            let mut b = reqwest::Client::new().request(method, uri);
            for (k, v) in &headers {
                if k == axum::http::header::HOST || k == axum::http::header::CONNECTION {
                    continue;
                }
                b = b.header(k, v);
            }
            match b.body(body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let ct = resp
                        .headers()
                        .get(axum::http::header::CONTENT_TYPE)
                        .cloned();
                    let bytes = resp.bytes().await.unwrap_or_default();
                    let mut out = axum::response::Response::new(axum::body::Body::from(bytes));
                    *out.status_mut() = status;
                    if let Some(ct) = ct {
                        out.headers_mut()
                            .insert(axum::http::header::CONTENT_TYPE, ct);
                    }
                    out
                }
                Err(_) => axum::http::StatusCode::BAD_GATEWAY.into_response(),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// 渠道绑了出口代理（§11.41）时，token 刷新与重新授权的换码都得和 API 请求一样走这个代理：
/// 订阅账号对出口 IP 敏感，只有代理能出网的部署也才刷得动。
#[tokio::test]
async fn token_refresh_and_reauthorization_use_channel_proxy() {
    let env = setup().await;
    let (channel_id, key_id) = login_channel(&env, "anthropic_max").await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let proxy = spawn_recording_proxy(Arc::clone(&seen)).await;
    let url = format!("http://{proxy}");
    let endpoint = okapi_providers::http::ProxyEndpoint::parse(&url).unwrap();
    let proxy_id = okapi_store::egress::create_proxy(
        &env.pg,
        &okapi_store::egress::NewProxy {
            name: "oauth-refresh-proxy",
            url: &url,
            endpoint: okapi_store::egress::Endpoint {
                scheme: &endpoint.scheme,
                host: &endpoint.host,
                port: i32::from(endpoint.port),
                username: None,
            },
            max_keys: None,
            max_concurrency: None,
            note: None,
            status: 1,
            owner_id: None,
        },
        None,
    )
    .await
    .unwrap();
    okapi_store::egress::set_channel_binding(
        &env.pg,
        channel_id,
        &okapi_store::egress::Binding::Proxy { proxy_id },
    )
    .await
    .unwrap()
    .unwrap();
    env.state.invalidate_routing_caches();

    expire_cred(&env, key_id).await;
    let resp = chat(&env).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(
        env.mock_state.token_calls.load(Ordering::SeqCst),
        2,
        "登录一次 + 刷新一次"
    );
    let through_proxy = |path: &str| {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|u| u.contains(path))
            .count()
    };
    assert_eq!(
        through_proxy("/token"),
        1,
        "刷新经代理：{:?}",
        seen.lock().unwrap()
    );
    assert_eq!(
        through_proxy("/v1/messages"),
        1,
        "API 请求经代理：{:?}",
        seen.lock().unwrap()
    );

    // Reauthorize the original key: exchange still uses this channel proxy.
    let client = reqwest::Client::new();
    let started: Value = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&env.admin_token)
        .json(
            &json!({"provider": "anthropic_max", "channel_id":channel_id,"channel_key_id":key_id}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let state = started["state"].as_str().unwrap();
    let resp = client
        .post(format!(
            "http://{}/admin/channels/oauth/exchange",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .json(
            &json!({"state": state, "code": format!("auth-code-2#{state}"),
            "channel_id": channel_id, "channel_key_id": key_id, "token_url": format!("http://{}/token", env.mock)}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(
        through_proxy("/token"),
        2,
        "换码经代理：{:?}",
        seen.lock().unwrap()
    );
}

/// own 范围的渠道管理员不能把 OAuth key 追加到别人的渠道；拒绝发生在换码之前，
/// 一次性的授权码不被白白烧掉（token 端点零调用）。
#[tokio::test]
async fn own_scope_cannot_attach_oauth_key_to_foreign_channel() {
    let env = setup().await;
    let client = reqwest::Client::new();
    // 超管登出来的渠道 = 别人的渠道
    let (foreign_channel, _) = login_channel(&env, "anthropic_max").await;
    let keys_before = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM channel_keys WHERE channel_id = $1"#,
        foreign_channel
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();

    // own 范围渠道管理员
    let role_code = format!("oa-chadmin-{}", Uuid::new_v4().simple());
    let role: Value = client
        .post(format!("http://{}/admin/roles", env.console))
        .bearer_auth(&env.admin_token)
        .json(
            &json!({"role_code": role_code, "display_name": "渠道管理员",
                      "permissions": ["channel.read.own", "channel.write.own"]}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let role_id = role["admin_role_id"].as_i64().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let own_admin = okapi_store::provision::create_user(&env.pg, &format!("oa-own-{suffix}"))
        .await
        .unwrap();
    sqlx::query!(
        "UPDATE users SET role = 10, admin_role_id = $2 WHERE id = $1",
        own_admin,
        role_id
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let own_token = format!("sk-okapi-oa-own-{suffix}");
    okapi_store::provision::create_api_key(&env.pg, own_admin, &hash(&own_token), "sk-oa-own")
        .await
        .unwrap();

    let token_calls_before = env.mock_state.token_calls.load(Ordering::SeqCst);
    let started: Value = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&own_token)
        .json(&json!({"provider": "anthropic_max"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let state = started["state"].as_str().unwrap();
    let resp = client
        .post(format!(
            "http://{}/admin/channels/oauth/exchange",
            env.console
        ))
        .bearer_auth(&own_token)
        .json(&json!({
            "state": state, "code": format!("auth-code-2#{state}"),
            "channel_id": foreign_channel,
            "token_url": format!("http://{}/token", env.mock),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "permission_denied", "{body}");
    assert_eq!(body["error"]["param"], "owner");
    assert_eq!(
        env.mock_state.token_calls.load(Ordering::SeqCst),
        token_calls_before,
        "属主校验必须在换码之前，授权码不得被消耗"
    );
    let keys_after = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM channel_keys WHERE channel_id = $1"#,
        foreign_channel
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(keys_after, keys_before, "别人的渠道没有多出 key");
}

/// 旧开关 `settings.mimic_cc` 已退役：迁移把它改写成最新客户端的显式模拟配置，
/// 已保存的旧版本号改成最新版本；控制台拒绝再写入旧键（旧前端写进来只会被静默忽略）。
#[tokio::test]
async fn retired_mimic_switch_migrates_to_the_latest_client_profile() {
    let env = setup().await;
    let (legacy, _) = login_channel(&env, "anthropic_max").await;
    let (old_revision, _) = login_channel(&env, "anthropic_max").await;
    sqlx::query("UPDATE channels SET settings = settings || $2 WHERE id = $1")
        .bind(legacy)
        .bind(json!({"mimic_cc": true, "mimic_cc_version": "2.1.258", "extensions": null}))
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET settings = settings || $2 WHERE id = $1")
        .bind(old_revision)
        .bind(json!({"mimic_cc": false, "extensions": {"client_profile":
            {"name": "claude-code", "mode": "auto", "revision": "2.1.286", "entrypoint": "sdk-cli"}}}))
        .execute(&env.pg)
        .await
        .unwrap();
    // 迁移只对旧数据生效：在已迁移的库上重放同一份 SQL
    sqlx::raw_sql(include_str!(
        "../../../crates/okapi-store/migrations/0036_claude_code_profile_latest.sql"
    ))
    .execute(&env.pg)
    .await
    .unwrap();
    let settings = |id: i64| {
        let pg = env.pg.clone();
        async move {
            sqlx::query_scalar::<_, Value>("SELECT settings FROM channels WHERE id = $1")
                .bind(id)
                .fetch_one(&pg)
                .await
                .unwrap()
        }
    };
    let migrated = settings(legacy).await;
    assert!(migrated.get("mimic_cc").is_none() && migrated.get("mimic_cc_version").is_none());
    assert_eq!(
        migrated["extensions"]["client_profile"],
        json!({"name": "claude-code", "mode": "mimic", "revision": "2.1.290"})
    );
    let migrated = settings(old_revision).await;
    assert!(migrated.get("mimic_cc").is_none());
    assert_eq!(
        migrated["extensions"]["client_profile"],
        json!({"name": "claude-code", "mode": "auto", "revision": "2.1.290", "entrypoint": "sdk-cli"})
    );

    env.state.invalidate_routing_caches();
    let resp = chat(&env).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let ua = env.mock_state.seen("user-agent").unwrap();
    assert!(ua.starts_with("claude-cli/2.1.290 (external, "), "{ua}");

    let rejected = reqwest::Client::new()
        .patch(format!("http://{}/admin/channels/{legacy}", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"settings": {"mimic_cc": true}}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 400);
}

#[tokio::test]
async fn unexpired_but_rejected_oauth_token_is_refreshed_once_under_concurrency() {
    let env = setup().await;
    let (_, id) = login_channel(&env, "anthropic_max").await;
    env.mock_state.api_rejection.store(1, Ordering::SeqCst);
    let (a, b) = tokio::join!(chat(&env), chat(&env));
    assert_eq!(a.status(), 200, "{}", a.text().await.unwrap());
    assert_eq!(b.status(), 200, "{}", b.text().await.unwrap());
    assert_eq!(
        env.mock_state.token_calls.load(Ordering::SeqCst),
        2,
        "one exchange, one shared forced refresh"
    );
    assert_eq!(read_cred(&env, id).await.access_token, "access-2");
    let status: i16 = sqlx::query_scalar("SELECT status FROM channel_keys WHERE id=$1")
        .bind(id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 1);
}

#[tokio::test]
async fn oauth_401_recovery_is_bounded_and_preserves_permanent_failures() {
    for failure in [
        "still_401",
        "refresh_unavailable",
        "invalid_grant",
        "missing_refresh",
    ] {
        let env = setup().await;
        let (_, id) = login_channel(&env, "anthropic_max").await;
        env.mock_state.api_rejection.store(2, Ordering::SeqCst);
        env.mock_state
            .refresh_unavailable
            .store(failure == "refresh_unavailable", Ordering::SeqCst);
        env.mock_state
            .reject_refresh
            .store(failure == "invalid_grant", Ordering::SeqCst);
        if failure == "missing_refresh" {
            let mut cred = read_cred(&env, id).await;
            cred.refresh_token.clear();
            okapi_store::admin::write_key_credential(
                &env.pg,
                id,
                &cred.to_plaintext(),
                env.state.master_key.as_deref(),
            )
            .await
            .unwrap();
            env.state.invalidate_routing_caches();
        }
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), chat(&env))
            .await
            .unwrap();
        assert_eq!(response.status(), 502, "{failure}");
        assert_eq!(
            env.mock_state.token_calls.load(Ordering::SeqCst),
            if failure == "missing_refresh" { 1 } else { 2 },
            "{failure}: never loop refresh"
        );
        let (status, cooldown): (i16, bool) = sqlx::query_as(
            "SELECT status, cooldown_until IS NOT NULL FROM channel_keys WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
        let permanent = matches!(failure, "invalid_grant" | "missing_refresh");
        assert_eq!(status, if permanent { 6 } else { 3 }, "{failure}");
        assert_eq!(cooldown, !permanent, "{failure}");
        assert_eq!(
            env.state
                .ledger
                .balance(env.user_id)
                .await
                .unwrap()
                .as_micros(),
            10_000_000,
            "failed requests fully refund"
        );
        assert!(
            env.state
                .ledger
                .list_reservations(env.user_id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn codex_401_at_websocket_upgrade_refreshes_without_replaying_a_turn() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    let env = setup().await;
    let (_, id) = login_channel(&env, "codex").await;
    env.mock_state.api_rejection.store(1, Ordering::SeqCst);
    let mut request = format!("ws://{}/v1/responses", env.gateway)
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", env.user_token).parse().unwrap(),
    );
    let (mut client, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    client
        .send(Message::Text(
            json!({"type":"response.create","model":env.model,"max_output_tokens":64,
        "input":[{"role":"user","content":"hi"}]})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let value = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Message::Text(raw) = client.next().await.unwrap().unwrap() {
                let value: Value = serde_json::from_str(&raw).unwrap();
                if value["type"] == "response.completed" || value["type"] == "error" {
                    break value;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(value["type"], "response.completed", "{value}");
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 2);
    assert_eq!(read_cred(&env, id).await.access_token, "access-2");
    assert_eq!(
        env.mock_state.seen("authorization").as_deref(),
        Some("Bearer access-2")
    );
    for _ in 0..50 {
        if env
            .state
            .ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        9_800_000
    );
    let charges: i64 =
        sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1 AND status=20")
            .bind(env.user_id)
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert_eq!(charges, 1);
}

#[tokio::test]
async fn oauth_creation_preserves_account_controls_and_initial_concurrency() {
    for provider in ["anthropic_max", "codex"] {
        let env = setup().await;
        let client = reqwest::Client::new();
        let start: Value = client
            .post(format!("http://{}/admin/channels/oauth/start", env.console))
            .bearer_auth(&env.admin_token)
            .json(&json!({"provider":provider}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let settings = json!({"responses_native":true,"account_control":{"refresh_mode":"external","usage":{"period":"week","requests":100,"cost_micro":1_234_567},"failure_threshold":2}});
        let response=client.post(format!("http://{}/admin/channels/oauth/exchange",env.console)).bearer_auth(&env.admin_token)
            .json(&json!({"state":start["state"],"code":format!("mock-code#{}",start["state"].as_str().unwrap()),"name":format!("controlled-{provider}-{}",Uuid::new_v4().simple()),"models":[env.model],"token_url":format!("http://{}/token",env.mock),"settings":settings,"max_concurrency":3})).send().await.unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
        let body: Value = response.json().await.unwrap();
        let channel = body["channel_id"].as_i64().unwrap();
        let key = body["channel_key_id"].as_i64().unwrap();
        let stored: Value = sqlx::query_scalar("SELECT settings FROM channels WHERE id=$1")
            .bind(channel)
            .fetch_one(&env.pg)
            .await
            .unwrap();
        assert_eq!(stored["account_control"], settings["account_control"]);
        assert_eq!(stored["responses_native"], true);
        let cap: Option<i32> =
            sqlx::query_scalar("SELECT max_concurrency FROM channel_keys WHERE id=$1")
                .bind(key)
                .fetch_one(&env.pg)
                .await
                .unwrap();
        assert_eq!(cap, Some(3));
        let response = client
            .post(format!(
                "http://{}/admin/channels/{channel}/keys/{key}/oauth/refresh",
                env.console
            ))
            .bearer_auth(&env.admin_token)
            .send()
            .await
            .unwrap();
        assert!(!response.status().is_success());
        assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn an_existing_subscription_channel_cannot_append_another_account() {
    let env = setup().await;
    let (channel, key) = login_channel(&env, "anthropic_max").await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"provider":"anthropic_max","channel_id":channel}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["param"],
        "channel_key_id"
    );
    let started: Value = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"provider":"anthropic_max"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let response=client.post(format!("http://{}/admin/channels/oauth/exchange",env.console)).bearer_auth(&env.admin_token)
        .json(&json!({"state":started["state"],"code":"unused-code","channel_id":channel,"token_url":format!("http://{}/token",env.mock)})).send().await.unwrap();
    assert_eq!(response.status(), 400);
    let keys: Vec<i64> = sqlx::query_scalar("SELECT id FROM channel_keys WHERE channel_id=$1")
        .bind(channel)
        .fetch_all(&env.pg)
        .await
        .unwrap();
    assert_eq!(keys, vec![key]);
    assert_eq!(
        env.mock_state.token_calls.load(Ordering::SeqCst),
        1,
        "rejected append must not consume the authorization code upstream"
    );
}
