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
    /// 上游最近一次收到的请求头（断言客户端身份头透传）。
    last_headers: Arc<std::sync::Mutex<Vec<(String, String)>>>,
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

/// Anthropic 订阅上游：断言 `?beta=true` / Bearer / 必备 beta / 系统首句 / 无 x-api-key，回一条 Messages JSON。
async fn mock_messages(
    State(st): State<Mock>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    st.record(&headers);
    assert_eq!(
        query.as_deref(),
        Some("beta=true"),
        "订阅路径 URL 带 ?beta=true"
    );
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
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

/// Codex 订阅上游：断言 chatgpt-account-id / accept / 请求体整形（store=false、stream=true、
/// instructions 存在、system→developer、previous_response_id 被剥），只回 SSE（该后端只有流式面）。
async fn mock_codex_responses(
    State(st): State<Mock>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    st.record(&headers);
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
    assert_eq!(req["store"], false, "Codex 后端不持久化：store 强制 false");
    assert_eq!(req["stream"], true, "Codex 后端只有流式面");
    assert!(req["instructions"].is_string(), "instructions 键必须存在");
    assert!(
        req.get("previous_response_id").is_none(),
        "store=false 下无意义，剥掉"
    );
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
        last_headers: Arc::default(),
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

/// 渠道配了 `proxy_url` 时，token 刷新与追加 key 的换码都得和 API 请求一样走这个代理：
/// 订阅账号对出口 IP 敏感，只有代理能出网的部署也才刷得动。
#[tokio::test]
async fn token_refresh_and_attach_exchange_use_channel_proxy() {
    let env = setup().await;
    let (channel_id, key_id) = login_channel(&env, "anthropic_max").await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let proxy = spawn_recording_proxy(Arc::clone(&seen)).await;
    sqlx::query!(
        r#"UPDATE channels SET settings = settings || $2 WHERE id = $1"#,
        channel_id,
        json!({"proxy_url": format!("http://{proxy}")})
    )
    .execute(&env.pg)
    .await
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

    // 给这条渠道追加一把 key：换码也走它的代理
    let client = reqwest::Client::new();
    let started: Value = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&env.admin_token)
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
        .bearer_auth(&env.admin_token)
        .json(
            &json!({"state": state, "code": format!("auth-code-2#{state}"),
            "channel_id": channel_id, "token_url": format!("http://{}/token", env.mock)}),
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
