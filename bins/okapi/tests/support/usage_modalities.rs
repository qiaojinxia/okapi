//! Harness shared by `gateway_usage_modalities` and `gateway_server_tool_statistics`: the mock upstream for every
//! protocol (JSON and SSE), the per-test user / key / model / channel / published price, and the request / report /
//! settlement-record helpers. No tests live here, so each suite runs its own cases exactly once.
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug)]
pub(super) enum Protocol {
    Chat,
    Responses,
    Gemini,
    Anthropic,
}

impl Protocol {
    pub(super) fn fixture(self) -> Value {
        let all: Value = serde_json::from_str(include_str!(
            "../../../../crates/okapi-providers/tests/fixtures/multimodal_usage.json"
        ))
        .unwrap();
        all[match self {
            Self::Chat => "chat",
            Self::Responses => "responses",
            Self::Gemini => "gemini",
            Self::Anthropic => return anthropic_fixture(),
        }]
        .clone()
    }
}

#[derive(Default)]
pub(super) struct CostGate {
    pub(super) entered: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[derive(Clone)]
struct Mock {
    cost_gate: Option<Arc<CostGate>>,
    protocol: Protocol,
    usage: Value,
    calls: Arc<AtomicUsize>,
    captured: Arc<tokio::sync::Mutex<Vec<(Value, i64)>>>,
    ledger: okapi_ledger::BalanceLedger,
    user: i64,
}

async fn upstream(
    State(mock): State<Mock>,
    uri: Uri,
    Json(body): Json<Value>,
) -> axum::response::Response {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    mock.captured.lock().await.push((
        body.clone(),
        mock.ledger.balance(mock.user).await.unwrap().as_micros(),
    ));
    if let Some(gate) = &mock.cost_gate {
        gate.entered.notify_one();
        tokio::time::timeout(Duration::from_secs(15), gate.release.notified())
            .await
            .unwrap();
    }
    let stream = body["stream"] == true || uri.path().ends_with(":streamGenerateContent");
    let (json, events) = match mock.protocol {
        Protocol::Chat => {
            assert_eq!(uri.path(), "/up/v1/chat/completions");
            let usage = mock.usage.get("final").unwrap_or(&mock.usage);
            let mut body = json!({"id":"c","object":"chat.completion","model":"fixture","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":usage});
            if let Some(envelope) = mock.usage.get("envelope").and_then(Value::as_object) {
                for (key, value) in envelope {
                    body[key] = value.clone();
                }
            }
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
            if let Some(envelopes) = mock.usage.get("envelope_updates").and_then(Value::as_array) {
                for envelope in envelopes {
                    writeln!(events, "data: {envelope}\n").unwrap();
                }
            } else {
                for usage in updates {
                    writeln!(events, "data: {}\n", json!({"choices":[],"usage":usage})).unwrap();
                }
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
            anthropic_response(&mock.usage)
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

pub(super) struct Env {
    pub(super) state: gateway::state::AppState,
    pub(super) gateway: SocketAddr,
    pub(super) console: SocketAddr,
    pub(super) user: i64,
    pub(super) token: String,
    pub(super) model: String,
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) captured: Arc<tokio::sync::Mutex<Vec<(Value, i64)>>>,
    /// ClickHouse is the suite's shared database rather than one made for this case.
    shared_clickhouse: bool,
}

impl Env {
    /// This case's ClickHouse client, with the statistics schema in place.
    ///
    /// `ensure_schema` is idempotent, but every call re-sends about 130 DDL statements that ClickHouse analyzes again
    /// (about half a second here, more on CI runners), and the cases ask for it after each ingress and stream. The
    /// shared database gets it once per test process: nothing drops that database while a suite runs.
    pub(super) async fn clickhouse(&self) -> &okapi_store::ChClient {
        static SHARED_APPLIED: AtomicBool = AtomicBool::new(false);
        let ch = self
            .state
            .ch
            .as_ref()
            .expect("isolated ClickHouse is required");
        if !self.shared_clickhouse || !SHARED_APPLIED.load(Ordering::Acquire) {
            ch.ensure_schema().await.unwrap();
            if self.shared_clickhouse {
                SHARED_APPLIED.store(true, Ordering::Release);
            }
        }
        ch
    }
}

pub(super) async fn setup(protocol: Protocol, usage: Value) -> Env {
    setup_with_pricing(protocol, usage, None).await
}

pub(super) async fn setup_with_pricing(
    protocol: Protocol,
    usage: Value,
    cache_rates: Option<(&str, Value)>,
) -> Env {
    setup_with_pricing_and_cost_gate(protocol, usage, cache_rates, None).await
}

pub(super) async fn setup_with_pricing_and_cost_gate(
    protocol: Protocol,
    usage: Value,
    cache_rates: Option<(&str, Value)>,
    cost_gate: Option<Arc<CostGate>>,
) -> Env {
    setup_inner(protocol, usage, cache_rates, cost_gate, None).await
}

pub(super) async fn setup_with_ch_database(usage: Value, database: &str) -> Env {
    setup_inner(Protocol::Anthropic, usage, None, None, Some(database)).await
}

/// Builds on the pool `setup_inner` already migrated and seeded, like the worker does, instead of opening and
/// migrating a second one.
async fn fixture_state(
    pg: sqlx::PgPool,
    redis_url: &str,
    ch_url: Option<&str>,
    database: Option<&str>,
) -> gateway::state::AppState {
    let redis = okapi_store::connect_redis(redis_url).await.unwrap();
    let mut state = gateway::build_state_with_resources(pg, redis, "modal-test", ch_url, None)
        .await
        .unwrap();
    if let Some(database) = database {
        state.ch = Some(okapi_store::ChClient::new(ch_url.unwrap(), database).unwrap());
    }
    state
}

async fn setup_inner(
    protocol: Protocol,
    usage: Value,
    cache_rates: Option<(&str, Value)>,
    cost_gate: Option<Arc<CostGate>>,
    database: Option<&str>,
) -> Env {
    okapi_store::test_support::assert_isolated();
    let pg_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let ch_url = okapi_store::test_support::optional_service("OKAPI_CLICKHOUSE_URL");
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
    if let Some((write_ratio, rates)) = cache_rates {
        sqlx::query("UPDATE model_pricing SET cache_write_ratio=$2::text::numeric,modality_ratios=$3 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
            .bind(&model).bind(write_ratio).bind(rates).execute(&pg).await.unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let mock = serve(Router::new().fallback(post(upstream)).with_state(Mock {
        cost_gate,
        protocol,
        usage,
        calls: calls.clone(),
        captured: captured.clone(),
        ledger: okapi_ledger::BalanceLedger::new(
            okapi_store::connect_redis(&redis_url).await.unwrap(),
        ),
        user,
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
    super::published_pricing::publish(&pg, user).await;
    let state = fixture_state(pg, &redis_url, ch_url.as_deref(), database).await;
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
        captured,
        shared_clickhouse: database.is_none(),
    }
}

pub(super) async fn request(
    env: &Env,
    protocol: Protocol,
    stream: bool,
    native_gemini: bool,
) -> reqwest::Response {
    request_with_tools(env, protocol, stream, native_gemini, None).await
}

pub(super) async fn request_with_tools(
    env: &Env,
    protocol: Protocol,
    stream: bool,
    native_gemini: bool,
    tools: Option<Value>,
) -> reqwest::Response {
    let (path, mut body) = if native_gemini {
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
    if let Some(tools) = tools {
        body["tools"] = tools;
    }
    reqwest::Client::new()
        .post(format!("http://{}{path}", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

pub(super) async fn report(env: &Env, path: &str) -> Value {
    let resp = reqwest::Client::new()
        .get(format!("http://{}{path}", env.console))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "{path}: {body}");
    body
}

pub(super) async fn record(env: &Env) -> Value {
    for _ in 0..50 {
        let records = report(env, "/api/me/logs").await;
        if let Some(row) = records["data"].as_array().and_then(|v| v.first()) {
            return row.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("missing settlement");
}

pub(super) fn anthropic_fixture() -> Value {
    json!({"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":800,
        "cache_creation_input_tokens":100,"cache_creation":{"ephemeral_5m_input_tokens":100,"ephemeral_1h_input_tokens":0},
        "output_tokens_details":{"thinking_tokens":20}})
}

fn anthropic_response(raw: &Value) -> (Value, String) {
    let usage = raw.get("final").unwrap_or(raw);
    let model = raw
        .get("response_model")
        .and_then(Value::as_str)
        .unwrap_or("fixture");
    let mut body = json!({"id":"msg_usage","type":"message","role":"assistant","model":model,
        "content":[{"type":"text","text":"Hello"}],"stop_reason":"end_turn","usage":usage});
    if let Some(content) = raw.get("content") {
        body["content"] = content.clone();
    }
    let start = raw.get("start").unwrap_or(usage);
    let updates = raw
        .get("updates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![usage.clone()]);
    let mut events = format!(
        "event: message_start\ndata: {}\n\nevent: content_block_delta\ndata: {}\n\n",
        json!({"type":"message_start","message":{"id":"msg_usage","type":"message","role":"assistant","model":model,"content":[],"usage":start}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}})
    );
    if let Some(content) = raw.get("content").and_then(Value::as_array) {
        for (index, block) in content.iter().enumerate() {
            write!(
                events,
                "event: content_block_start\ndata: {}\n\nevent: content_block_stop\ndata: {}\n\n",
                json!({"type":"content_block_start","index":index,"content_block":block}),
                json!({"type":"content_block_stop","index":index})
            )
            .unwrap();
        }
    }
    for usage in updates {
        write!(
            events,
            "event: message_delta\ndata: {}\n\n",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":usage})
        )
        .unwrap();
    }
    events.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    (body, events)
}
