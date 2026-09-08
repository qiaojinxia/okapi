//! 云渠道的管理面测活与模型发现（IMPLEMENTATION §11.35 / §11.38）。
//!
//! `console/cloud_probe.rs` 把 bedrock / vertex / anthropic_max / codex 四家 ×
//! credential / model 两种探测范围分派到各自的上游客户端——签名、换 token、刷新都在这条路上。
//! 此前只有客户端层的单测，管理面这两个端点（`POST /admin/channels/{id}/test`、
//! `GET /admin/channels/{id}/fetch-models`）一个集成用例都没有。本套件用 mock 上游把
//! 能打通的分支全部走一遍，并核对结果形状与 `admin::probe_channel` 一致（前端不感知差异）。
//!
//! 唯一接不上 mock 的分支：bedrock 的 SigV4 测活与拉模型走 `ListFoundationModels`，控制面主机
//! 在 `bedrock.rs` 里固定成 `https://bedrock.{region}.amazonaws.com`（有意为之，见该处注释），
//! 只能留给 `aws_sigv4` 单测和真实凭证。
//! 依赖 .env（scripts/dev-deps.sh up）。

use aws_lc_rs::encoding::AsDer as _;
use axum::Router;
use axum::extract::{OriginalUri, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine as _;
use okapi::{console, gateway};
use okapi_store::credential::OAuthCredential;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

const VERTEX_PATH: &str = "/v1/projects/p-okapi/locations/us-central1";
const BEDROCK_MODEL: &str = "anthropic.claude-sonnet-4-5-20250929-v1:0";

// ---- mock 上游 ----

#[derive(Clone)]
struct Mock {
    token_calls: Arc<AtomicUsize>,
}

/// 现场生成一把 2048 位 RSA 私钥，拼成服务账号 JSON（`token_uri` 指向 mock）。
fn service_account_json(token_uri: &str) -> String {
    let key = aws_lc_rs::signature::RsaKeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
    let der = key.as_der().unwrap();
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
    for chunk in b64.as_bytes().chunks(64) {
        let _ = writeln!(pem, "{}", std::str::from_utf8(chunk).unwrap());
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    json!({
        "type": "service_account",
        "project_id": "p-okapi",
        "client_email": "okapi@p-okapi.iam.gserviceaccount.com",
        "private_key": pem,
        "token_uri": token_uri,
    })
    .to_string()
}

/// Vertex 的服务账号换 token：JWT-bearer 表单，回一把带序号的 access token。
async fn mock_vertex_token(State(st): State<Mock>, body: String) -> axum::response::Response {
    let n = st.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(
        body.starts_with(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="
        ),
        "JWT-bearer 表单：{body}"
    );
    axum::Json(
        json!({"access_token": format!("ya29.tok-{n}"), "expires_in": 3600,
                      "token_type": "Bearer"}),
    )
    .into_response()
}

/// Vertex 的 token 端点回 401：测活要把上游状态原样报出来。
async fn mock_vertex_token_denied() -> axum::response::Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error": "invalid_grant"})),
    )
        .into_response()
}

async fn mock_vertex_generate(
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    assert!(
        uri.path().ends_with(":generateContent"),
        "非流式探测打 :generateContent，实际 {}",
        uri.path()
    );
    assert!(
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer ya29.tok-")),
        "Bearer 必须是换出来的 token"
    );
    axum::Json(
        json!({"candidates": [{"content": {"parts": [{"text": "pong"}]}}],
                      "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 1}}),
    )
    .into_response()
}

/// Bedrock 的 API key 形态测活：只验 key 认不认。
async fn mock_bedrock_openai_models(headers: axum::http::HeaderMap) -> axum::response::Response {
    if headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        == Some("Bearer bedrock-api-key")
    {
        axum::Json(json!({"object": "list", "data": [{"id": "anthropic.claude-x"}]}))
            .into_response()
    } else {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({"message": "The security token included in the request is invalid"})),
        )
            .into_response()
    }
}

/// Bedrock 的模型探测：SigV4 签过名的 InvokeModel。
async fn mock_bedrock_invoke(
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert!(
        uri.path().ends_with("-v1%3A0/invoke"),
        "模型 ID 里的冒号编成 %3A，实际 {}",
        uri.path()
    );
    assert!(
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("AWS4-HMAC-SHA256 Credential=")),
        "SigV4 凭证走签名头"
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert!(req.get("model").is_none(), "model 走 URL，不进体");
    assert_eq!(req["anthropic_version"], "bedrock-2023-05-31");
    axum::Json(
        json!({"id":"msg_b","type":"message","role":"assistant","model":"claude-x",
                      "content":[{"type":"text","text":"pong"}],"stop_reason":"end_turn",
                      "usage":{"input_tokens":3,"output_tokens":1}}),
    )
    .into_response()
}

