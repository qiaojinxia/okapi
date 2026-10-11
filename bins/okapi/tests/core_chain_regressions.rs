//! 核心链路评审回归（2026-10-04）：
//! - 客户端在响应前断开：预扣与 key 并发槽当场释放，并留 `client_closed_request` 失败日志
//!   （此前 handler 被丢弃后无人退款，要等约 10 分钟的过期清理）；首字已交付的流照常按产出结算；
//! - 渠道 key 失败计数按「连续」语义：成功清零、冷却中迟到的失败不叠加、恢复后再失败才翻倍，
//!   请求自己导致的空回复（预算耗尽 / 策略拦截）不记成 key 故障；
//! - 一个用户等自己的账本锁时，不能占着共享连接池或全局结算闸挡住其他用户；
//! - 预扣是实际扣费的上界：`n` 条候选、超过 32768 的显式 max_tokens 都要算进去，
//!   中断的纯工具调用流按已产出的参数兜底计费；
//! - HTTP Responses 拒绝后台模式；单个用户的坏回执不让过期清理整轮中止；
//!   订阅凭证拿不到时换账号，token 端点的错误体不外泄。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures::StreamExt as _;
use okapi::{gateway, worker};
use okapi_domain::Money;
use okapi_store::channels::{KeyFailure, clear_key_failures, mark_key_failure};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use uuid::Uuid;

const BALANCE: i64 = 10_000_000;

// ---- mock 上游 ----

#[derive(Clone)]
struct Mock {
    calls: Arc<AtomicUsize>,
    failing: Arc<AtomicBool>,
    received: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    /// `held` 路由：前 `hold` 次调用停在闸前，到齐时通知 `arrived`。
    hold: Arc<AtomicUsize>,
    arrived: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::watch::Sender<bool>>,
    token_calls: Arc<AtomicUsize>,
    /// `held` 路由收到的请求体（看网关往上游写了什么）。
    bodies: Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Default for Mock {
    fn default() -> Self {
        Self {
            calls: Arc::default(),
            failing: Arc::default(),
            received: Arc::default(),
            release: Arc::default(),
            hold: Arc::default(),
            arrived: Arc::default(),
            gate: Arc::new(tokio::sync::watch::channel(false).0),
            token_calls: Arc::default(),
            bodies: Arc::default(),
        }
    }
}

fn chunk(delta: &Value, finish: Option<&str>) -> Value {
    json!({"id":"c","object":"chat.completion.chunk",
        "choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
}

fn usage_chunk(prompt: u32, completion: u32) -> Value {
    json!({"id":"c","object":"chat.completion.chunk","choices":[],
        "usage":{"prompt_tokens":prompt,"completion_tokens":completion}})
}

fn frames(chunks: &[Value], done: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for chunk in chunks {
        let _ = write!(out, "data: {chunk}\n\n");
    }
    if done {
        out.push_str("data: [DONE]\n\n");
    }
    out
}

fn sse(body: Body) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn is_stream(body: &Bytes) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|v| v["stream"] == true)
}

fn ok_reply(stream: bool) -> Response {
    if stream {
        sse(Body::from(frames(
            &[
                chunk(&json!({"role":"assistant"}), None),
                chunk(&json!({"content":"Hello"}), None),
                chunk(&json!({}), Some("stop")),
                usage_chunk(100, 20),
            ],
            true,
        )))
    } else {
        axum::Json(json!({
            "id":"cmpl","object":"chat.completion",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Hello"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":20}
        }))
        .into_response()
    }
}

/// 第一次调用永不给出输出（流式只发 role 帧），之后正常回答。
async fn hang_first(State(mock): State<Mock>, body: Bytes) -> Response {
    let stream = is_stream(&body);
    if mock.calls.fetch_add(1, Ordering::SeqCst) > 0 {
        return ok_reply(stream);
    }
    if !stream {
        std::future::pending::<()>().await;
    }
    let role = frames(&[chunk(&json!({"role":"assistant"}), None)], false);
    let events = futures::stream::once(async move { Ok::<_, std::io::Error>(Bytes::from(role)) })
        .chain(futures::stream::pending());
    sse(Body::from_stream(events))
}

/// 首字立即给出，余下内容与 usage 停顿后才到：客户端读完首字就断开。
async fn slow_tail(_body: Bytes) -> Response {
    let head = frames(
        &[
            chunk(&json!({"role":"assistant"}), None),
            chunk(&json!({"content":"Hello"}), None),
        ],
        false,
    );
    let tail = frames(
        &[
            chunk(&json!({"content":" world"}), None),
            chunk(&json!({}), Some("stop")),
            usage_chunk(100, 20),
        ],
        true,
    );
    let events =
        futures::stream::iter([(Duration::ZERO, head), (Duration::from_millis(800), tail)]).then(
            |(delay, frame)| async move {
                tokio::time::sleep(delay).await;
                Ok::<_, std::io::Error>(Bytes::from(frame))
            },
        );
    sse(Body::from_stream(events))
}

