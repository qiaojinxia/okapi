//! 渠道出站代理 + 额外请求头验收（IMPLEMENTATION §11.30 / §11.41）：
//! extra_headers 出现在上游、受保护头被热路径跳过、经出口绑定的 HTTP 正向代理真正经手、
//! 退役的 settings.proxy_url 残留不再生效。
//! 依赖 .env（scripts/dev-deps.sh up）。

#[path = "support/published_pricing.rs"]
mod published_pricing;

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

/// 建一个代理并把渠道的出口绑到它（§11.41）。
async fn bind_proxy(pg: &sqlx::PgPool, channel_id: i64, url: &str, suffix: &str) {
    let endpoint = okapi_providers::http::ProxyEndpoint::parse(url).unwrap();
    let proxy_id = okapi_store::egress::create_proxy(
        pg,
        &okapi_store::egress::NewProxy {
            name: &format!("ob-px-{suffix}"),
            url,
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
        pg,
        channel_id,
        &okapi_store::egress::Binding::Proxy { proxy_id },
    )
    .await
    .unwrap()
    .unwrap();
}

struct TestEnv {
    gateway: SocketAddr,
    token: String,
    model: String,
    captured: Captured,
    proxy_hits: Arc<AtomicUsize>,
}

async fn setup(settings: Value) -> TestEnv {
    okapi_store::test_support::assert_isolated();
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
    // "USE_PROXY" = 经出口绑定（§11.41）挂一个代理；"LEGACY_PROXY" = 只在 settings 里残留旧键
    let proxy_mode = settings
        .get("proxy_url")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut proxy_url = None;
    if proxy_mode.is_some() {
        let proxy = spawn_proxy(Arc::clone(&proxy_hits)).await;
        proxy_url = Some(format!("http://{proxy}"));
        if proxy_mode.as_deref() == Some("LEGACY_PROXY") {
            settings["proxy_url"] = json!(proxy_url);
        } else {
            settings.as_object_mut().unwrap().remove("proxy_url");
        }
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
    if proxy_mode.as_deref() == Some("USE_PROXY") {
        bind_proxy(&pg, channel_id, &proxy_url.clone().unwrap(), &suffix).await;
    }

    published_pricing::publish(&pg, user_id).await;
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

/// 渠道绑定的 HTTP 正向代理真正经手（计数 > 0），上游仍收到请求。
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

/// 退役的 settings.proxy_url 残留（迁移解析不了而保留、或绕过写入校验塞进库的）不再被任何路径
/// 当成出口：出口只认绑定。
#[tokio::test]
async fn legacy_settings_proxy_is_ignored() {
    let env = setup(json!({"proxy_url": "LEGACY_PROXY"})).await;
    chat(&env).await;
    assert_eq!(env.proxy_hits.load(Ordering::SeqCst), 0);
}
