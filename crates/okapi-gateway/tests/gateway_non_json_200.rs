//! 上游 2xx 却回了网页（渠道地址缺 `/v1`、指到了网页端口——站点的前端对任何未知路径都回 `200 text/html`）：
//! 这不是一次成功的调用。不转发、不计费、换下一个候选；count_tokens 不再替上游编一个 `input_tokens: 0`。
//! 依赖 .env 的 DATABASE_URL / OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi_domain::Money;
use okapi_gateway as gateway;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

type Hits = Arc<Mutex<Vec<String>>>;

/// 站点的网页前端：没有 API 路由的路径都落在它这里。
async fn web_front(State(hits): State<Hits>, uri: axum::http::Uri) -> axum::response::Response {
    hits.lock().unwrap().push(format!("page {}", uri.path()));
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<!doctype html>\n<html lang=\"en\"><head><title>console</title></head><body></body></html>",
    )
        .into_response()
}

fn hit(hits: &Hits, what: &str) {
    hits.lock().unwrap().push(format!("good {what}"));
}

async fn good_messages(State(hits): State<Hits>) -> axum::response::Response {
    hit(&hits, "messages");
    axum::Json(json!({
        "id": "msg_a", "type": "message", "role": "assistant", "model": "claude-real",
        "content": [{"type": "text", "text": "native"}], "stop_reason": "end_turn",
        "usage": {"input_tokens": 100, "output_tokens": 50}
    }))
    .into_response()
}

async fn good_chat(State(hits): State<Hits>) -> axum::response::Response {
    hit(&hits, "chat");
    axum::Json(json!({
        "id": "cmpl-1", "object": "chat.completion", "model": "gpt-real",
        "choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "ok"}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    }))
    .into_response()
}

async fn good_embeddings(State(hits): State<Hits>) -> axum::response::Response {
    hit(&hits, "embeddings");
    axum::Json(json!({
        "object": "list",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
        "model": "emb-real",
        "usage": {"prompt_tokens": 120, "total_tokens": 120}
    }))
    .into_response()
}

async fn good_count_tokens(State(hits): State<Hits>) -> axum::response::Response {
    hit(&hits, "count_tokens");
    axum::Json(json!({"input_tokens": 42})).into_response()
}

/// `/good/…` 是正常的 API，其余一切（包括 `/page/…`）都是网页前端。
async fn spawn_site() -> (SocketAddr, Hits) {
    let hits = Hits::default();
    let router = Router::new()
        .route("/good/v1/messages", post(good_messages))
        .route("/good/v1/chat/completions", post(good_chat))
        .route("/good/v1/embeddings", post(good_embeddings))
        .route("/good/v1/messages/count_tokens", post(good_count_tokens))
        .fallback(web_front)
        .with_state(Arc::clone(&hits));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, hits)
}

struct TestEnv {
    state: gateway::state::AppState,
    pg: PgPool,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
    hits: Hits,
}

/// `channels`：(基址前缀, 优先级)，优先级大的先试。
async fn setup(provider: &str, channels: &[(&str, i32)]) -> TestEnv {
    okapi_store::test_support::assert_isolated();
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

    let (site, hits) = spawn_site().await;
    for (i, (prefix, priority)) in channels.iter().enumerate() {
        let (channel_id, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("ch{i}-{suffix}"),
            provider,
            &format!("http://{site}{prefix}"),
            "mock-credential",
            &[model.as_str()],
            false,
            None,
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

    published_pricing::publish(&pg, user_id).await;
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
        state,
        pg,
        gateway: addr,
        token,
        user_id,
        model,
        hits,
    }
}

async fn post_chat(env: &TestEnv) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "stream": false,
            "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
}

async fn post_messages(env: &TestEnv, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}{path}", env.gateway))
        .header("x-api-key", &env.token)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({"model": env.model, "max_tokens": 256,
            "messages": [{"role": "user", "content": "hi there"}]}))
        .send()
        .await
        .unwrap()
}

async fn post_embeddings(env: &TestEnv) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}/v1/embeddings", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({"model": env.model, "input": ["hello world"]}))
        .send()
        .await
        .unwrap()
}

