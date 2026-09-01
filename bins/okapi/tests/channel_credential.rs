//! 渠道凭证信封落库（AES-256-GCM）的端到端语义：
//! 密文真的进了库、解封后链路照通、存量明文行不受影响、丢主密钥 fail-closed。
//! 依赖 .env 的 DATABASE_URL 与 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::json;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// 上游看到的 Authorization 头（验证解封结果真的发出去了）。
type SeenAuth = Arc<Mutex<Vec<String>>>;

const UPSTREAM_SECRET: &str = "sk-upstream-should-never-be-plaintext";

fn master_key() -> String {
    hex::encode([0x5au8; 32])
}

async fn spawn_mock(seen: SeenAuth) -> SocketAddr {
    let handler = move |headers: axum::http::HeaderMap, _body: axum::body::Bytes| {
        let seen = Arc::clone(&seen);
        async move {
            let auth = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            seen.lock().unwrap().push(auth);
            axum::Json(json!({
                "id": "cmpl", "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5}
            }))
            .into_response()
        }
    };
    let router = Router::new().route("/up/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Env {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    model: String,
    channel_key_id: i64,
    seen: SeenAuth,
}

/// `seal_at_rest`：建渠道时是否用主密钥封装。
/// `gateway_has_key`：网关侧是否持有主密钥（模拟丢密钥/未配置）。
async fn setup(seal_at_rest: bool, gateway_has_key: bool) -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-cred-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-cred")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let seen: SeenAuth = Arc::new(Mutex::new(Vec::new()));
    let mock = spawn_mock(Arc::clone(&seen)).await;
    let mk = master_key();
    let (_, channel_key_id) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        "openai",
        &format!("http://{mock}/up/v1"),
        UPSTREAM_SECRET,
        &[model.as_str()],
        false,
        seal_at_rest.then_some(mk.as_str()),
    )
    .await
    .unwrap();

    let mut state = gateway::build_state(&database_url, &redis_url, "cred-node", None, None)
        .await
        .unwrap();
    state.master_key = gateway_has_key.then(|| Arc::from(mk.as_str()));
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

    Env {
        pg,
        gateway: addr,
        token,
        model,
        channel_key_id,
        seen,
    }
}

async fn stored_bytes(pg: &PgPool, channel_key_id: i64) -> Vec<u8> {
    sqlx::query_scalar!(
        r#"SELECT credential_ciphertext FROM channel_keys WHERE id = $1"#,
        channel_key_id
    )
    .fetch_one(pg)
    .await
    .unwrap()
}

async fn chat(env: &Env) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "stream": false,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap()
}

/// 主用例：库里是密文，但链路照通——解封结果原样进了上游 Authorization。
#[tokio::test]
async fn sealed_credential_is_ciphertext_at_rest_yet_serves_traffic() {
    let env = setup(true, true).await;

    let stored = stored_bytes(&env.pg, env.channel_key_id).await;
    assert!(stored.starts_with(b"okc1"), "落库应带信封前缀");
    assert!(
        !String::from_utf8_lossy(&stored).contains("sk-upstream"),
        "库里绝不能出现明文片段"
    );

    let resp = chat(&env).await;
    assert_eq!(resp.status(), 200);
    let auth = env.seen.lock().unwrap().first().cloned().unwrap();
    assert_eq!(
        auth,
        format!("Bearer {UPSTREAM_SECRET}"),
        "解封后必须原样发给上游"
    );
}

/// 向后兼容：升级前落库的明文行照读——不需要停机迁移。
#[tokio::test]
async fn legacy_plaintext_row_still_serves_after_upgrade() {
    let env = setup(false, true).await;

    let stored = stored_bytes(&env.pg, env.channel_key_id).await;
    assert_eq!(stored, UPSTREAM_SECRET.as_bytes(), "存量行仍是明文");

    let resp = chat(&env).await;
    assert_eq!(resp.status(), 200, "带主密钥的网关也要能读存量明文行");
    let auth = env.seen.lock().unwrap().first().cloned().unwrap();
    assert_eq!(auth, format!("Bearer {UPSTREAM_SECRET}"));
}

/// 丢主密钥：拒绝服务，而不是把密文当明文发给上游。
#[tokio::test]
async fn sealed_row_without_master_key_fails_closed() {
    let env = setup(true, false).await;

    let resp = chat(&env).await;
    assert_ne!(resp.status(), 200, "解不开就不该放行");
    assert!(
        env.seen.lock().unwrap().is_empty(),
        "绝不能拿解不开的凭证去打上游"
    );
}
