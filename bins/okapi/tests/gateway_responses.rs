//! /v1/responses 验收（§4.4）：
//! - 直转（openai 渠道缺省 / 兼容渠道 `settings.responses_native`）：请求原样到上游 /responses
//!   （previous_response_id / store / include / reasoning 全保住），事件原样透出，usage 取
//!   response.completed；
//! - 降级（#5209）：Responses 请求 → chat 上游 → Responses 事件骨架/对象；两跳
//!   （responses→chat→anthropic）；usage 与计费一致。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[path = "support/published_pricing.rs"]
mod published_pricing;

// ---- mock 上游 ----

async fn mock_chat(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    // 降级语义：instructions → system；input 文本 → user
    assert_eq!(req["messages"][0]["role"], "system");
    assert_eq!(req["messages"][0]["content"], "be helpful");
    assert_eq!(req["messages"][1]["role"], "user");
    assert_eq!(req["max_tokens"], 128, "max_output_tokens 必须映射");
    if req["stream"].as_bool().unwrap_or(false) {
        assert_eq!(req["stream_options"], json!({"include_usage": true}));
        let chunks = [
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"content":"Hello "}}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{"content":"responses"}}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            json!({"id":"c9","object":"chat.completion.chunk","model":"gpt-real","choices":[],
                "usage":{"prompt_tokens":100,"completion_tokens":20,
                    "prompt_tokens_details":{"cached_tokens":0}}}),
        ];
        let mut out = String::new();
        for c in &chunks {
            let _ = write!(out, "data: {c}\n\n");
        }
        out.push_str("data: [DONE]\n\n");
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            out,
        )
            .into_response()
    } else {
        axum::Json(json!({
            "id":"c9","object":"chat.completion","model":"gpt-real",
            "choices":[{"index":0,"finish_reason":"stop",
                "message":{"role":"assistant","content":"Hello responses"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20}
        }))
        .into_response()
    }
}

async fn mock_anthropic(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    // 两跳语义：responses→chat→anthropic，system 抽到顶层
    assert_eq!(req["system"], "be helpful");
    assert!(req["max_tokens"].as_u64().unwrap() >= 128);
    axum::Json(json!({
        "id":"msg_1","type":"message","role":"assistant","model":"claude-real",
        "content":[{"type":"text","text":"Hello responses"}],
        "stop_reason":"end_turn",
        "usage":{"input_tokens":100,"output_tokens":20}
    }))
    .into_response()
}

async fn mock_reasoning(uri: axum::http::Uri, body: axum::body::Bytes) -> axum::response::Response {
    let request: Value = serde_json::from_slice(&body).unwrap();
    let reported = uri.path().contains("reported");
    let reasoning = "thinking ".repeat(32);
    let usage = json!({"prompt_tokens":100,"completion_tokens":20,
        "completion_tokens_details":{"reasoning_tokens":8}});
    if request["stream"] == true {
        let mut chunks = vec![
            json!({"id":"r1","model":"fixture","choices":[{"delta":{"reasoning_content":"thinking ".repeat(16)}}]}),
            json!({"id":"r1","model":"fixture","choices":[{"delta":{"reasoning":"thinking ".repeat(16)}}]}),
            json!({"id":"r1","model":"fixture","choices":[{"delta":{"content":"OK"}}]}),
        ];
        if reported {
            chunks.push(json!({"id":"r1","choices":[],"usage":usage}));
        }
        let mut events = String::new();
        for chunk in chunks {
            let _ = write!(events, "data: {chunk}\n\n");
        }
        events.push_str("data: [DONE]\n\n");
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            events,
        )
            .into_response()
    } else {
        let mut response = json!({"id":"r1","model":"fixture",
            "choices":[{"message":{"role":"assistant","content":"OK","reasoning_content":reasoning}}]});
        if reported {
            response["usage"] = usage;
        }
        axum::Json(response).into_response()
    }
}