/// 订阅凭证的刷新端点：回 access-N / refresh-N（轮转）。
async fn mock_oauth_token(State(st): State<Mock>, body: String) -> axum::response::Response {
    let n = st.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
    assert!(
        body.contains("refresh_token"),
        "测活只会走刷新，不该换码：{body}"
    );
    axum::Json(
        json!({"access_token": format!("access-{n}"), "refresh_token": format!("refresh-{n}"),
                      "expires_in": 28800, "token_type": "Bearer"}),
    )
    .into_response()
}

async fn mock_max_messages(headers: axum::http::HeaderMap) -> axum::response::Response {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(auth.starts_with("Bearer access-"), "订阅 token 走 Bearer");
    assert!(headers.get("x-api-key").is_none(), "不得再带 x-api-key");
    axum::Json(
        json!({"id":"msg_m","type":"message","role":"assistant","model":"claude-x",
                      "content":[{"type":"text","text":"pong"}],"stop_reason":"end_turn",
                      "usage":{"input_tokens":3,"output_tokens":1}}),
    )
    .into_response()
}

async fn mock_codex_responses(headers: axum::http::HeaderMap) -> axum::response::Response {
    assert_eq!(
        headers
            .get("chatgpt-account-id")
            .and_then(|v| v.to_str().ok()),
        Some("acct-okapi"),
        "account id 随凭证下发"
    );
    let sse = concat!(
        "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_p\",\"status\":\"in_progress\"}}\n\n",
        "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_p\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"gpt-5\",\"output\":[{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"pong\",\"annotations\":[]}]}],\"usage\":{\"input_tokens\":3,\"output_tokens\":1,\"total_tokens\":4}}}\n\n",
    );
    ([("content-type", "text/event-stream")], sse).into_response()
}

