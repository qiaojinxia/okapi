//! M3 reasoning 后缀 + thinking-to-content 验收：
//! `-high` 注入 openai reasoning_effort、`-thinking-N` 注入 anthropic thinking、
//! 计费落在基名模型；t2c 渠道开关把 reasoning 转 <think> 正文。
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
use uuid::Uuid;

// ---- mock 上游 ----

async fn mock_openai_reasoning(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    // 后缀已剥：model 是基名；effort 已注入
    assert!(!req["model"].as_str().unwrap().ends_with("-high"));
    assert_eq!(req["reasoning_effort"], "high");
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":20}
    }))
    .into_response()
}

async fn mock_anthropic_thinking(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["thinking"]["type"], "enabled");
    assert_eq!(req["thinking"]["budget_tokens"], 2048);
    assert!(
        req["max_tokens"].as_u64().unwrap() > 2048,
        "max_tokens 必须大于思考预算"
    );
    axum::Json(json!({
        "id":"msg_1","type":"message","role":"assistant","model":"claude-real",
        "content":[{"type":"text","text":"ok"}],
        "stop_reason":"end_turn",
        "usage":{"input_tokens":100,"output_tokens":20}
    }))
    .into_response()
}

async fn mock_openai_t2c(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    if req["stream"].as_bool().unwrap_or(false) {
        let chunks = [
            json!({"id":"c","object":"chat.completion.chunk",
                "choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"id":"c","object":"chat.completion.chunk",
                "choices":[{"index":0,"delta":{"reasoning_content":"pondering"}}]}),
            json!({"id":"c","object":"chat.completion.chunk",
                "choices":[{"index":0,"delta":{"content":"final answer"}}]}),
            json!({"id":"c","object":"chat.completion.chunk","choices":[],
                "usage":{"prompt_tokens":100,"completion_tokens":20}}),
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
            "id":"cmpl","object":"chat.completion",
            "choices":[{"index":0,"message":{"role":"assistant",
                "content":"final answer","reasoning_content":"pondering"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20}
        }))
        .into_response()
    }
}

async fn mock_openai_fullname(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    // 全名直命中：不剥后缀、不注入 effort
    assert!(req["model"].as_str().unwrap().ends_with("-high"));
    assert!(req.get("reasoning_effort").is_none());
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":20}
    }))
    .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/oai/v1/chat/completions", post(mock_openai_reasoning))
        .route("/oai-full/v1/chat/completions", post(mock_openai_fullname))
        .route("/ant/v1/messages", post(mock_anthropic_thinking))
        .route("/t2c/v1/chat/completions", post(mock_openai_t2c));
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
    state: gateway::state::AppState,
}

/// provider + base_path + 渠道 settings。
async fn setup(provider: &str, path: &str, settings: Value) -> TestEnv {
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
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        provider,
        &format!("http://{mock}{path}"),
        "mock-credential",
        &[model.as_str()],
        false,
    )
    .await
    .unwrap();
    if !settings.as_object().is_none_or(serde_json::Map::is_empty) {
        sqlx::query!(
            "UPDATE channels SET settings = $2 WHERE id = $1",
            channel_id,
            settings
        )
        .execute(&pg)
        .await
        .unwrap();
    }

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();

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
    }
}

async fn post_chat(env: &TestEnv, model: &str, stream: bool) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": model,
            "stream": stream,
            "max_tokens": 64,
            "messages": [{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap()
}

async fn wait_record_model(pg: &PgPool, user_id: i64) -> (String, i64) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT model_name, amount_micro FROM billing_records
               WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.model_name, r.amount_micro);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

/// `-high` 后缀：openai 渠道注入 reasoning_effort，计费落基名。
#[tokio::test]
async fn suffix_high_injects_effort_and_bills_base_model() {
    let env = setup("openai", "/oai/v1", json!({})).await;
    let resp = post_chat(&env, &format!("{}-high", env.model), false).await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);

    let (billed_model, amount) = wait_record_model(&env.pg, env.user_id).await;
    assert_eq!(billed_model, env.model, "计费必须落在基名模型");
    assert_eq!(amount, 240, "(100+20)×1×$2/1M");
}

/// `-thinking-2048`：anthropic 渠道注入 thinking 预算并抬高 max_tokens。
#[tokio::test]
async fn suffix_thinking_budget_on_anthropic() {
    let env = setup("anthropic", "/ant/v1", json!({})).await;
    let resp = post_chat(&env, &format!("{}-thinking-2048", env.model), false).await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let (billed_model, _) = wait_record_model(&env.pg, env.user_id).await;
    assert_eq!(billed_model, env.model);
}

/// 后缀模型若真实存在（模型表直命中），不做剥离——全名优先。
#[tokio::test]
async fn real_model_with_suffix_like_name_wins() {
    let env = setup("openai", "/oai-full/v1", json!({})).await;
    // 建一个真实存在的 "<model>-high" 模型（指向同渠道）
    let full = format!("{}-high", env.model);
    okapi_store::provision::create_model_ratio(&env.pg, &full, "2", "1", "1")
        .await
        .unwrap();
    sqlx::query!(
        r#"UPDATE channels SET models = models || $2::jsonb WHERE models @> $1::jsonb"#,
        json!([env.model]),
        json!([full])
    )
    .execute(&env.pg)
    .await
    .unwrap();
    // 启动后新增的模型要经 epoch 发布 + PriceBook 热更（正规路径）
    okapi_store::admin::publish_epoch(&env.pg, env.user_id, &json!({"reason": "test"}))
        .await
        .unwrap();
    assert!(
        gateway::refresh_pricebook_if_newer(&env.state)
            .await
            .unwrap(),
        "PriceBook 应热更"
    );

    let resp = post_chat(&env, &full, false).await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let (billed_model, amount) = wait_record_model(&env.pg, env.user_id).await;
    assert_eq!(billed_model, full, "全名直命中优先，禁止剥后缀");
    assert_eq!(amount, 480, "按全名模型的 2 倍率计费：(100+20)×2×$2/1M");
}

/// t2c 渠道开关：流式 reasoning 转 <think> 正文；关闭时原样透传。
#[tokio::test]
async fn thinking_to_content_channel_toggle() {
    let env = setup("openai", "/t2c/v1", json!({"thinking_to_content": true})).await;
    let resp = post_chat(&env, &env.model, true).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(
        !text.contains("reasoning_content"),
        "开关开启后不得出现 reasoning_content：{text}"
    );
    let content: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|c| {
            c.pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(content, "<think>\npondering\n</think>\nfinal answer");

    // 非流式：<think> 前缀
    let resp = post_chat(&env, &env.model, false).await;
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["choices"][0]["message"]["content"],
        "<think>\npondering\n</think>\nfinal answer"
    );
    assert!(
        body["choices"][0]["message"]
            .get("reasoning_content")
            .is_none()
    );
}