/// 原生 Responses 上游：断言请求**原样**（降级链会丢的字段一个都不能少），
/// 回官方形状的事件流 / 对象（含 reasoning item 与 built-in tool 调用）。
async fn mock_native_responses(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer mock-credential"
    );
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["model"], "gpt-upstream", "model_mapping 必须重写");
    assert_eq!(
        req["instructions"], "be helpful",
        "直转不得改写 instructions"
    );
    assert_eq!(req["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(req["previous_response_id"], "resp_prev_1", "续聊链不能丢");
    assert_eq!(req["store"], false);
    assert_eq!(req["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(
        req["tools"][0]["type"], "web_search_preview",
        "内置工具原样"
    );
    assert_eq!(
        req["reasoning"],
        json!({"effort": "high", "summary": "auto"}),
        "reasoning.effort 是原生键，不得被 strip_unified 摘掉"
    );
    assert_eq!(req["max_output_tokens"], 128);
    assert!(
        req.get("messages").is_none() && req.get("stream_options").is_none(),
        "直转不能带 chat 方言字段：{req}"
    );
    let usage = json!({"input_tokens": 100, "output_tokens": 20,
        "input_tokens_details": {"cached_tokens": 40, "cache_write_tokens": 20},
        "output_tokens_details": {"reasoning_tokens": 8}, "total_tokens": 120});
    if req["stream"].as_bool().unwrap_or(false) {
        let events = [
            (
                "response.created",
                json!({"type":"response.created","sequence_number":0,
                "response":{"id":"resp_n1","object":"response","status":"in_progress","model":"gpt-upstream-2026","output":[]}}),
            ),
            (
                "response.output_item.added",
                json!({"type":"response.output_item.added","sequence_number":1,
                "output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[]}}),
            ),
            (
                "response.reasoning_summary_text.delta",
                json!({"type":"response.reasoning_summary_text.delta","sequence_number":2,
                "item_id":"rs_1","output_index":0,"summary_index":0,"delta":"thinking…"}),
            ),
            (
                "response.output_item.done",
                json!({"type":"response.output_item.done","sequence_number":3,
                "output_index":0,"item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"thinking…"}]}}),
            ),
            (
                "response.output_item.added",
                json!({"type":"response.output_item.added","sequence_number":4,
                "output_index":1,"item":{"type":"message","id":"msg_1","status":"in_progress","role":"assistant","content":[]}}),
            ),
            (
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","sequence_number":5,
                "item_id":"msg_1","output_index":1,"content_index":0,"delta":"Hello "}),
            ),
            (
                "response.output_text.delta",
                json!({"type":"response.output_text.delta","sequence_number":6,
                "item_id":"msg_1","output_index":1,"content_index":0,"delta":"native"}),
            ),
            (
                "response.completed",
                json!({"type":"response.completed","sequence_number":7,
                "response":{"id":"resp_n1","object":"response","status":"completed","model":"gpt-upstream-2026",
                    "service_tier":"default",
                    "output":[{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"thinking…"}]},
                              {"type":"message","id":"msg_1","status":"completed","role":"assistant",
                               "content":[{"type":"output_text","text":"Hello native","annotations":[]}]}],
                    "usage": usage}}),
            ),
        ];
        let mut out = String::new();
        for (name, data) in &events {
            let _ = write!(out, "event: {name}\ndata: {data}\n\n");
        }
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            out,
        )
            .into_response()
    } else {
        axum::Json(json!({
            "id":"resp_n1","object":"response","status":"completed","model":"gpt-upstream-2026",
            "output":[{"type":"reasoning","id":"rs_1","summary":[]},
                      {"type":"message","id":"msg_1","status":"completed","role":"assistant",
                       "content":[{"type":"output_text","text":"Hello native","annotations":[]}]}],
            "usage": usage
        }))
        .into_response()
    }
}

fn compact_output() -> Value {
    json!({"id": "cmp-response-1", "object": "response.compaction", "created_at": 1,
        "output": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "retained history ".repeat(1000)}]},
            {"id": "cmp-1", "type": "compaction", "encrypted_content": "opaque-fixture-context", "opaque": {"preserve": true}}
        ],
        "usage": {"input_tokens": 100, "output_tokens": 20,
            "input_tokens_details": {"cached_tokens": 40, "cache_write_tokens": 20},
            "output_tokens_details": {"reasoning_tokens": 8}, "total_tokens": 120}})
}