/// `failing` 打开时 500，否则正常。
async fn flaky(State(mock): State<Mock>, body: Bytes) -> Response {
    if mock.failing.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({"error":{"message":"boom"}})),
        )
            .into_response();
    }
    ok_reply(is_stream(&body))
}

/// 收到请求先报到，等测试放行才回答（让测试在预扣之后、结算之前插手）。
async fn gated(State(mock): State<Mock>, body: Bytes) -> Response {
    mock.received.notify_one();
    mock.release.notified().await;
    ok_reply(is_stream(&body))
}

/// 同 `gated`，回一条 embeddings 结果（10 个输入 token）。
async fn gated_embeddings(State(mock): State<Mock>) -> Response {
    mock.received.notify_one();
    mock.release.notified().await;
    axum::Json(json!({
        "object": "list",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2]}],
        "model": "mock",
        "usage": {"prompt_tokens": 10, "total_tokens": 10}
    }))
    .into_response()
}

/// 前 `hold` 次调用等测试开闸（到齐时报到），之后的调用直接回答。
async fn held(State(mock): State<Mock>, body: Bytes) -> Response {
    if let Ok(value) = serde_json::from_slice(&body) {
        mock.bodies.lock().unwrap().push(value);
    }
    let call = mock.calls.fetch_add(1, Ordering::SeqCst) + 1;
    let hold = mock.hold.load(Ordering::SeqCst);
    if call <= hold {
        if call == hold {
            mock.arrived.notify_one();
        }
        let _ = mock.gate.subscribe().wait_for(|open| *open).await;
    }
    ok_reply(is_stream(&body))
}

/// 推理模型把 max_completion_tokens 全花在思考上：没有可见输出，finish_reason=length。
async fn budget_exhausted(_body: Bytes) -> Response {
    sse(Body::from(frames(
        &[
            chunk(&json!({"role":"assistant","content":""}), None),
            chunk(&json!({}), Some("length")),
            usage_chunk(100, 16),
        ],
        true,
    )))
}

/// 只产出工具调用（大段参数），usage 帧迟到：客户端在拿到参数后就断开。
async fn tool_tail(_body: Bytes) -> Response {
    let arguments = format!("{{\"patch\":\"{}\"}}", "x".repeat(2000));
    let head = frames(
        &[
            chunk(&json!({"role":"assistant"}), None),
            chunk(
                &json!({"tool_calls":[{"index":0,"id":"call_1","type":"function",
                    "function":{"name":"apply","arguments":arguments}}]}),
                None,
            ),
        ],
        false,
    );
    let tail = frames(
        &[chunk(&json!({}), Some("tool_calls")), usage_chunk(100, 700)],
        true,
    );
    let events =
        futures::stream::iter([(Duration::ZERO, head), (Duration::from_millis(800), tail)]).then(
            |(delay, frame)| async move {
                tokio::time::sleep(delay).await;
                Ok::<_, std::io::Error>(Bytes::from(frame))
            },
        );
    sse(Body::from_stream(events))
}

/// 订阅 token 端点：拒绝刷新，但不是授权失效类错误（凭证本身没被吊销，不会被判死）。
async fn token_rejected(State(mock): State<Mock>) -> Response {
    mock.token_calls.fetch_add(1, Ordering::SeqCst);
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({"error": "invalid_scope", "error_description": "token-endpoint-secret"})),
    )
        .into_response()
}

/// 坏中转：没有任何原因的空流。
async fn broken_empty(_body: Bytes) -> Response {
    sse(Body::from(frames(
        &[chunk(&json!({"role":"assistant"}), None)],
        true,
    )))
}

async fn spawn_mock(mock: Mock) -> SocketAddr {
    let router = Router::new()
        .route("/hang/v1/chat/completions", post(hang_first))
        .route("/slow/v1/chat/completions", post(slow_tail))
        .route("/flaky/v1/chat/completions", post(flaky))
        .route("/gated/v1/chat/completions", post(gated))
        .route("/gated/v1/embeddings", post(gated_embeddings))
        .route("/held/v1/chat/completions", post(held))
        .route("/length/v1/chat/completions", post(budget_exhausted))
        .route("/empty/v1/chat/completions", post(broken_empty))
        .route("/tools/v1/chat/completions", post(tool_tail))
        .route("/oauth/token", post(token_rejected))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

// ---- 测试环境 ----

struct Env {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    gateway: SocketAddr,
    upstream: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
    channel_key: i64,
    mock: Mock,
}

async fn setup(route: &str, key_concurrency: Option<i32>) -> Env {
    setup_funded(route, key_concurrency, BALANCE).await
}

async fn setup_funded(route: &str, key_concurrency: Option<i32>, balance: i64) -> Env {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
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
    let api_key = okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();
    if let Some(cap) = key_concurrency {
        sqlx::query("UPDATE api_keys SET max_concurrency = $2 WHERE id = $1")
            .bind(api_key)
            .bind(cap)
            .execute(&pg)
            .await
            .unwrap();
    }
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let mock = Mock::default();
    let upstream = spawn_mock(mock.clone()).await;
    let (_channel, channel_key) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        "openai",
        &format!("http://{upstream}/{route}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    published_pricing::publish(&pg, user_id).await;

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    okapi_ledger::operations::credit(
        &pg,
        &state.ledger,
        user_id,
        Money::from_micros(balance),
        "adjust",
        "test",
        json!({}),
    )
    .await
    .unwrap();
    let ledger = state.ledger.clone();
    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Env {
        pg,
        ledger,
        gateway,
        upstream,
        token,
        user_id,
        model,
        channel_key,
        mock,
    }
}

async fn post_chat(
    env: &Env,
    stream: bool,
    timeout: Option<Duration>,
) -> reqwest::Result<reqwest::Response> {
    let mut request = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model,
            "stream": stream,
            "max_tokens": 64,
            "messages": [{"role":"user","content":"hi there"}]
        }));
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    request.send().await
}

