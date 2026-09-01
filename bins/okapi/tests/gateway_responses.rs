//! M3 /v1/responses 降级验收（§4.4 #5209）：
//! Responses 请求 → chat 上游 → Responses 事件骨架/对象；两跳（responses→chat→anthropic）；
//! usage 与计费一致。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::net::SocketAddr;
use uuid::Uuid;

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

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/oai/v1/chat/completions", post(mock_chat))
        .route("/ant/v1/messages", post(mock_anthropic));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

// ---- 环境 ----

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(provider: &str, path: &str) -> TestEnv {
    dotenvy::dotenv().ok();
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
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        provider,
        &format!("http://{mock}{path}"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
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
        pg,
        gateway: addr,
        token,
        user_id,
        model,
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

/// 流式：Responses 事件骨架 + usage + 计费。
#[tokio::test]
async fn responses_stream_skeleton_and_billing() {
    let env = setup("openai", "/oai/v1").await;
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

/// 非流式：Responses 对象 + 计费。
#[tokio::test]
async fn responses_json_object() {
    let env = setup("openai", "/oai/v1").await;
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
    let env = setup("anthropic", "/ant/v1").await;
    let resp = post_responses(&env, false).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["output"][0]["content"][0]["text"], "Hello responses");
    let (status, amount) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
}