async fn mock_compact(
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    assert_eq!(headers["authorization"], "Bearer mock-credential");
    assert_eq!(headers["accept"], "application/json");
    if uri.path().starts_with("/codex/") {
        assert_eq!(headers["chatgpt-account-id"], "acct-compact-fixture");
        assert_eq!(headers["originator"], "codex-fixture");
        assert_eq!(headers["openai-beta"], "responses=experimental");
    }
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["model"], "gpt-upstream");
    assert_eq!(req["instructions"], "compact carefully");
    assert_eq!(req["previous_response_id"], "resp_previous");
    assert_eq!(req["input"][0]["arguments"], "{\"q\":\"test\"}");
    assert_eq!(req["input"][1]["encrypted_content"], "opaque-input");
    assert!(req.get("stream").is_none());
    assert!(req.get("messages").is_none() && req.get("stream_options").is_none());
    let mut output = compact_output();
    match headers
        .get("x-fixture-compact")
        .and_then(|v| v.to_str().ok())
    {
        Some("404") => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(json!({"error": {"code": "compact_unavailable"}})),
            )
                .into_response();
        }
        Some("missing-usage") => {
            output.as_object_mut().unwrap().remove("usage");
        }
        Some("empty-usage") => output["usage"] = json!({}),
        Some("missing-ciphertext") => {
            output["output"][1]
                .as_object_mut()
                .unwrap()
                .remove("encrypted_content");
        }
        Some("wrong-object") => output["object"] = json!("response"),
        None => {}
        other => panic!("unknown fixture: {other:?}"),
    }
    axum::Json(output).into_response()
}

type MockCalls = Arc<Mutex<Vec<String>>>;

async fn spawn_mock() -> (SocketAddr, MockCalls) {
    let calls = MockCalls::default();
    let recorded = Arc::clone(&calls);
    let router = Router::new()
        .route("/oai/v1/chat/completions", post(mock_chat))
        .route("/oai/v1/responses", post(mock_native_responses))
        .route("/oai/v1/responses/compact", post(mock_compact))
        .route("/codex/responses/compact", post(mock_compact))
        // 只实现了 chat 的"openai"上游：/responses 由 axum 回 404
        .route("/chatonly/v1/chat/completions", post(mock_chat))
        .route("/reasoning/v1/chat/completions", post(mock_reasoning))
        .route(
            "/reasoning-reported/v1/chat/completions",
            post(mock_reasoning),
        )
        .route("/ant/v1/messages", post(mock_anthropic))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let calls = Arc::clone(&recorded);
                async move {
                    calls.lock().unwrap().push(request.uri().path().to_owned());
                    next.run(request).await
                }
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, calls)
}

// ---- 环境 ----

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
    state: gateway::state::AppState,
    calls: MockCalls,
    channel_id: i64,
}

/// `settings`：渠道 settings 对象（None = 缺省）。直转用例给 openai 渠道配
/// model_mapping → gpt-upstream，验证同方言 model 重写。
async fn setup(provider: &str, path: &str, settings: Option<Value>) -> TestEnv {
    setup_with_cache_write(provider, path, settings, "1").await
}