async fn chat_with(env: &Env, extra: Value) -> u16 {
    let mut body = json!({"model": env.model, "stream": false,
        "messages": [{"role":"user","content":"hi there"}]});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let _ = resp.bytes().await;
    status
}

/// 同一模型 / 渠道下再开一个有余额的用户，返回其 token。
async fn second_user(env: &Env) -> String {
    funded_user(env).await.2
}

/// (user_id, api_key_id, token)：同一模型 / 渠道下再开一个有余额的用户。
async fn funded_user(env: &Env) -> (i64, i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let user = okapi_store::provision::create_user(&env.pg, &format!("u2-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-test-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    let key = okapi_store::provision::create_api_key(&env.pg, user, &key_hash, "sk-okapi-test")
        .await
        .unwrap();
    okapi_ledger::operations::credit(
        &env.pg,
        &env.ledger,
        user,
        Money::from_micros(BALANCE),
        "adjust",
        "test",
        json!({}),
    )
    .await
    .unwrap();
    (user, key, token)
}

async fn chat_status(gateway: SocketAddr, token: String, model: String) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{gateway}/v1/chat/completions"))
        .bearer_auth(token)
        .json(&json!({"model": model, "stream": false, "max_tokens": 64,
            "messages": [{"role":"user","content":"hi there"}]}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// (status, log_type, error_code, amount_micro) of the user's newest bill.
async fn wait_latest_record(env: &Env) -> (i16, i16, Option<String>, i64) {
    for _ in 0..100 {
        let row: Option<(i16, i16, Option<String>, i64)> = sqlx::query_as(
            "SELECT status, log_type, error_code, amount_micro FROM billing_records
             WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(env.user_id)
        .fetch_optional(&env.pg)
        .await
        .unwrap();
        if let Some(row) = row {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no billing record for user {}", env.user_id);
}

/// Settlement is PG-first: the bill row commits before the Redis hold closes.
async fn wait_hold_closed(env: &Env, balance: i64, why: &str) {
    for _ in 0..100 {
        let open = env.ledger.list_reservations(env.user_id).await.unwrap();
        if open.is_empty() {
            let actual = env.ledger.balance(env.user_id).await.unwrap();
            assert_eq!(actual.as_micros(), balance, "{why}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("reservation still held: {why}");
}

async fn key_state(pg: &PgPool, key: i64) -> (i16, i32) {
    sqlx::query_as("SELECT status, failed_count FROM channel_keys WHERE id = $1")
        .bind(key)
        .fetch_one(pg)
        .await
        .unwrap()
}

async fn cooldown_secs(pg: &PgPool, key: i64) -> i64 {
    sqlx::query_scalar(
        "SELECT extract(epoch FROM cooldown_until - now())::bigint FROM channel_keys WHERE id = $1",
    )
    .bind(key)
    .fetch_one(pg)
    .await
    .unwrap()
}

// ---- 客户端断开 ----

async fn disconnect_before_response(stream: bool) {
    let env = setup("hang", Some(1)).await;
    let cancelled = post_chat(&env, stream, Some(Duration::from_millis(500))).await;
    assert!(
        cancelled.is_err_and(|e| e.is_timeout()),
        "the upstream never answers the first call"
    );

    wait_hold_closed(&env, BALANCE, "the hold is refunded in full").await;
    let (status, log_type, error_code, amount) = wait_latest_record(&env).await;
    assert_eq!(
        (status, log_type, amount),
        (40, 5, 0),
        "failed, unbilled log"
    );
    assert_eq!(error_code.as_deref(), Some("client_closed_request"));

    // 并发上限 1：断开的请求若仍占着 key 并发槽，这一笔会被 429 concurrency 挡住
    let next = post_chat(&env, stream, None).await.unwrap();
    assert_eq!(next.status(), 200, "the key concurrency slot was released");
    let _ = next.bytes().await;
    assert_eq!(env.mock.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn json_client_disconnect_releases_hold_and_concurrency() {
    disconnect_before_response(false).await;
}

#[tokio::test]
async fn stream_disconnect_before_first_output_releases_hold_and_concurrency() {
    disconnect_before_response(true).await;
}

/// 首字后断开：结算已交给流泵，取消兜底不得把已交付的产出退掉。
#[tokio::test]
async fn delivered_stream_is_billed_after_client_disconnect() {
    let env = setup("slow", None).await;
    let mut resp = post_chat(&env, true, None).await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut seen = String::new();
    while !seen.contains("Hello") {
        let frame = resp.chunk().await.unwrap().expect("first output arrives");
        seen.push_str(&String::from_utf8_lossy(&frame));
    }
    assert!(
        !seen.contains("world"),
        "disconnect before the tail: {seen}"
    );
    drop(resp);

    let (status, log_type, error_code, amount) = wait_latest_record(&env).await;
    assert_eq!((status, log_type), (20, 2), "committed by the stream pump");
    assert_ne!(error_code.as_deref(), Some("client_closed_request"));
    assert!(amount > 0, "delivered output is billed");
    wait_hold_closed(&env, BALANCE - amount, "charged for the delivered part").await;
}

/// 上游已回答、结算已交给后台任务，handler 还在等落账时客户端断开：
/// 取消兜底不得退款或抢先写失败日志，后台结算照常按实际用量记账。
#[tokio::test]
async fn handed_off_json_settlement_survives_client_disconnect() {
    let env = setup("gated", None).await;
    let client = {
        let gateway = env.gateway;
        let token = env.token.clone();
        let model = env.model.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://{gateway}/v1/chat/completions"))
                .bearer_auth(token)
                .timeout(Duration::from_secs(1))
                .json(&json!({"model": model, "stream": false, "max_tokens": 64,
                    "messages": [{"role":"user","content":"hi there"}]}))
                .send()
                .await
        })
    };
    // 预扣已完成、请求已到上游：拿住该用户的结算锁，让交接后的结算停在落账前
    env.mock.received.notified().await;
    let lock = okapi_ledger::holds::UserGuard::acquire(&env.pg, env.user_id)
        .await
        .unwrap();
    env.mock.release.notify_one();
    let outcome = client.await.unwrap();
    assert!(
        outcome.is_err_and(|e| e.is_timeout()),
        "client gave up first"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(lock);

    let (status, log_type, error_code, amount) = wait_latest_record(&env).await;
    assert_eq!(
        (status, log_type),
        (20, 2),
        "the handed-off settlement wins"
    );
    assert_eq!(error_code, None);
    assert_eq!(amount, 240, "(100 + 20) × ratio 1 × $2/1M");
    wait_hold_closed(&env, BALANCE - 240, "charged once, never refunded").await;
}

/// 媒体端点在 handler 里等结算：上游已回答、落账还在排队时客户端断开，
/// 失败守卫曾抢先写下 0 元失败账，按 request_id 先到先得，真正的扣费被当重放跳过。
/// 现在结算与连接解绑，守卫在交给结算前解除。
#[tokio::test]
async fn media_settlement_survives_client_disconnect() {
    let env = setup("gated", None).await;
    let client = {
        let gateway = env.gateway;
        let token = env.token.clone();
        let model = env.model.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://{gateway}/v1/embeddings"))
                .bearer_auth(token)
                .timeout(Duration::from_secs(1))
                .json(&json!({"model": model, "input": "hello there"}))
                .send()
                .await
        })
    };
    env.mock.received.notified().await;
    let lock = okapi_ledger::holds::UserGuard::acquire(&env.pg, env.user_id)
        .await
        .unwrap();
    env.mock.release.notify_one();
    let outcome = client.await.unwrap();
    assert!(
        outcome.is_err_and(|e| e.is_timeout()),
        "client gave up while the settlement waited"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(lock);

    let (status, log_type, error_code, amount) = wait_latest_record(&env).await;
    assert_eq!((status, log_type), (20, 2), "the charge is recorded");
    assert_eq!(error_code, None);
    assert_eq!(amount, 20, "10 input tokens × ratio 1 × $2/1M");
    wait_hold_closed(&env, BALANCE - 20, "charged once, never refunded").await;
    let records: i64 =
        sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id = $1")
            .bind(env.user_id)
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert_eq!(records, 1, "no competing failure record");
}

// ---- 单个用户的锁排队不得挤占全站 ----

/// A 的预扣都在等 A 的用户锁（测试持有）：它们排在进程内队列里，
/// 不占共享连接池，B 的请求照常秒回。此前 A 的等待者各占一条池连接卡在锁上。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hot_user_waiting_for_its_lock_does_not_starve_other_users() {
    let env = setup("held", None).await;
    let other = second_user(&env).await;
    let lock = okapi_ledger::holds::UserGuard::acquire(&env.pg, env.user_id)
        .await
        .unwrap();
    let burst: Vec<_> = (0..40)
        .map(|_| {
            tokio::spawn(chat_status(
                env.gateway,
                env.token.clone(),
                env.model.clone(),
            ))
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    let status = chat_status(env.gateway, other, env.model.clone()).await;
    let elapsed = started.elapsed();
    assert_eq!(status, 200);
    assert!(
        elapsed < Duration::from_millis(1500),
        "B waited {elapsed:?}"
    );

    tokio::time::sleep(Duration::from_secs(3).saturating_sub(elapsed)).await;
    drop(lock);
    for task in burst {
        let status = task.await.unwrap();
        assert!(
            matches!(status, 200 | 429),
            "A finishes or backs off, never 500: {status}"
        );
    }
}

/// A 的 12 个结算都在等 A 的用户锁（测试持有）：先排用户队、后占结算闸，
/// 所以全局结算闸仍有空位，B 的结算照常落账。此前 A 先占满结算闸再等锁。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hot_user_settlements_do_not_hold_the_shared_settlement_gate() {
    let env = setup("held", None).await;
    env.mock.hold.store(12, Ordering::SeqCst);
    let other = second_user(&env).await;
    let burst: Vec<_> = (0..12)
        .map(|_| {
            tokio::spawn(chat_status(
                env.gateway,
                env.token.clone(),
                env.model.clone(),
            ))
        })
        .collect();
    // 12 笔都已预扣并到达上游，此时拿住 A 的锁再放上游回答：结算全部卡在 A 的锁上
    tokio::time::timeout(Duration::from_secs(10), env.mock.arrived.notified())
        .await
        .unwrap();
    let lock = okapi_ledger::holds::UserGuard::acquire(&env.pg, env.user_id)
        .await
        .unwrap();
    env.mock.gate.send_replace(true);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    let status = chat_status(env.gateway, other, env.model.clone()).await;
    let elapsed = started.elapsed();
    assert_eq!(status, 200);
    assert!(
        elapsed < Duration::from_millis(1500),
        "B waited {elapsed:?}"
    );

    tokio::time::sleep(Duration::from_secs(3).saturating_sub(elapsed)).await;
    drop(lock);
    for task in burst {
        assert_eq!(task.await.unwrap(), 200, "A settles once its lock frees");
    }
}

// ---- 预扣是扣费上界 ----

/// 价格 2 micro / token：n=1、max_tokens=1000 约预扣 2 千 micro，n=8 约 1.6 万 micro。
/// 余额 1 万时后者必须在预扣阶段被拒——此前只按 1 条预扣，结算时余额被透支。
#[tokio::test]
async fn every_requested_choice_is_reserved() {
    let env = setup_funded("held", None, 10_000).await;
    assert_eq!(
        chat_with(&env, json!({"max_tokens": 1000, "n": 8})).await,
        429
    );
    assert_eq!(
        env.mock.calls.load(Ordering::SeqCst),
        0,
        "rejected before upstream"
    );
    assert_eq!(
        chat_with(&env, json!({"max_tokens": 1000, "n": 1})).await,
        200
    );
}

/// 显式 max_tokens 超过 32768 时照单预扣（不超过模型 max_output）：此前截到 32768，
/// 上游却按原值生成，余额 10 万 micro 的用户能发出可能花掉 12 万的请求。
#[tokio::test]
async fn explicit_long_output_is_reserved_in_full() {
    let env = setup_funded("held", None, 100_000).await;
    assert_eq!(chat_with(&env, json!({"max_tokens": 60_000})).await, 429);
    assert_eq!(env.mock.calls.load(Ordering::SeqCst), 0);
    // 模型声明了 max_output：上游不可能产出更多，按它封顶即可放行
    let configured = setup_funded("held", None, 100_000).await;
    sqlx::query("UPDATE models SET max_output = 30000 WHERE model_name = $1")
        .bind(&configured.model)
        .execute(&configured.pg)
        .await
        .unwrap();
    assert_eq!(
        chat_with(&configured, json!({"max_tokens": 60_000})).await,
        200
    );
}

/// 没写输出上限、模型 max_output 又大于预扣封顶：预扣按 32768 估，转发时就得把 32768
/// 写进请求，否则上游可按 12.8 万生成、结算多退少补透支余额。显式上限原样转发。
#[tokio::test]
async fn omitted_output_cap_is_bounded_upstream_to_what_was_reserved() {
    let env = setup("held", None).await;
    sqlx::query("UPDATE models SET max_output = 128000 WHERE model_name = $1")
        .bind(&env.model)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(chat_with(&env, json!({})).await, 200);
    assert_eq!(chat_with(&env, json!({"max_tokens": 50})).await, 200);
    let bodies = env.mock.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["max_tokens"], 32_768, "{}", bodies[0]);
    assert_eq!(bodies[1]["max_tokens"], 50, "{}", bodies[1]);

    // max_output 不超过封顶的模型生成不到更多：请求不动
    let small = setup("held", None).await;
    sqlx::query("UPDATE models SET max_output = 8192 WHERE model_name = $1")
        .bind(&small.model)
        .execute(&small.pg)
        .await
        .unwrap();
    assert_eq!(chat_with(&small, json!({})).await, 200);
    let bodies = small.mock.bodies.lock().unwrap().clone();
    assert!(bodies[0].get("max_tokens").is_none(), "{}", bodies[0]);
}

/// 图片输入计入预扣：每张按 2560 token 估。4 张图 ≈ 2 万 micro，余额 1.5 万应在预扣阶段被拒；
/// 同样的文字不带图则放行。此前 prompt 估算只数文本，带大量图片的请求可透支。
#[tokio::test]
async fn image_inputs_are_reserved() {
    let env = setup_funded("held", None, 15_000).await;
    let image = json!({"type": "image_url", "image_url": {"url": "https://example.com/a.png"}});
    let with_images = json!({"max_tokens": 10, "messages": [{"role": "user", "content": [
        {"type": "text", "text": "hi there"}, image, image, image, image
    ]}]});
    assert_eq!(chat_with(&env, with_images).await, 429);
    assert_eq!(env.mock.calls.load(Ordering::SeqCst), 0);
    let text_only = json!({"max_tokens": 10, "messages": [{"role": "user", "content": [
        {"type": "text", "text": "hi there"}
    ]}]});
    assert_eq!(chat_with(&env, text_only).await, 200);
}

/// 内联 PDF 按页计入预扣（每页 3000 token）：3 页 ≈ 1.8 万 micro，余额 1.5 万应被拒。
/// 此前 prompt 估算看不见文件内容，带大文档的请求可透支。
#[tokio::test]
async fn inline_documents_are_reserved_by_page() {
    use base64::Engine as _;
    let env = setup_funded("held", None, 15_000).await;
    let pdf = base64::prelude::BASE64_STANDARD.encode(
        b"%PDF-1.4 <</Type /Pages /Count 3>> <</Type /Page>> <</Type /Page>> <</Type /Page>>",
    );
    let with_pdf = json!({"max_tokens": 10, "messages": [{"role": "user", "content": [
        {"type": "text", "text": "summarize"},
        {"type": "file", "file": {"filename": "a.pdf",
            "file_data": format!("data:application/pdf;base64,{pdf}")}}
    ]}]});
    assert_eq!(chat_with(&env, with_pdf).await, 429);
    assert_eq!(env.mock.calls.load(Ordering::SeqCst), 0);
}

/// 纯工具调用的流在 usage 帧前断开：按已交付的参数估补全，而不是只记 1 个 token。
#[tokio::test]
async fn interrupted_tool_call_stream_bills_its_arguments() {
    let env = setup("tools", None).await;
    let mut resp = post_chat(&env, true, None).await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut seen = String::new();
    while !seen.contains("xxxxxxxxxx") {
        let frame = resp.chunk().await.unwrap().expect("tool arguments arrive");
        seen.push_str(&String::from_utf8_lossy(&frame));
    }
    drop(resp);

    let (status, _, _, amount) = wait_latest_record(&env).await;
    assert_eq!(status, 20);
    let completion: i32 = sqlx::query_scalar(
        "SELECT completion_tokens FROM billing_records WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(env.user_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(
        completion > 500,
        "~2000 argument chars estimated, got {completion}"
    );
    wait_hold_closed(
        &env,
        BALANCE - amount,
        "charged for the delivered arguments",
    )
    .await;
}

// ---- 中危项 ----

/// HTTP Responses 不接后台模式：上游先回「排队中」不带用量、生成在结算后继续，
/// 网关只能按估算收一点而推理费全落在站方。显式 false 照常处理。
#[tokio::test]
async fn responses_background_mode_is_rejected_before_admission() {
    let env = setup("held", None).await;
    let responses = |background: bool| {
        reqwest::Client::new()
            .post(format!("http://{}/v1/responses", env.gateway))
            .bearer_auth(&env.token)
            .json(
                &json!({"model": env.model, "input": "hi", "max_output_tokens": 64,
                "background": background}),
            )
            .send()
    };
    let rejected = responses(true).await.unwrap();
    assert_eq!(rejected.status(), 400);
    let body: Value = rejected.json().await.unwrap();
    assert_eq!(body["error"]["param"], "background", "{body}");
    assert_eq!(
        env.mock.calls.load(Ordering::SeqCst),
        0,
        "never reaches upstream"
    );
    assert!(
        env.ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty()
    );
    let normal = responses(false).await.unwrap();
    assert_eq!(normal.status(), 200);
}

/// 一个用户的坏回执不能让过期清理整轮中止：id 更大的用户的过期预扣照常回收。
#[tokio::test]
async fn one_broken_user_does_not_stall_reservation_expiry_for_others() {
    use fred::interfaces::HashesInterface as _;
    let env = setup("held", None).await;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let broken_field = format!("r:{}", Uuid::new_v4());
    let _: i64 = redis
        .hset(
            format!("bal:{{{}}}", env.user_id),
            (broken_field.as_str(), "not-a-number|0|1|0"),
        )
        .await
        .unwrap();
    let (later, key, _) = funded_user(&env).await;
    assert!(
        later > env.user_id,
        "the healthy user sorts after the broken one"
    );
    let request = Uuid::new_v4();
    // 过期清理扫全库：拿 11 分钟前的时钟预扣（截止已过），清理用真实时钟，
    // 并行用例的在途预扣就不会被当成过期退掉
    let admitted = chrono::Utc::now() - chrono::Duration::minutes(11);
    let reserved = env
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: later,
                api_key_id: key,
                request_id: request,
                est: Money::from_micros(1_000),
                caps: okapi_ledger::LimitCaps::default(),
                est_tokens: 1,
            },
            admitted,
        )
        .await
        .unwrap();
    assert!(matches!(
        reserved,
        okapi_ledger::ReserveOutcome::Reserved { .. }
    ));

    let swept = worker::sweep_expired_reservations(&env.pg, &env.ledger, chrono::Utc::now())
        .await
        .expect("a broken user is skipped, not fatal to the whole pass");
    assert!(
        swept
            .iter()
            .any(|s| s.request_id == request && s.action == "refund"),
        "{swept:?}"
    );
    assert!(
        env.ledger
            .list_reservations(later)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        env.ledger.balance(later).await.unwrap().as_micros(),
        BALANCE
    );
    let _: i64 = redis
        .hdel(format!("bal:{{{}}}", env.user_id), broken_field)
        .await
        .unwrap();
}

/// 订阅账号刷新被拒（非授权失效类，如 invalid_scope）是账号问题：换下一个渠道完成请求；
/// 只剩这一个账号时回 502，token 端点的错误体不能外泄给客户端。
#[tokio::test]
async fn subscription_credential_failure_fails_over_without_leaking_the_token_endpoint() {
    for with_fallback in [true, false] {
        let env = setup("held", None).await;
        if !with_fallback {
            sqlx::query(
                "UPDATE channels SET status = 0 WHERE id = (SELECT channel_id FROM channel_keys WHERE id = $1)",
            )
            .bind(env.channel_key)
            .execute(&env.pg)
            .await
            .unwrap();
        }
        // 凭证记着已授 user:plugins：被拒 invalid_scope 不降级重试（上游误报不能让新 token 丢 scope），只刷一次
        let expired = json!({"kind": "oauth", "access_token": "stale", "refresh_token": "refresh",
            "expires_at": chrono::Utc::now().timestamp() - 60,
            "scope": "user:profile user:inference user:plugins"})
        .to_string();
        let settings = json!({"oauth_token_url": format!("http://{}/oauth/token", env.upstream)});
        okapi_store::provision::create_channel_configured(
            &env.pg,
            okapi_store::provision::ChannelCreate {
                name: &format!("max-{}", Uuid::new_v4().simple()),
                provider: "anthropic_max",
                api_base: &format!("http://{}/max", env.upstream),
                credential: &expired,
                models: &[env.model.as_str()],
                trust_upstream_usage: false,
                owner_id: None,
                settings: Some(&settings),
                priority: 10,
                max_concurrency: None,
                cost_milli: None,
                pools: None,
                egress: None,
                egress_preassigned: None,
            },
            None,
        )
        .await
        .unwrap();

        let resp = reqwest::Client::new()
            .post(format!("http://{}/v1/messages", env.gateway))
            .header("x-api-key", &env.token)
            .json(&json!({"model": env.model, "max_tokens": 64,
                "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        assert_eq!(
            env.mock.token_calls.load(Ordering::SeqCst),
            1,
            "the refresh was tried"
        );
        assert!(!text.contains("token-endpoint-secret"), "{text}");
        if with_fallback {
            assert_eq!(status, 200, "{text}");
        } else {
            assert_eq!(status, 502, "{text}");
        }
    }
}

// ---- key 健康：连续失败语义 ----

/// 失败被一次成功打断后重新计数：两次失败 + 一次成功 + 两次失败不进冷却。
#[tokio::test]
async fn success_clears_consecutive_failure_count() {
    let env = setup("flaky", None).await;
    env.mock.failing.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(post_chat(&env, false, None).await.unwrap().status(), 502);
    }
    assert_eq!(key_state(&env.pg, env.channel_key).await, (1, 2));

    env.mock.failing.store(false, Ordering::SeqCst);
    assert_eq!(post_chat(&env, false, None).await.unwrap().status(), 200);
    assert_eq!(
        key_state(&env.pg, env.channel_key).await,
        (1, 0),
        "a success resets the consecutive count"
    );

    env.mock.failing.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(post_chat(&env, false, None).await.unwrap().status(), 502);
    }
    assert_eq!(
        key_state(&env.pg, env.channel_key).await,
        (1, 2),
        "two fresh failures stay below the threshold of three"
    );
    assert_eq!(post_chat(&env, false, None).await.unwrap().status(), 502);
    assert_eq!(key_state(&env.pg, env.channel_key).await, (2, 3));
}

/// 冷却中迟到的失败不叠加；恢复后很快再失败才翻倍；久远的旧计数从 1 重来。
#[tokio::test]
async fn cooling_ignores_late_failures_and_relapse_doubles_the_pause() {
    let env = setup("flaky", None).await;
    let key = env.channel_key;
    for _ in 0..3 {
        mark_key_failure(&env.pg, key, "upstream_status", KeyFailure::Transient)
            .await
            .unwrap();
    }
    assert_eq!(key_state(&env.pg, key).await, (2, 3));
    let first = cooldown_secs(&env.pg, key).await;
    assert!((55..=60).contains(&first), "base pause: {first}");
    for _ in 0..10 {
        mark_key_failure(&env.pg, key, "late", KeyFailure::Transient)
            .await
            .unwrap();
    }
    assert_eq!(
        key_state(&env.pg, key).await,
        (2, 3),
        "late failures ignored"
    );
    assert!(
        cooldown_secs(&env.pg, key).await <= first,
        "pause not extended"
    );
    assert!(
        !clear_key_failures(&env.pg, key).await.unwrap(),
        "cooling keys keep their state"
    );

    // 到期恢复保留计数（半开），紧接着再失败 → 下一轮冷却翻倍
    sqlx::query(
        "UPDATE channel_keys SET cooldown_until = now() - interval '1 second' WHERE id = $1",
    )
    .bind(key)
    .execute(&env.pg)
    .await
    .unwrap();
    worker::recover_cooled_keys(&env.pg).await.unwrap();
    assert_eq!(key_state(&env.pg, key).await, (1, 3));
    mark_key_failure(&env.pg, key, "relapse", KeyFailure::Transient)
        .await
        .unwrap();
    assert_eq!(key_state(&env.pg, key).await, (2, 4));
    let second = cooldown_secs(&env.pg, key).await;
    assert!((115..=120).contains(&second), "doubled pause: {second}");

    // 冷却结束已久的旧计数不再一击即冷却
    sqlx::query(
        "UPDATE channel_keys SET cooldown_until = now() - interval '20 minutes' WHERE id = $1",
    )
    .bind(key)
    .execute(&env.pg)
    .await
    .unwrap();
    worker::recover_cooled_keys(&env.pg).await.unwrap();
    assert_eq!(key_state(&env.pg, key).await, (1, 4));
    mark_key_failure(&env.pg, key, "sporadic", KeyFailure::Transient)
        .await
        .unwrap();
    assert_eq!(
        key_state(&env.pg, key).await,
        (1, 1),
        "stale count restarts"
    );
    assert!(clear_key_failures(&env.pg, key).await.unwrap());
    assert_eq!(key_state(&env.pg, key).await, (1, 0));
    assert!(
        !clear_key_failures(&env.pg, key).await.unwrap(),
        "no write without a count"
    );
}

/// 一次短暂故障里并发涌来的失败只触发一轮基础冷却，不会被叠成两小时。
#[tokio::test]
async fn concurrent_failures_trigger_one_base_pause() {
    let env = setup("flaky", None).await;
    let key = env.channel_key;
    let burst = (0..20).map(|_| {
        let pg = env.pg.clone();
        tokio::spawn(async move {
            mark_key_failure(&pg, key, "upstream_status", KeyFailure::Transient)
                .await
                .unwrap();
        })
    });
    for task in futures::future::join_all(burst).await {
        task.unwrap();
    }
    assert_eq!(key_state(&env.pg, key).await, (2, 3));
    let pause = cooldown_secs(&env.pg, key).await;
    assert!(pause <= 60, "one base pause, not an escalated one: {pause}");
}

/// 预算耗尽的空流照旧不计费、换渠道，但不记成 key 故障；无原因的空流仍然计数。
#[tokio::test]
async fn request_caused_empty_stream_does_not_penalize_the_key() {
    let env = setup("length", None).await;
    let resp = post_chat(&env, true, None).await.unwrap();
    assert_eq!(resp.status(), 502);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "empty_completion");
    assert_eq!(key_state(&env.pg, env.channel_key).await, (1, 0));
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), BALANCE, "still not billed");

    let broken = setup("empty", None).await;
    let resp = post_chat(&broken, true, None).await.unwrap();
    assert_eq!(resp.status(), 502);
    assert_eq!(
        key_state(&broken.pg, broken.channel_key).await,
        (1, 1),
        "an unexplained empty stream still counts"
    );
}
