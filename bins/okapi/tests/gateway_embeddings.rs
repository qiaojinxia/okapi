//! /v1/embeddings 验收：prompt-only 计费、failover、透传。
//! 依赖 .env 的 DATABASE_URL / OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert!(req["model"].as_str().unwrap().starts_with("m-"));
    axum::Json(json!({
        "object": "list",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
        "model": req["model"],
        "usage": {"prompt_tokens": 120, "total_tokens": 120}
    }))
    .into_response()
}

async fn mock_fail() -> axum::response::Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({"error": {"message": "boom"}})),
    )
        .into_response()
}

async fn mock_rerank(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(req["query"], "best llm gateway", "请求体原样透传");
    axum::Json(json!({
        "results": [{"index": 0, "relevance_score": 0.9}, {"index": 1, "relevance_score": 0.1}],
        "usage": {"prompt_tokens": 120, "total_tokens": 120}
    }))
    .into_response()
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new()
        .route("/ok/v1/embeddings", post(mock_ok))
        .route("/ok/v1/rerank", post(mock_rerank))
        .route("/fail/v1/embeddings", post(mock_fail));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(channels: &[(&str, i32)]) -> TestEnv {
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
    for (i, (path, priority)) in channels.iter().enumerate() {
        let (channel_id, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("ch{i}-{suffix}"),
            "openai",
            &format!("http://{mock}{path}"),
            "mock-credential",
            &[model.as_str()],
            false,
        )
        .await
        .unwrap();
        sqlx::query!(
            "UPDATE channels SET priority = $2 WHERE id = $1",
            channel_id,
            priority
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

async fn post_embeddings(env: &TestEnv) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "input": ["hello world", "second doc"]}))
        .send()
        .await
        .unwrap()
}

async fn wait_record(pg: &PgPool, user_id: i64) -> (i16, i64, i32, i16) {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens, failover_count
               FROM billing_records WHERE user_id = $1 AND log_type = 2"#,
            user_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return (r.status, r.amount_micro, r.prompt_tokens, r.failover_count);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("等待记账超时");
}

/// 闭环：prompt-only 计费（120 × 1 × $2/1M = 240 micro）。
#[tokio::test]
async fn embeddings_bills_prompt_only() {
    let env = setup(&[("/ok/v1", 0)]).await;
    let resp = post_embeddings(&env).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "list");
    assert_eq!(body["usage"]["prompt_tokens"], 120);

    let (status, amount, prompt, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt, 120);
    assert_eq!(amount, 240);
    let balance = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events WHERE user_id = $1 AND event_type = 'commit'"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(balance >= 1);
}

/// rerank：同一泛化链路（Jina/Cohere 形状），prompt-only 计费。
#[tokio::test]
async fn rerank_bills_prompt_only() {
    let env = setup(&[("/ok/v1", 0)]).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/rerank", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "query": "best llm gateway",
            "documents": ["okapi is a gateway", "bananas are yellow"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["usage"]["prompt_tokens"], 120, "上游响应原样");

    let (status, amount, prompt, _) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(prompt, 120);
    assert_eq!(amount, 240);
}

/// 5xx failover 到次优先级渠道，客户端无感。
#[tokio::test]
async fn embeddings_failover_on_transient() {
    let env = setup(&[("/fail/v1", 10), ("/ok/v1", 0)]).await;
    let resp = post_embeddings(&env).await;
    assert_eq!(resp.status(), 200);
    let (status, amount, _, failover) = wait_record(&env.pg, env.user_id).await;
    assert_eq!(status, 20);
    assert_eq!(amount, 240);
    assert_eq!(failover, 1);
}