// 建库 → 种模型与定价 → 建渠道与 key → 起网关是一条线性夹具，拆开只会把同一组局部变量来回传
#[allow(clippy::too_many_lines)]
async fn setup_with_cache_write(
    provider: &str,
    path: &str,
    settings: Option<Value>,
    cache_write_ratio: &str,
) -> TestEnv {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-test-{suffix}");
    let key_hash = key_hash(&token);
    let api_key_id =
        okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-test")
            .await
            .unwrap();
    let model_id = okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE model_pricing SET cache_write_ratio = ($2::text)::numeric WHERE model_id = $1",
    )
    .bind(model_id)
    .bind(cache_write_ratio)
    .execute(&pg)
    .await
    .unwrap();

    let (mock, calls) = spawn_mock().await;
    let credential = if provider == "codex" {
        okapi_store::credential::OAuthCredential {
            access_token: "mock-credential".to_owned(),
            refresh_token: "unused-fixture".to_owned(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            account_id: Some("acct-compact-fixture".to_owned()),
            account_label: None,
        }
        .to_plaintext()
    } else {
        "mock-credential".to_owned()
    };
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        provider,
        &format!("http://{mock}{path}"),
        &credential,
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    let native_opt_in = settings
        .as_ref()
        .and_then(|s| s.get("responses_native"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(settings) = settings {
        sqlx::query("UPDATE channels SET settings = $2 WHERE id = $1")
            .bind(channel_id)
            .bind(settings)
            .execute(&pg)
            .await
            .unwrap();
    }
    // 走直转的渠道配 model_mapping：mock 断言上游收到的 model 是映射名
    if matches!(provider, "openai" | "codex") || native_opt_in {
        sqlx::query("UPDATE channels SET model_mapping = $2 WHERE id = $1")
            .bind(channel_id)
            .bind(json!({ model.as_str(): "gpt-upstream" }))
            .execute(&pg)
            .await
            .unwrap();
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

    seed_protocol_history(&state, &pg, &model, user_id, api_key_id, channel_id).await;

    let app = gateway::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        gateway: addr,
        token,
        user_id,
        model,
        state,
        calls,
        channel_id,
    }
}

fn key_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn seed_protocol_history(
    state: &gateway::state::AppState,
    pg: &PgPool,
    model: &str,
    user_id: i64,
    api_key_id: i64,
    channel_id: i64,
) {
    // 该套件验证既有历史字段的协议透传；真实首轮→续聊在 gateway_response_affinity 验证。
    let candidates = okapi_store::channels::candidates_for_model(pg, model, &["default"], None)
        .await
        .unwrap();
    let candidate = candidates
        .iter()
        .find(|c| c.channel_id == channel_id)
        .unwrap();
    let binding =
        gateway::sched_redis::response_affinity::ResponseBinding::from_candidate(candidate);
    for previous in ["resp_prev_1", "resp_previous"] {
        state
            .sched
            .response_binding_set(user_id, api_key_id, previous, &binding)
            .await
            .unwrap();
    }
}

async fn post_responses(env: &TestEnv, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/responses", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "stream": stream,
            "max_output_tokens": 128,
            "instructions": "be helpful",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi there"}]}]
        }))
        .send()
        .await
        .unwrap()
}

/// Codex CLI 风格请求：续聊 id、不落库、要 encrypted reasoning、内置工具、reasoning 档位。
/// 这些正是降级链会静默丢掉的字段。
async fn post_codex_style(env: &TestEnv, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/responses", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "stream": stream,
            "max_output_tokens": 128,
            "instructions": "be helpful",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi there"}]}],
            "previous_response_id": "resp_prev_1",
            "store": false,
            "include": ["reasoning.encrypted_content"],
            "tools": [{"type": "web_search_preview"}],
            "reasoning": {"effort": "high", "summary": "auto"}
        }))
        .send()
        .await
        .unwrap()
}

async fn wait_record(pg: &PgPool, user_id: i64) -> (i16, i64) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro FROM billing_records
               WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.status, r.amount_micro);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

fn compact_request(env: &TestEnv) -> Value {
    json!({"model": env.model, "instructions": "compact carefully", "stream": false,
    "previous_response_id": "resp_previous", "input": [
        {"type": "function_call", "call_id": "call-fixture", "name": "search", "arguments": "{\"q\":\"test\"}"},
        {"type": "reasoning", "encrypted_content": "opaque-input"}
    ]})
}

