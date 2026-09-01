//! 按上游响应模型计费验收（Sub2API 0.1.175 对齐，渠道 opt-in）：
//! 响应模型有价 → 按其倍率计费并记账；无价 → 回退请求 canonical；
//! 未开开关 → 不受响应模型影响。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

/// mock 上游：响应恒报告 model = 请求 model 前加 "up-" 前缀（模拟映射改名）。
async fn mock_chat(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    let upstream_model = format!("up-{}", req["model"].as_str().unwrap());
    if req["stream"].as_bool().unwrap_or(false) {
        use std::fmt::Write as _;
        let chunks = [
            json!({"model": upstream_model, "choices":[{"index":0,"delta":{"role":"assistant"}}]}),
            json!({"model": upstream_model, "choices":[{"index":0,"delta":{"content":"hi"}}]}),
            json!({"model": upstream_model, "choices":[],
                   "usage":{"prompt_tokens":100,"completion_tokens":20,
                            "prompt_tokens_details":{"cached_tokens":0}}}),
        ];
        let mut out = String::new();
        for c in chunks {
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
            "id":"cmpl","object":"chat.completion","model": upstream_model,
            "choices":[{"index":0,"message":{"role":"assistant","content":"hi"}}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,
                     "prompt_tokens_details":{"cached_tokens":0}}
        }))
        .into_response()
    }
}

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

/// 请求模型倍率 1.0/1.0；`up-<model>` 若 seed_upstream_price 则 3.0/1.0（金额差 3 倍）。
async fn setup(bill_by_response_model: bool, seed_upstream_price: bool) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("rm-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-rm-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-rm")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    if seed_upstream_price {
        okapi_store::provision::create_model_ratio(
            &pg,
            &format!("up-{model}"),
            "3.0",
            "1.0",
            "1.0",
        )
        .await
        .unwrap();
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    let router = Router::new().route("/v1/chat/completions", post(mock_chat));
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("rm-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        true, // trust_upstream_usage
        None,
    )
    .await
    .unwrap();
    if bill_by_response_model {
        sqlx::query!(
            r#"UPDATE channels
               SET settings = COALESCE(settings, '{}'::jsonb)
                   || '{"bill_by_response_model": true}'::jsonb
               WHERE id = $1"#,
            channel_id
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
        .credit(user_id, Money::from_micros(50_000_000))
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

async fn chat(env: &TestEnv, stream: bool) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "stream": stream,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();
}

/// 首条 committed 记录 (model_name, amount_micro)。
async fn wait_committed(pg: &PgPool, user_id: i64) -> (String, i64) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT model_name, amount_micro FROM billing_records
               WHERE user_id = $1 AND status = 20"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.model_name, r.amount_micro);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("未等到 committed 记录");
}

/// 开关开 + 响应模型有价：非流式按响应模型倍率（3×）计费并记账。
#[tokio::test]
async fn bills_by_response_model_when_priced_json() {
    let env = setup(true, true).await;
    chat(&env, false).await;
    let (model_name, amount) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(model_name, format!("up-{}", env.model), "应记响应模型");
    // 1.0 倍率基线：100 prompt + 20 completion = 240 micro；3.0 倍率 = 720
    assert_eq!(amount, 720, "应按响应模型 3.0 倍率计费");
}

/// 开关开 + 响应模型有价：流式同语义（chunk model 采集）。
#[tokio::test]
async fn bills_by_response_model_when_priced_stream() {
    let env = setup(true, true).await;
    chat(&env, true).await;
    let (model_name, amount) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(model_name, format!("up-{}", env.model));
    assert_eq!(amount, 720);
}

/// 开关开 + 响应模型无价：回退请求 canonical（fail-open 不拒付）。
#[tokio::test]
async fn falls_back_when_response_model_unpriced() {
    let env = setup(true, false).await;
    chat(&env, false).await;
    let (model_name, amount) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(model_name, env.model, "无价响应模型应回退 canonical");
    assert_eq!(amount, 240, "金额维持请求模型口径");
}

/// 开关关：即便响应模型有价也不受影响（缺省行为不变）。
#[tokio::test]
async fn disabled_flag_keeps_canonical() {
    let env = setup(false, true).await;
    chat(&env, false).await;
    let (model_name, amount) = wait_committed(&env.pg, env.user_id).await;
    assert_eq!(model_name, env.model);
    assert_eq!(amount, 240);
}