/// 成功记账行：(status, amount_micro, failover_count)；等不到就 None。
async fn success_record(env: &TestEnv) -> Option<(i16, i64, i16)> {
    for _ in 0..30 {
        let row = sqlx::query_as::<_, (i16, i64, i16)>(
            "SELECT status, amount_micro, failover_count FROM billing_records WHERE user_id = $1 AND log_type = 2",
        )
        .bind(env.user_id)
        .fetch_optional(&env.pg)
        .await
        .unwrap();
        if row.is_some() {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

/// 失败终态行：(amount_micro, error_code)。
async fn failure_record(env: &TestEnv) -> Option<(i64, String)> {
    for _ in 0..50 {
        let row = sqlx::query_as::<_, (i64, String)>(
            "SELECT amount_micro, error_code FROM billing_records WHERE user_id = $1 AND log_type = 5",
        )
        .bind(env.user_id)
        .fetch_optional(&env.pg)
        .await
        .unwrap();
        if row.is_some() {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

async fn success_rows(env: &TestEnv) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM billing_records WHERE user_id = $1 AND log_type = 2",
    )
    .bind(env.user_id)
    .fetch_one(&env.pg)
    .await
    .unwrap()
}

async fn balance(env: &TestEnv) -> i64 {
    env.state
        .ledger
        .balance(env.user_id)
        .await
        .unwrap()
        .as_micros()
}

/// 只有一个渠道、它指到了网页：客户端拿到 502 `upstream_error`，不是一个 200 的网页；没有成功记账，余额不动。
#[tokio::test]
async fn a_web_page_is_not_an_answer_to_a_non_stream_chat_request() {
    let env = setup("openai", &[("/page/v1", 0)]).await;
    let response = post_chat(&env).await;
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], okapi_api::codes::UPSTREAM_ERROR);
    assert_eq!(
        failure_record(&env).await,
        Some((0, okapi_api::codes::UPSTREAM_ERROR.to_owned())),
        "失败终态行，金额 0"
    );
    assert_eq!(success_rows(&env).await, 0, "网页不能被记成一次成功的补全");
    assert_eq!(balance(&env).await, 10_000_000, "预扣已退还，没有扣费");
    assert_eq!(
        env.hits.lock().unwrap().as_slice(),
        ["page /page/v1/chat/completions"],
        "确定性的失败不做同 key 重试"
    );
}

/// 第一个候选指到了网页，第二个是正常 API：客户端无感，只为第二个候选的真实用量付费。
#[tokio::test]
async fn a_web_page_from_one_candidate_fails_over_to_the_next_on_messages() {
    let env = setup("anthropic", &[("/page/v1", 10), ("/good/v1", 0)]).await;
    let response = post_messages(&env, "/v1/messages").await;
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["content"][0]["text"], "native", "{body}");

    let (record_status, amount, failover) = success_record(&env).await.expect("成功记账");
    assert_eq!(record_status, 20);
    assert_eq!(failover, 1, "换过一次渠道");
    assert!(amount > 0);
    assert_eq!(success_rows(&env).await, 1, "只有一次计费");
    assert_eq!(balance(&env).await, 10_000_000 - amount);
    assert_eq!(
        env.hits.lock().unwrap().as_slice(),
        ["page /page/v1/messages", "good messages"]
    );
}

/// 对照：正常的 JSON 照常计费一次、不换渠道（新的检查不能误伤好的上游）。
#[tokio::test]
async fn a_normal_json_answer_is_billed_once_with_no_failover() {
    let env = setup("openai", &[("/good/v1", 0)]).await;
    let response = post_chat(&env).await;
    assert_eq!(response.status(), 200);
    let (status, amount, failover) = success_record(&env).await.expect("成功记账");
    assert_eq!((status, failover), (20, 0));
    assert!(amount > 0);
    assert_eq!(env.hits.lock().unwrap().as_slice(), ["good chat"]);
}

/// count_tokens 不计费，但原来会把网页悄悄变成 `{"input_tokens": 0}` 回给客户端（客户端据此以为上下文是空的）。
#[tokio::test]
async fn count_tokens_does_not_invent_a_zero_for_a_web_page() {
    let page = setup("anthropic", &[("/page/v1", 0)]).await;
    let response = post_messages(&page, "/v1/messages/count_tokens").await;
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 502, "{body}");
    assert_eq!(
        page.hits.lock().unwrap().as_slice(),
        ["page /page/v1/messages/count_tokens"]
    );

    let good = setup("anthropic", &[("/good/v1", 0)]).await;
    let response = post_messages(&good, "/v1/messages/count_tokens").await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["input_tokens"], 42, "正常的 JSON 原样回");
}

/// embeddings / rerank 共用的候选循环：网页当失败换下一个，只为成功的那次付费（120 × $2/1M = 240 micro）。
#[tokio::test]
async fn embeddings_fail_over_from_a_web_page_without_charging_for_it() {
    let env = setup("openai", &[("/page/v1", 10), ("/good/v1", 0)]).await;
    let response = post_embeddings(&env).await;
    assert_eq!(response.status(), 200);
    let (status, amount, failover) = success_record(&env).await.expect("成功记账");
    assert_eq!((status, amount, failover), (20, 240, 1));
    assert_eq!(success_rows(&env).await, 1);
    assert_eq!(balance(&env).await, 10_000_000 - 240);
    assert_eq!(
        env.hits.lock().unwrap().as_slice(),
        ["page /page/v1/embeddings", "good embeddings"]
    );

    let only_page = setup("openai", &[("/page/v1", 0)]).await;
    let response = post_embeddings(&only_page).await;
    assert_eq!(response.status(), 502);
    assert_eq!(success_rows(&only_page).await, 0);
    assert_eq!(balance(&only_page).await, 10_000_000);
}