async fn post_compact(env: &TestEnv, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/responses/compact", env.gateway))
        .bearer_auth(&env.token)
        .header("originator", "codex-fixture")
        .json(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn compact_preserves_context_and_bills_usage_for_native_compat_and_codex() {
    for (provider, path, settings) in [
        ("openai", "/oai/v1", None),
        (
            "openai_compat",
            "/oai/v1",
            Some(json!({"responses_native": true})),
        ),
        ("codex", "/codex", None),
    ] {
        let env = setup_with_cache_write(provider, path, settings, "2").await;
        let resp = post_compact(&env, &compact_request(&env)).await;
        assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
        let request_id = resp.headers()["x-okapi-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(resp.json::<Value>().await.unwrap(), compact_output());
        assert_eq!(wait_record(&env.pg, env.user_id).await, (20, 280));
        env.state
            .settlements
            .wait_idle(std::time::Duration::from_secs(5))
            .await;
        assert_eq!(
            env.state
                .ledger
                .balance(env.user_id)
                .await
                .unwrap()
                .as_micros(),
            10_000_000 - 280
        );
        let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id' = $1 ORDER BY id DESC LIMIT 1")
            .bind(request_id).fetch_one(&env.pg).await.unwrap();
        assert_eq!(payload["endpoint"], "/v1/responses/compact");
        assert_eq!(payload["upstream_endpoint"], "/v1/responses/compact");
        assert_eq!(
            env.calls.lock().unwrap().as_slice(),
            &[format!("{path}/responses/compact")]
        );
    }
}

#[tokio::test]
async fn compact_rejects_anonymous_and_streaming_requests_before_upstream_or_billing() {
    let env = setup("openai", "/oai/v1", None).await;
    let mut body = compact_request(&env);
    let anonymous = reqwest::Client::new()
        .post(format!("http://{}/v1/responses/compact", env.gateway))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    body["stream"] = json!(true);
    let resp = post_compact(&env, &body).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["param"],
        "stream"
    );
    assert_eq!(
        post_compact(&env, &json!({"input": "missing model"}))
            .await
            .status(),
        400
    );
    assert!(env.calls.lock().unwrap().is_empty());
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        10_000_000
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM billing_records WHERE user_id = $1")
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn compact_never_downgrades_and_refunds_unusable_upstream_results() {
    for (case, expected) in [
        ("404", 404),
        ("missing-usage", 502),
        ("empty-usage", 502),
        ("missing-ciphertext", 502),
        ("wrong-object", 502),
    ] {
        let env = setup(
            "openai",
            "/oai/v1",
            Some(json!({"extra_headers": {"x-fixture-compact": case}})),
        )
        .await;
        let resp = post_compact(&env, &compact_request(&env)).await;
        assert_eq!(
            resp.status(),
            expected,
            "case {case}: {}",
            resp.text().await.unwrap()
        );
        assert_eq!(
            env.state
                .ledger
                .balance(env.user_id)
                .await
                .unwrap()
                .as_micros(),
            10_000_000
        );
        let rows: Vec<(i16, i64)> =
            sqlx::query_as("SELECT status, amount_micro FROM billing_records WHERE user_id = $1")
                .bind(env.user_id)
                .fetch_all(&env.pg)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![(40, 0)],
            "case {case}: failure must close the reservation"
        );
        let calls = env.calls.lock().unwrap();
        assert!(!calls.is_empty());
        assert!(
            calls.iter().all(|p| p == "/oai/v1/responses/compact"),
            "{calls:?}"
        );
        if case == "404" {
            assert_eq!(
                calls.len(),
                1,
                "unsupported compact must not become a chat request"
            );
        }
    }
}

#[tokio::test]
async fn compact_filters_incompatible_and_explicitly_disabled_channels() {
    for disabled in [false, true] {
        let env = setup(
            if disabled { "openai" } else { "openai_compat" },
            "/oai/v1",
            None,
        )
        .await;
        if disabled {
            sqlx::query(
                "UPDATE channels SET capabilities = '{\"compact\": false}'::jsonb WHERE id = $1",
            )
            .bind(env.channel_id)
            .execute(&env.pg)
            .await
            .unwrap();
            env.state.invalidate_routing_caches_local();
        }
        // Shared ingress rules distinguish a configured but incompatible endpoint
        // from an unavailable channel. Keep the error actionable without retrying.
        let response = post_compact(&env, &compact_request(&env)).await;
        assert_eq!(response.status(), 400);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "unsupported_endpoint");
        assert_eq!(
            error["error"]["param"],
            "/v1/chat/completions,/v1/responses,/v1/messages,/v1beta/models/{model}:generateContent"
        );
        assert!(Uuid::parse_str(error["error"]["request_id"].as_str().unwrap()).is_ok());
        assert!(env.calls.lock().unwrap().is_empty());

        // Once the channel itself is disabled, switching endpoints cannot help.
        sqlx::query("UPDATE channels SET status=2 WHERE id=$1")
            .bind(env.channel_id)
            .execute(&env.pg)
            .await
            .unwrap();
        env.state.invalidate_routing_caches_local();
        let unavailable = post_compact(&env, &compact_request(&env)).await;
        assert_eq!(unavailable.status(), 503);
        let error: Value = unavailable.json().await.unwrap();
        assert_eq!(error["error"]["code"], "no_available_channel");
        assert!(env.calls.lock().unwrap().is_empty());
        assert_eq!(
            env.state
                .ledger
                .balance(env.user_id)
                .await
                .unwrap()
                .as_micros(),
            10_000_000
        );
        let paid: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM billing_records WHERE user_id=$1 AND amount_micro<>0",
        )
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
        assert_eq!(paid, 0, "neither rejection may charge the caller");
    }
}