async fn spawn_mock(mock: Mock) -> SocketAddr {
    let router = Router::new()
        .route("/vertex/token", post(mock_vertex_token))
        .route("/vertex/token-denied", post(mock_vertex_token_denied))
        .route(
            &format!("{VERTEX_PATH}/publishers/google/models/{{model_action}}"),
            post(mock_vertex_generate),
        )
        .route("/openai/v1/models", get(mock_bedrock_openai_models))
        .route("/model/{model_id}/invoke", post(mock_bedrock_invoke))
        .route("/oauth/token", post(mock_oauth_token))
        .route("/v1/messages", post(mock_max_messages))
        .route("/codex/responses", post(mock_codex_responses))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

// ---- 环境 ----

struct Env {
    pg: PgPool,
    console: SocketAddr,
    mock: SocketAddr,
    admin_token: String,
    token_calls: Arc<AtomicUsize>,
    suffix: String,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("cp-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-cp-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(admin_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &hash, "sk-okapi-cp")
        .await
        .unwrap();

    let token_calls = Arc::new(AtomicUsize::new(0));
    let mock = spawn_mock(Mock {
        token_calls: Arc::clone(&token_calls),
    })
    .await;

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    Env {
        pg,
        console,
        mock,
        admin_token,
        token_calls,
        suffix,
    }
}

/// 直接落库建渠道（绕开管理面写入的 SSRF 闸：mock 在 127.0.0.1，闸本身另有 `console_ssrf` 覆盖）。
async fn mk_channel(
    env: &Env,
    provider: &str,
    api_base: &str,
    credential: &str,
    settings: Value,
) -> (i64, i64) {
    let tag = Uuid::new_v4().simple().to_string();
    let model = format!("m-cp-{}", &tag[..8]);
    let (channel_id, key_id) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("{provider}-{tag}"),
        provider,
        api_base,
        credential,
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query!(
        r#"UPDATE channels SET settings = settings || $2 WHERE id = $1"#,
        channel_id,
        settings
    )
    .execute(&env.pg)
    .await
    .unwrap();
    (channel_id, key_id)
}

async fn probe(env: &Env, id: i64, model: Option<&str>) -> Value {
    let req = reqwest::Client::new()
        .post(format!("http://{}/admin/channels/{id}/test", env.console))
        .bearer_auth(&env.admin_token);
    let req = match model {
        Some(m) => req.json(&json!({ "model": m })),
        None => req,
    };
    req.send().await.unwrap().json().await.unwrap()
}

async fn fetch_models(env: &Env, id: i64) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/channels/{id}/fetch-models",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap())
}

/// OAuth 订阅凭证：`expires_at` 给到过去就必须先刷新。
fn oauth_credential(expires_at: i64, account_id: Option<&str>) -> String {
    OAuthCredential {
        access_token: "access-stale".to_owned(),
        refresh_token: "refresh-0".to_owned(),
        expires_at,
        account_id: account_id.map(str::to_owned),
    }
    .to_plaintext()
}

// ---- 用例 ----

/// vertex：两种探测范围各走一遍换 token（credential 只换 token，model 还要真发一次
/// generateContent），token 端点报错时把上游状态原样带出；vertex 没有公开列表接口，
/// 拉模型明确回不支持。结果形状与通用探测一致，并回填列表页的"最近测试"。
#[tokio::test]
async fn vertex_probe_covers_both_scopes_and_reports_token_failure() {
    let env = setup().await;
    let credential = service_account_json(&format!("http://{}/vertex/token", env.mock));
    let (id, _) = mk_channel(
        &env,
        "vertex",
        &format!("http://{}{VERTEX_PATH}", env.mock),
        &credential,
        json!({}),
    )
    .await;

    let cred_scope = probe(&env, id, None).await;
    assert_eq!(cred_scope["ok"], true, "{cred_scope}");
    assert_eq!(cred_scope["http_status"], 200);
    assert_eq!(cred_scope["scope"], "credential");
    assert!(cred_scope["model"].is_null());
    assert!(cred_scope["at"].is_string());
    assert!(cred_scope["latency_ms"].is_i64());

    let model_scope = probe(&env, id, Some("gemini-2.5-flash")).await;
    assert_eq!(model_scope["ok"], true, "{model_scope}");
    assert_eq!(model_scope["scope"], "model");
    assert_eq!(model_scope["model"], "gemini-2.5-flash");
    assert!(
        env.token_calls.load(Ordering::SeqCst) >= 1,
        "两次探测都要换 token"
    );

    // vertex 没有稳定的公开列表接口
    let (status, body) = fetch_models(&env, id).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["param"], "fetch_models_unsupported", "{body}");

    // token 端点 401：ok=false 且带上游状态与原文
    let denied = service_account_json(&format!("http://{}/vertex/token-denied", env.mock));
    let (bad_id, _) = mk_channel(
        &env,
        "vertex",
        &format!("http://{}{VERTEX_PATH}", env.mock),
        &denied,
        json!({}),
    )
    .await;
    let failed = probe(&env, bad_id, None).await;
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(failed["http_status"], 401);
    assert!(
        failed["upstream_body"]
            .as_str()
            .is_some_and(|b| b.contains("invalid_grant")),
        "{failed}"
    );

    // 留痕回填列表页"最近测试"（与通用探测同一机制）
    let row = channel_row(&env, id).await;
    assert_eq!(row["last_test"]["scope"], "model", "{row}");
    assert_eq!(row["last_test"]["ok"], true);
}

/// bedrock 两种凭证形态：Bearer API key 只能验 key（列 OpenAI 兼容模型），SigV4 才能签
/// InvokeModel；拉模型要求 SigV4，Bearer 凭证明确回参数错而不是打上游。
#[tokio::test]
async fn bedrock_probe_splits_api_key_and_sigv4_credentials() {
    let env = setup().await;
    let region = json!({"aws_region": "us-east-1"});

    // Bearer 形态：认得的 key → ok
    let (ok_id, _) = mk_channel(
        &env,
        "bedrock",
        &format!("http://{}", env.mock),
        "bedrock-api-key",
        region.clone(),
    )
    .await;
    let ok = probe(&env, ok_id, None).await;
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["http_status"], 200);
    assert_eq!(ok["scope"], "credential");

    // Bearer 形态：不认的 key → 上游 401 原样报出
    let (bad_id, _) = mk_channel(
        &env,
        "bedrock",
        &format!("http://{}", env.mock),
        "wrong-key",
        region.clone(),
    )
    .await;
    let bad = probe(&env, bad_id, None).await;
    assert_eq!(bad["ok"], false, "{bad}");
    assert_eq!(bad["http_status"], 401);
    assert!(
        bad["upstream_body"]
            .as_str()
            .is_some_and(|b| b.contains("security token")),
        "{bad}"
    );

    // Bearer 凭证拉不了模型：ListFoundationModels 只认 SigV4
    let (status, body) = fetch_models(&env, ok_id).await;
    assert_eq!(status, 400);
    assert_eq!(
        body["error"]["param"], "fetch_models_requires_sigv4",
        "{body}"
    );

    // SigV4 形态 + 模型探测：真发一次签过名的 InvokeModel
    let (sig_id, _) = mk_channel(
        &env,
        "bedrock",
        &format!("http://{}", env.mock),
        "AKIAOKAPIEXAMPLE:c2VjcmV0LWtleS1va2FwaS1leGFtcGxl",
        region,
    )
    .await;
    let signed = probe(&env, sig_id, Some(BEDROCK_MODEL)).await;
    assert_eq!(signed["ok"], true, "{signed}");
    assert_eq!(signed["scope"], "model");
    assert_eq!(signed["model"], BEDROCK_MODEL);
}

