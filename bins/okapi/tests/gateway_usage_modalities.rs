//! Same upstream token fixture through Chat, Responses and Gemini, JSON and SSE.
use axum::{Json, Router, extract::State, http::Uri, response::IntoResponse, routing::post};
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[path = "support/published_pricing.rs"]
mod published_pricing;

#[path = "support/anthropic_usage.rs"]
mod anthropic_usage;

#[path = "support/usage_provenance.rs"]
mod usage_provenance;

#[derive(Clone, Copy, Debug)]
enum Protocol {
    Chat,
    Responses,
    Gemini,
    Anthropic,
}

impl Protocol {
    fn fixture(self) -> Value {
        let all: Value = serde_json::from_str(include_str!(
            "../../../crates/okapi-providers/tests/fixtures/multimodal_usage.json"
        ))
        .unwrap();
        all[match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Gemini => "gemini",
            Self::Anthropic => return anthropic_usage::fixture(),
        }]
        .clone()
    }
}

#[derive(Clone)]
struct Mock {
    protocol: Protocol,
    usage: Value,
    calls: Arc<AtomicUsize>,
}

async fn upstream(
    State(mock): State<Mock>,
    uri: Uri,
    Json(body): Json<Value>,
) -> axum::response::Response {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    let stream = body["stream"] == true || uri.path().ends_with(":streamGenerateContent");
    let (json, events) = match mock.protocol {
        Protocol::Chat => {
            assert_eq!(uri.path(), "/up/v1/chat/completions");
            let usage = mock.usage.get("final").unwrap_or(&mock.usage);
            let body = json!({"id":"c","object":"chat.completion","model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":usage});
            let mut events = format!(
                "data: {}\n\n",
                json!({"choices":[{"index":0,"delta":{"content":"ok"}}]})
            );
            let updates = mock
                .usage
                .get("updates")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| vec![usage.clone()]);
            for usage in updates {
                writeln!(events, "data: {}\n", json!({"choices":[],"usage":usage})).unwrap();
            }
            events.push_str("data: [DONE]\n\n");
            (body, events)
        }
        Protocol::Responses => {
            assert_eq!(uri.path(), "/up/v1/responses");
            let body = json!({"id":format!("resp_{}", Uuid::new_v4().simple()),"object":"response","status":"completed","output":[{"id":"msg_1","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":mock.usage});
            let events = format!(
                "event: response.output_text.delta\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                json!({"type":"response.output_text.delta","delta":"ok"}),
                json!({"type":"response.completed","response":body})
            );
            (body, events)
        }
        Protocol::Gemini => {
            assert!(uri.path().starts_with("/up/v1beta/models/"));
            let body = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}],"usageMetadata":mock.usage});
            let events = format!(
                "data: {}\n\ndata: {body}\n\n",
                json!({"candidates":[{"content":{"parts":[{"text":"prefix"}]}}]})
            );
            (body, events)
        }
        Protocol::Anthropic => {
            assert_eq!(uri.path(), "/up/v1/messages");
            anthropic_usage::response(&mock.usage)
        }
    };
    if stream {
        ([("content-type", "text/event-stream")], events).into_response()
    } else {
        Json(json).into_response()
    }
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Test servers live until their owning Tokio test runtime is dropped.
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Env {
    state: gateway::state::AppState,
    gateway: SocketAddr,
    console: SocketAddr,
    user: i64,
    token: String,
    model: String,
    calls: Arc<AtomicUsize>,
}

