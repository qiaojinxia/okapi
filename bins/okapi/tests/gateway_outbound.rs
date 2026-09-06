//! 渠道出站代理 + 额外请求头验收（IMPLEMENTATION §11.30）：
//! extra_headers 出现在上游、受保护头被热路径跳过、HTTP 正向代理真正经手。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

type Captured = Arc<Mutex<Option<(HeaderMap, Value)>>>;

async fn spawn_upstream(captured: Captured) -> SocketAddr {
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let captured = Arc::clone(&captured);
            async move {
                let req: Value = serde_json::from_slice(&body).unwrap();
                *captured.lock().unwrap() = Some((headers, req));
                axum::Json(json!({
                    "id":"cmpl","object":"chat.completion","model":"m",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
                    "usage":{"prompt_tokens":10,"completion_tokens":5,
                             "prompt_tokens_details":{"cached_tokens":0}}
                }))
                .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// 最小 HTTP 正向代理：把绝对 URI 原样转发给上游，计数命中。
async fn spawn_proxy(hits: Arc<AtomicUsize>) -> SocketAddr {
    let router = Router::new().fallback(move |req: Request| {
        let hits = Arc::clone(&hits);
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            let method = req.method().clone();
            let uri = req.uri().to_string();
            let headers = req.headers().clone();
            let body = to_bytes(req.into_body(), 16 * 1024 * 1024)
                .await
                .unwrap_or_default();
            let client = reqwest::Client::new();
            let mut b = client.request(method, uri);
            for (k, v) in &headers {
                if k == header::HOST || k == header::CONNECTION {
                    continue;
                }
                b = b.header(k, v);
            }
            match b.body(body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let ct = resp.headers().get(header::CONTENT_TYPE).cloned();
                    let bytes = resp.bytes().await.unwrap_or_default();
                    let mut out = Response::new(Body::from(bytes));
                    *out.status_mut() = status;
                    if let Some(ct) = ct {
                        out.headers_mut().insert(header::CONTENT_TYPE, ct);
                    }
                    out
                }
                Err(_) => StatusCode::BAD_GATEWAY.into_response(),
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

use axum::http::StatusCode;

struct TestEnv {
    gateway: SocketAddr,
    token: String,
    model: String,
    captured: Captured,
    proxy_hits: Arc<AtomicUsize>,
}

async fn setup(settings: Value) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("ob-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-ob-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-ob")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();

    let captured: Captured = Arc::new(Mutex::new(None));
    let upstream = spawn_upstream(Arc::clone(&captured)).await;
    let proxy_hits = Arc::new(AtomicUsize::new(0));
    let mut settings = settings;
    if settings.get("proxy_url").is_some_and(|v| v == "USE_PROXY") {
        let proxy = spawn_proxy(Arc::clone(&proxy_hits)).await;
        settings["proxy_url"] = json!(format!("http://{proxy}"));
    }

    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ob-ch-{suffix}"),
        "openai",
        &format!("http://{upstream}/v1"),
        "mock-credential",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channels SET settings = $2 WHERE id = $1")
        .bind(channel_id)
        .bind(&settings)
        .execute(&pg)
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
        gateway: addr,
        token,
        model,
        captured,
        proxy_hits,
    }
}

async fn chat(env: &TestEnv) -> (HeaderMap, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let _ = resp.bytes().await.unwrap();
    env.captured
        .lock()
        .unwrap()
        .clone()
        .expect("mock 应收到上游请求")
}

/// 额外头出现；写入时本该拒掉的 Authorization 即使被 SQL 塞进 settings 也不覆盖凭证。
#[tokio::test]
async fn extra_headers_sent_forbidden_skipped() {
    let env = setup(json!({
        "extra_headers": {
            "OpenAI-Organization": "org-test",
            "Authorization": "Bearer hijack"
        }
    }))
    .await;
    let (headers, _) = chat(&env).await;
    assert_eq!(
        headers
            .get("openai-organization")
            .and_then(|v| v.to_str().ok()),
        Some("org-test")
    );
    assert_eq!(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer mock-credential"),
        "鉴权头必须仍是渠道凭证"
    );
}

/// HTTP 正向代理真正经手（计数 > 0），上游仍收到请求。
#[tokio::test]
async fn http_proxy_is_used() {
    let env = setup(json!({"proxy_url": "USE_PROXY"})).await;
    let (_, body) = chat(&env).await;
    assert_eq!(body["messages"][0]["content"], "hi");
    assert!(
        env.proxy_hits.load(Ordering::SeqCst) >= 1,
        "请求必须经过代理"
    );
}