fn parse_named_events(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut event: Option<String> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("event: ") {
            event = Some(name.to_owned());
        } else if let Some(data) = line.strip_prefix("data: ")
            && let Some(name) = event.take()
            && let Ok(v) = serde_json::from_str::<Value>(data)
        {
            out.push((name, v));
        }
    }
    out
}

/// 直转·流式：上游事件原样透出（reasoning item、sequence_number 保留，无合成骨架、无 [DONE]），
/// usage 取 response.completed（含 cached / reasoning 细分），计费一致。
#[tokio::test]
async fn native_stream_passthrough_and_billing() {
    let env = setup("openai", "/oai/v1", None).await;
    let resp = post_codex_style(&env, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let events = parse_named_events(&text);
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "response.created",
            "response.output_item.added",
            "response.reasoning_summary_text.delta",
            "response.output_item.done",
            "response.output_item.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.completed",
        ],
        "直转必须原样透出上游事件序列：{text}"
    );
    assert!(!text.contains("[DONE]"), "Responses 出口无 [DONE]");
    // 原文透出：上游的 sequence_number / reasoning item 不被改写
    assert_eq!(events[0].1["sequence_number"], 0);
    assert_eq!(events[3].1["item"]["type"], "reasoning");
    let (_, completed) = events
        .iter()
        .find(|(n, _)| n == "response.completed")
        .unwrap();
    assert_eq!(completed["response"]["model"], "gpt-upstream-2026");
    assert_eq!(
        completed["response"]["usage"]["input_tokens_details"]["cached_tokens"],
        40
    );

    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    // 普通输入 40、缓存读 40、缓存写 20 同价 → (100+20)×$2/1M = 240
    assert_eq!(
        amount, 240,
        "usage 必须来自 response.completed 而非字符估算"
    );
}

/// 直转·非流式：Responses 对象原样返回（reasoning item 在 output 里），计费一致。
#[tokio::test]
async fn native_json_passthrough() {
    let env = setup("openai", "/oai/v1", None).await;
    let resp = post_codex_style(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "resp_n1", "对象原样：id 不得被改写成合成值");
    assert_eq!(body["output"][0]["type"], "reasoning");
    assert_eq!(body["output"][1]["content"][0]["text"], "Hello native");
    assert_eq!(
        body["usage"]["output_tokens_details"]["reasoning_tokens"],
        8
    );

    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
}