/// anthropic_max / codex 订阅渠道：测活就是"这把订阅还能用吗"。没到期的凭证零网络往返，
/// 过期的先刷新再探；model 范围真发一次补全（codex 只有流式面，非流式探测靠 SSE 聚合成 JSON）。
/// 刷新轮转出的新 refresh_token 必须回写，否则下次刷新会拿着废掉的 token 去换。
#[tokio::test]
async fn subscription_probe_refreshes_only_when_expired() {
    let env = setup().await;
    let now = chrono::Utc::now().timestamp();
    let token_url = json!({"oauth_token_url": format!("http://{}/oauth/token", env.mock)});

    // 未到期：直接判可用，不碰 token 端点
    let (fresh_id, _) = mk_channel(
        &env,
        "anthropic_max",
        &format!("http://{}/v1", env.mock),
        &oauth_credential(now + 3600, None),
        token_url.clone(),
    )
    .await;
    let before = env.token_calls.load(Ordering::SeqCst);
    let fresh = probe(&env, fresh_id, None).await;
    assert_eq!(fresh["ok"], true, "{fresh}");
    assert_eq!(fresh["scope"], "credential");
    assert_eq!(
        env.token_calls.load(Ordering::SeqCst),
        before,
        "没到期不该刷新"
    );

    // 已过期：先刷新再判可用，轮转出的 refresh_token 回写
    let (stale_id, stale_key) = mk_channel(
        &env,
        "anthropic_max",
        &format!("http://{}/v1", env.mock),
        &oauth_credential(now - 60, None),
        token_url.clone(),
    )
    .await;
    let refreshed = probe(&env, stale_id, None).await;
    assert_eq!(refreshed["ok"], true, "{refreshed}");
    assert!(
        env.token_calls.load(Ordering::SeqCst) > before,
        "过期凭证必须走刷新"
    );
    // 刷新回写走 state.master_key（有配就重新封装），读回来要用同一把
    let master_key = std::env::var("OKAPI_MASTER_KEY").ok();
    let stored = okapi_store::admin::read_key_credential(&env.pg, stale_key, master_key.as_deref())
        .await
        .unwrap()
        .unwrap();
    let stored = OAuthCredential::parse(&stored).expect("仍是 OAuth 凭证");
    assert_ne!(stored.refresh_token, "refresh-0", "refresh 轮转须回写");
    assert!(stored.expires_at > now, "新的到期时间要往后推");

    // model 范围：anthropic_max 真发一次 Messages
    let max_model = probe(&env, stale_id, Some("claude-sonnet-4-5")).await;
    assert_eq!(max_model["ok"], true, "{max_model}");
    assert_eq!(max_model["scope"], "model");

    // codex：account_id 随凭证下发，非流式探测由 SSE 聚合成 JSON
    let (codex_id, _) = mk_channel(
        &env,
        "codex",
        &format!("http://{}/codex", env.mock),
        &oauth_credential(now - 60, Some("acct-okapi")),
        token_url,
    )
    .await;
    let codex_cred = probe(&env, codex_id, None).await;
    assert_eq!(codex_cred["ok"], true, "{codex_cred}");
    let codex_model = probe(&env, codex_id, Some("gpt-5")).await;
    assert_eq!(codex_model["ok"], true, "{codex_model}");
    assert_eq!(codex_model["http_status"], 200);

    // 订阅登录没有模型列表面
    for id in [stale_id, codex_id] {
        let (status, body) = fetch_models(&env, id).await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["param"], "fetch_models_unsupported", "{body}");
    }
}

/// 共享开发库里渠道很多，按页翻到本用例的渠道为止。
async fn channel_row(env: &Env, id: i64) -> Value {
    let client = reqwest::Client::new();
    let mut offset = 0;
    loop {
        let list: Value = client
            .get(format!(
                "http://{}/admin/channels?limit=100&offset={offset}",
                env.console
            ))
            .bearer_auth(&env.admin_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let page = list["data"].as_array().unwrap();
        if let Some(row) = page.iter().find(|c| c["id"].as_i64() == Some(id)) {
            return row.clone();
        }
        assert!(
            !page.is_empty(),
            "翻完全部分页仍未找到渠道 {id}（{}）",
            env.suffix
        );
        offset += 100;
    }
}