async fn setup(protocol: Protocol, usage: Value) -> Env {
    dotenvy::dotenv().ok();
    let pg_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let pg = okapi_store::connect_pg(&pg_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("modal-{suffix}");
    let user = okapi_store::provision::create_user(&pg, &format!("modal-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-modal-{suffix}");
    okapi_store::provision::create_api_key(
        &pg,
        user,
        &hex::encode(Sha256::digest(token.as_bytes())),
        "sk-modal",
    )
    .await
    .unwrap();
    let (input, output) = if matches!(protocol, Protocol::Anthropic) {
        ("1", "2")
    } else {
        ("2", "4")
    };
    okapi_store::provision::create_model_ratio(&pg, &model, input, output, "0.5")
        .await
        .unwrap();
    sqlx::query("UPDATE model_pricing SET audio_ratio=8,audio_completion_ratio=2,image_ratio=3,modality_ratios=$2 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&model).bind(json!({"audio_cache_read":"2","image_cache_read":"1","image_output":"5"})).execute(&pg).await.unwrap();
    if matches!(protocol, Protocol::Anthropic) {
        sqlx::query("UPDATE model_pricing SET cache_write_ratio=2 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
            .bind(&model).execute(&pg).await.unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mock = serve(Router::new().fallback(post(upstream)).with_state(Mock {
        protocol,
        usage,
        calls: calls.clone(),
    }))
    .await;
    let (provider, base) = match protocol {
        Protocol::Gemini => ("gemini", "v1beta"),
        Protocol::Anthropic => ("anthropic", "v1"),
        _ => ("openai", "v1"),
    };
    let (channel, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("modal-{suffix}"),
        provider,
        &format!("http://{mock}/up/{base}"),
        "mock",
        &[&model],
        true,
        None,
    )
    .await
    .unwrap();
    sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1")
        .bind(channel)
        .bind(json!({"responses_native":matches!(protocol, Protocol::Responses)}))
        .execute(&pg)
        .await
        .unwrap();
    published_pricing::publish(&pg, user).await;
    let state = gateway::build_state(&pg_url, &redis_url, "modal-test", ch_url.as_deref(), None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user, Money::from_micros(50_000_000))
        .await
        .unwrap();
    let gateway = serve(gateway::router(state.clone())).await;
    let console = serve(console::router(state.clone())).await;
    Env {
        state,
        gateway,
        console,
        user,
        token,
        model,
        calls,
    }
}

async fn request(
    env: &Env,
    protocol: Protocol,
    stream: bool,
    native_gemini: bool,
) -> reqwest::Response {
    let (path, body) = if native_gemini {
        (
            format!(
                "/v1beta/models/{}:{}",
                env.model,
                if stream {
                    "streamGenerateContent"
                } else {
                    "generateContent"
                }
            ),
            json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{"maxOutputTokens":512}}),
        )
    } else {
        match protocol {
            Protocol::Anthropic => (
                "/v1/messages".into(),
                json!({"model":env.model,"messages":[{"role":"user","content":"hi"}],"stream":stream,"max_tokens":512}),
            ),
            Protocol::Responses => (
                "/v1/responses".into(),
                json!({"model":env.model,"input":"hi","stream":stream,"max_output_tokens":512}),
            ),
            Protocol::Chat | Protocol::Gemini => (
                "/v1/chat/completions".into(),
                json!({"model":env.model,"messages":[{"role":"user","content":"hi"}],"stream":stream,"max_tokens":512}),
            ),
        }
    };
    reqwest::Client::new()
        .post(format!("http://{}{path}", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn report(env: &Env, path: &str) -> Value {
    let resp = reqwest::Client::new()
        .get(format!("http://{}{path}", env.console))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body
}

async fn record(env: &Env) -> Value {
    for _ in 0..50 {
        let records = report(env, "/api/me/logs").await;
        if let Some(row) = records["data"].as_array().and_then(|v| v.first()) {
            return row.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("missing settlement");
}

async fn verify(env: &Env) {
    let row = record(env).await;
    assert_eq!(row["amount_micro"], 27_900, "{row}");
    let usage = &row["usage"];
    assert_eq!(
        usage["reported_details"]["prompt"],
        json!({"audio":true,"image":true})
    );
    assert_eq!(
        usage["reported_details"]["completion"],
        json!({"audio":true,"image":true})
    );
    assert_eq!(usage["reported_details"]["reasoning"], true);
    let observation = report(env, "/api/me/logs/stat").await;
    assert_eq!(
        observation["token_detail_observations"]["image_completion_tokens"]["tokens"],
        200
    );
    assert_eq!(
        observation["token_detail_observations"]["image_completion_tokens"]["coverage_bp"],
        10000
    );
    for (name, expected) in [
        ("prompt_tokens", 1000),
        ("completion_tokens", 400),
        ("cached_tokens", 300),
        ("reasoning_tokens", 20),
        ("audio_prompt_tokens", 350),
        ("image_prompt_tokens", 200),
        ("audio_completion_tokens", 100),
        ("image_completion_tokens", 200),
    ] {
        assert_eq!(usage[name], expected, "{name}: {row}");
    }
    assert_eq!(
        usage["cache_read_modalities"],
        json!({"audio_tokens":150,"image_tokens":100})
    );
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - 27_900
    );
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(row["request_id"].as_str().unwrap()).fetch_one(&env.state.pg).await.unwrap();
    for name in [
        "reported_details",
        "prompt_tokens",
        "completion_tokens",
        "cached_tokens",
        "reasoning_tokens",
    ] {
        assert_eq!(payload[name], usage[name]);
    }
    for name in ["amount_micro", "original_amount_micro", "discount_micro"] {
        assert_eq!(payload[name], row[name]);
    }
    if let Some(ch) = &env.state.ch {
        ch.ensure_schema().await.unwrap();
        for _ in 0..100 {
            if okapi::worker::chsink::process_once(&env.state.pg, ch)
                .await
                .unwrap()
                == 0
            {
                break;
            }
        }
        let stats = report(env, "/api/me/stats/breakdown?days=1").await;
        assert_eq!(stats["total"]["requests"], 1);
        assert_eq!(stats["total"]["tokens"], 1400);
        assert_eq!(stats["total"]["cache_hit_bp"], 3000);
    } else {
        eprintln!("SKIP: modal usage CH assertions require OKAPI_CLICKHOUSE_URL");
    }
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn equivalent_protocol_usage_matches_money_ledger_outbox_and_statistics() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Gemini] {
        for stream in [false, true] {
            let env = setup(protocol, protocol.fixture()).await;
            let resp = request(&env, protocol, stream, false).await;
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(status, 200, "{protocol:?} {stream}: {body}");
            assert!(!body.contains("\"code\":\"upstream_error\""), "{body}");
            verify(&env).await;
        }
    }
}

#[tokio::test]
async fn native_gemini_usage_matches_converted_usage() {
    for stream in [false, true] {
        let env = setup(Protocol::Gemini, Protocol::Gemini.fixture()).await;
        let resp = request(&env, Protocol::Gemini, stream, true).await;
        let status = resp.status();
        let body = resp.text().await.unwrap();
        assert_eq!(status, 200, "{body}");
        verify(&env).await;
    }
}

#[tokio::test]
async fn malformed_usage_refunds_without_estimation_or_upstream_replay() {
    for protocol in [Protocol::Chat, Protocol::Responses, Protocol::Gemini] {
        for stream in [false, true] {
            let mut usage = protocol.fixture();
            let key = match protocol {
                Protocol::Chat => "prompt_tokens",
                Protocol::Responses | Protocol::Anthropic => "input_tokens",
                Protocol::Gemini => "promptTokenCount",
            };
            usage[key] = json!(-1);
            let env = setup(protocol, usage).await;
            let resp = request(&env, protocol, stream, false).await;
            let status = resp.status();
            let body = resp.text().await.unwrap();
            assert_eq!(
                status,
                if stream { 200 } else { 502 },
                "{protocol:?}: {body}"
            );
            assert!(body.contains("upstream_error"), "{protocol:?}: {body}");
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 0, "{row}");
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn converted_missing_usage_estimates_but_explicit_zero_remains_zero() {
    for stream in [false, true] {
        for zero in [false, true] {
            for ingress in [Protocol::Chat, Protocol::Responses, Protocol::Gemini] {
                let usage = if zero {
                    json!({"prompt_tokens":0,"completion_tokens":0})
                } else {
                    Value::Null
                };
                let env = setup(Protocol::Chat, usage).await;
                let resp =
                    request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
                let status = resp.status();
                let body = resp.text().await.unwrap();
                assert_eq!(status, 200, "{ingress:?} {stream}: {body}");
                let row = record(&env).await;
                let amount = row["amount_micro"].as_i64().unwrap();
                let input = row["usage"]["prompt_tokens"].as_i64().unwrap();
                if zero {
                    assert_eq!((amount, input), (0, 0));
                } else {
                    assert!(amount > 0 && input > 0, "{row}");
                }
                assert_eq!(env.calls.load(Ordering::SeqCst), 1);
            }
        }
    }
}