#[tokio::test]
async fn native_cache_write_tokens_are_billed_in_json_and_sse() {
    for stream in [false, true] {
        let env = setup_with_cache_write("openai", "/oai/v1", None, "2").await;
        let response = post_codex_style(&env, stream).await;
        assert_eq!(response.status(), 200);
        let body = response.text().await.unwrap();
        let usage = if stream {
            parse_named_events(&body)
                .into_iter()
                .find(|(name, _)| name == "response.completed")
                .expect("terminal event")
                .1["response"]["usage"]
                .clone()
        } else {
            serde_json::from_str::<Value>(&body).unwrap()["usage"].clone()
        };
        assert_eq!(usage["input_tokens_details"]["cache_write_tokens"], 20);
        let (status, amount) = wait_record(&env.pg, env.user_id).await;
        assert_eq!(status, 20);
        // 输入总计 100 已含缓存段：普通 40 + 读 40 + 写 20×2 + 输出 20。
        assert_eq!(
            amount, 280,
            "stream={stream}: 缓存写入不能被当成普通输入或重复相加"
        );
        let snapshot: Value = sqlx::query_scalar(
            "SELECT pricing_snapshot FROM billing_records WHERE user_id = $1 AND log_type = 2",
        )
        .bind(env.user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
        assert_eq!(snapshot["cache_write_ratio"], 2);
    }
}

/// openai 渠道显式 `responses_native:false` → 回到降级链（上游只实现了 chat 的"openai"渠道）。
#[tokio::test]
async fn openai_channel_can_opt_out_of_native() {
    let env = setup(
        "openai",
        "/oai/v1",
        Some(json!({"responses_native": false})),
    )
    .await;
    let resp = post_responses(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "resp_c9", "降级链合成的对象 id 带 resp_ 前缀");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello responses");
}

/// openai 渠道指着只实现 chat 的上游：直转 404 → 同候选就地改走降级链，
/// 客户端拿到正常 Responses 对象，账单 failover_count 仍为 0（渠道没坏，是方言不对）。
#[tokio::test]
async fn native_404_falls_back_to_downgrade_on_same_channel() {
    let env = setup("openai", "/chatonly/v1", None).await;
    let resp = post_responses(&env, false).await;
    assert_eq!(resp.status(), 200, "404 不该透给客户端");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello responses");
    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
    let row = sqlx::query!(
        r#"SELECT failover_count, request_id FROM billing_records
           WHERE user_id = $1 AND log_type = 2"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(row.failover_count, 0, "同候选换方言不算 failover");
    let payload: Value = sqlx::query_scalar!(
        r#"SELECT payload FROM billing_outbox WHERE payload->>'request_id' = $1 ORDER BY id DESC LIMIT 1"#,
        row.request_id.to_string()
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        payload["upstream_endpoint"], "/v1/chat/completions",
        "账单维度记的是实际打到的上游端点"
    );

    // 流式同样兜底
    let resp = post_responses(&env, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("event: response.completed"), "{text}");
}

/// 兼容渠道显式 `responses_native:true` → 直转（上游是另一台支持 Responses 的网关）。
#[tokio::test]
async fn compat_channel_can_opt_in_to_native() {
    let env = setup(
        "openai_compat",
        "/oai/v1",
        Some(json!({"responses_native": true})),
    )
    .await;
    let resp = post_codex_style(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "resp_n1");
    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
}

/// 降级·流式（兼容渠道缺省）：Responses 事件骨架 + usage + 计费。
#[tokio::test]
async fn responses_stream_skeleton_and_billing() {
    let env = setup("openai_compat", "/oai/v1", None).await;
    let resp = post_responses(&env, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let events = parse_named_events(&text);
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "response.created",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ],
        "必须合成完整 Responses 事件骨架：{text}"
    );
    assert!(!text.contains("[DONE]"), "Responses 出口无 [DONE]");
    let deltas: String = events
        .iter()
        .filter(|(n, _)| n == "response.output_text.delta")
        .filter_map(|(_, d)| d["delta"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(deltas, "Hello responses");
    let (_, completed) = events
        .iter()
        .find(|(n, _)| n == "response.completed")
        .unwrap();
    assert_eq!(completed["response"]["status"], "completed");
    assert_eq!(completed["response"]["usage"]["input_tokens"], 100);
    assert_eq!(completed["response"]["usage"]["output_tokens"], 20);
    assert_eq!(
        completed["response"]["output"][0]["content"][0]["text"],
        "Hello responses"
    );

    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240, "(100+20)×1×$2/1M");
}

/// 降级·非流式：Responses 对象 + 计费。
#[tokio::test]
async fn responses_json_object() {
    let env = setup("openai_compat", "/oai/v1", None).await;
    let resp = post_responses(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["type"], "message");
    assert_eq!(
        body["output"][0]["content"][0],
        json!({"type": "output_text", "text": "Hello responses", "annotations": []})
    );
    assert_eq!(body["usage"]["input_tokens"], 100);

    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
}

/// 两跳：Responses 入口 + anthropic 渠道（responses→chat→anthropic→回程）。
#[tokio::test]
async fn responses_over_anthropic_two_hops() {
    let env = setup("anthropic", "/ant/v1", None).await;
    let resp = post_responses(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello responses");
    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
}

#[tokio::test]
async fn reasoning_downgrade_preserves_output_and_usage_in_json_and_sse() {
    for reported in [false, true] {
        let mut receipts = Vec::new();
        for stream in [false, true] {
            let path = if reported {
                "/reasoning-reported/v1"
            } else {
                "/reasoning/v1"
            };
            let env = setup("openai_compat", path, None).await;
            // Authoritative usage wins on trusted channels; untrusted recount has its own suite.
            sqlx::query("UPDATE channels SET trust_upstream_usage=true WHERE id=$1")
                .bind(env.channel_id)
                .execute(&env.pg)
                .await
                .unwrap();
            env.state.invalidate_routing_caches_local();
            let response = post_responses(&env, stream).await;
            assert_eq!(response.status(), 200);
            let body = response.text().await.unwrap();
            let output = if stream {
                let events = parse_named_events(&body);
                let deltas: String = events
                    .iter()
                    .filter(|(name, _)| name == "response.reasoning_summary_text.delta")
                    .filter_map(|(_, event)| event["delta"].as_str())
                    .collect();
                assert_eq!(deltas, "thinking ".repeat(32));
                events
                    .into_iter()
                    .find(|(name, _)| name == "response.completed")
                    .unwrap()
                    .1["response"]
                    .clone()
            } else {
                serde_json::from_str::<Value>(&body).unwrap()
            };
            assert_eq!(
                output["output"][0]["summary"][0]["text"],
                "thinking ".repeat(32)
            );
            assert_eq!(output["output"][1]["content"][0]["text"], "OK");
            assert_eq!(wait_record(&env.pg, env.user_id).await.0, 20);
            let receipt: (i32, i32, i64) = sqlx::query_as(
                "SELECT prompt_tokens,completion_tokens,amount_micro FROM billing_records WHERE user_id=$1 AND log_type=2"
            ).bind(env.user_id).fetch_one(&env.pg).await.unwrap();
            if reported {
                assert_eq!(
                    receipt,
                    (100, 20, 240),
                    "real usage must win over summary estimation"
                );
            } else {
                assert!(
                    receipt.1 >= 43,
                    "288 reasoning characters cannot be billed as two body characters: {receipt:?}"
                );
                assert_eq!(receipt.2, i64::from(receipt.0 + receipt.1) * 2);
                assert!(output["usage"].is_null());
            }
            assert_eq!(
                env.calls.lock().unwrap().as_slice(),
                &[format!("{path}/chat/completions")]
            );
            receipts.push(receipt);
        }
        assert_eq!(
            receipts[0], receipts[1],
            "JSON and SSE must count the same reasoning exactly once"
        );
    }
}
