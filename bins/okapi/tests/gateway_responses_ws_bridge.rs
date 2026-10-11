//! Real WS ingress and HTTP/SSE upstreams with isolated PG/Redis principals.
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use okapi::{gateway, gateway::state::AppState};
use okapi_domain::Money;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const WAIT: Duration = Duration::from_secs(12);

fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Pending {
    body: Value,
    headers: HeaderMap,
    path: String,
    reply: oneshot::Sender<Response>,
}
impl Pending {
    fn respond(self, events: &[Value]) {
        let body: String = events.iter().map(frame).collect();
        self.reply
            .send(
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("x-request-id", "http-bridge-fixture")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .unwrap();
    }
    fn error(self, status: u16) {
        self.reply
            .send(
                (
                    StatusCode::from_u16(status).unwrap(),
                    axum::Json(
                        json!({"error":{"code":"fixture_failure","message":"controlled failure"}}),
                    ),
                )
                    .into_response(),
            )
            .unwrap();
    }
    fn stream(self) -> mpsc::Sender<Result<Bytes, Infallible>> {
        let (send, receive) = mpsc::channel(8);
        let stream = futures::stream::unfold(receive, |mut receive| async move {
            receive.recv().await.map(|item| (item, receive))
        });
        self.reply
            .send(
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from_stream(stream))
                    .unwrap(),
            )
            .unwrap();
        send
    }
}

struct Env {
    state: AppState,
    address: SocketAddr,
    incoming: mpsc::Receiver<Pending>,
    get_status: Arc<AtomicU16>,
    gets: Arc<AtomicUsize>,
    posts: Arc<AtomicUsize>,
    model: String,
    token: String,
    user: i64,
    key: i64,
    channel: i64,
    channel_key: i64,
}
async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let model = format!("bridge-{}", Uuid::new_v4().simple());
    let user = okapi_store::provision::create_user(&pg, &model)
        .await
        .unwrap();
    let token = format!("sk-{model}");
    let key = okapi_store::provision::create_api_key(&pg, user, &hash(&token), "bridge")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let (send, incoming) = mpsc::channel(16);
    let get_status = Arc::new(AtomicU16::new(405));
    let gets = Arc::new(AtomicUsize::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let status = get_status.clone();
    let get_count = gets.clone();
    let post_count = posts.clone();
    let upstream = serve(
        Router::new().fallback(
            get(move || {
                get_count.fetch_add(1, Ordering::SeqCst);
                let status = status.load(Ordering::SeqCst);
                async move {
                    (
                        StatusCode::from_u16(status).unwrap(),
                        axum::Json(json!({"error":{"code":"ws_unsupported"}})),
                    )
                }
            })
            .post(
                move |headers: HeaderMap, uri: Uri, axum::Json(body): axum::Json<Value>| {
                    let send = send.clone();
                    post_count.fetch_add(1, Ordering::SeqCst);
                    async move {
                        let (reply, receive) = oneshot::channel();
                        send.send(Pending {
                            body,
                            headers,
                            path: uri.path().to_owned(),
                            reply,
                        })
                        .await
                        .unwrap();
                        receive.await.unwrap()
                    }
                },
            ),
        ),
    )
    .await;
    let (channel, channel_key) = channel(&pg, &model, upstream).await;
    let snapshot = serde_json::to_value(
        okapi_store::pricing::load_pricing_source_rows(&pg)
            .await
            .unwrap(),
    )
    .unwrap();
    okapi_store::admin::publish_epoch(&pg, user, &snapshot)
        .await
        .unwrap();
    let state = gateway::build_state(&database, &redis, &model, None, None)
        .await
        .unwrap();
    state
        .settings_cache
        .insert(
            "responses_ws_transport".into(),
            Arc::new(Some(json!("http"))),
        )
        .await;
    state
        .ledger
        .credit(user, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let address = serve(gateway::router(state.clone())).await;
    Env {
        state,
        address,
        incoming,
        get_status,
        gets,
        posts,
        model,
        token,
        user,
        key,
        channel,
        channel_key,
    }
}
async fn channel(pg: &sqlx::PgPool, model: &str, upstream: SocketAddr) -> (i64, i64) {
    let (channel, channel_key) = okapi_store::provision::create_channel(
        pg,
        model,
        "openai",
        &format!("http://{upstream}/v1"),
        "bridge-credential",
        &[model],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query(r#"UPDATE channels SET retry_policy='{"same_key_retries":2,"first_output_timeout_secs":2}' WHERE id=$1"#).bind(channel).execute(pg).await.unwrap();
    sqlx::query("UPDATE channel_keys SET max_concurrency=4 WHERE id=$1")
        .bind(channel_key)
        .execute(pg)
        .await
        .unwrap();
    (channel, channel_key)
}
impl Env {
    async fn client(&self) -> Client {
        let mut request = format!("ws://{}/v1/responses", self.address)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        timeout(WAIT, tokio_tungstenite::connect_async(request))
            .await
            .unwrap()
            .unwrap()
            .0
    }
    fn body(&self, lane: &str) -> Value {
        json!({"type":"response.create","model":self.model,"stream_id":lane,"input":"hello","store":false,"max_output_tokens":64})
    }
    async fn peer(&mut self) -> Pending {
        timeout(WAIT, self.incoming.recv()).await.unwrap().unwrap()
    }
    async fn mode(&self, value: &str) {
        self.state
            .settings_cache
            .insert(
                "responses_ws_transport".into(),
                Arc::new(Some(json!(value))),
            )
            .await;
    }
    async fn idle(&self) {
        self.state.settlements.wait_idle(WAIT).await;
        assert_eq!(self.state.settlements.in_flight(), 0);
        assert!(
            self.state
                .ledger
                .list_reservations(self.user)
                .await
                .unwrap()
                .is_empty()
        );
    }
    async fn amount(&self, event: &Value) -> (i64, Option<i16>, Option<String>) {
        self.idle().await;
        sqlx::query_as("SELECT amount_micro, upstream_status, error_code FROM billing_records WHERE user_id=$1 AND request_id=$2")
            .bind(self.user).bind(Uuid::parse_str(event["okapi_request_id"].as_str().unwrap()).unwrap()).fetch_one(&self.state.pg).await.unwrap()
    }
    async fn no_post(&mut self) {
        assert!(
            timeout(Duration::from_millis(100), self.incoming.recv())
                .await
                .is_err()
        );
    }
}
async fn send(client: &mut Client, body: Value) {
    client
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
}
async fn read(client: &mut Client) -> Value {
    loop {
        match timeout(WAIT, client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected text, got {other:?}"),
        }
    }
}
fn frame(value: &Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap()
    )
}
fn completed(output: Value) -> Value {
    let mut value = json!({"type":"response.completed","response":{"id":format!("resp_{}",Uuid::new_v4().simple()),"object":"response","status":"completed","output":null,"usage":{"input_tokens":100,"output_tokens":20}}});
    value["response"]["output"] = output;
    value
}

#[tokio::test]
async fn replay_preserves_opaque_items_tools_and_new_instructions() {
    let mut env = setup().await;
    sqlx::query("UPDATE channels SET model_mapping=$2 WHERE id=$1")
        .bind(env.channel)
        .bind(json!({&env.model:"mapped-model"}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut client = env.client().await;
    let mut body = env.body("main");
    body["instructions"] = "first instructions".into();
    send(&mut client, body).await;
    let peer = env.peer().await;
    assert_eq!(peer.path, "/v1/responses");
    assert_eq!(peer.headers["authorization"], "Bearer bridge-credential");
    assert_eq!(peer.body["model"], "mapped-model");
    assert_eq!(peer.body["stream"], true);
    for field in ["type", "stream_id", "generate"] {
        assert!(peer.body.get(field).is_none());
    }
    let output = json!([
        {"type":"reasoning","id":"rs_1","encrypted_content":"opaque-reasoning","summary":[]},
        {"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":"{\"city\":\"北京\"}"}
    ]);
    let first = completed(output.clone());
    peer.respond(std::slice::from_ref(&first));
    let event = read(&mut client).await;
    assert_eq!(event["stream_id"], "main");
    assert_eq!(event["response"], first["response"]);
    assert_eq!(env.amount(&event).await.0, 240);
    let request_id: Option<String> =
        sqlx::query_scalar("SELECT upstream_request_id FROM billing_records WHERE request_id=$1")
            .bind(Uuid::parse_str(event["okapi_request_id"].as_str().unwrap()).unwrap())
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(request_id.as_deref(), Some("http-bridge-fixture"));
    let tool = json!({"type":"function_call_output","call_id":"call_1","output":"sunny"});
    let mut next = env.body("main");
    next["previous_response_id"] = first["response"]["id"].clone();
    next["input"] = json!([tool]);
    send(&mut client, next).await;
    let peer = env.peer().await;
    assert_eq!(
        peer.body["input"],
        json!([{"role":"user","content":"hello"}, output[0], output[1], tool])
    );
    assert!(peer.body.get("previous_response_id").is_none());
    assert!(peer.body.get("instructions").is_none());
    assert_eq!(peer.body["store"], false);
    peer.respond(&[completed(json!([]))]);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 240);
    assert_eq!(env.gets.load(Ordering::SeqCst), 0);
    assert_eq!(env.posts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn local_warmup_is_free_and_cannot_be_reused_by_another_connection() {
    let mut env = setup().await;
    let mut client = env.client().await;
    let mut body = env.body("main");
    body["generate"] = false.into();
    send(&mut client, body).await;
    assert_eq!(read(&mut client).await["type"], "response.created");
    let warmup = read(&mut client).await;
    assert_eq!(warmup["response"]["okapi_warmup"], "local");
    assert_eq!(env.amount(&warmup).await.0, 0);
    env.no_post().await;
    let mut next = env.body("main");
    next["input"] = "next".into();
    next["previous_response_id"] = warmup["response"]["id"].clone();
    let mut other = env.client().await;
    send(&mut other, next.clone()).await;
    assert_eq!(read(&mut other).await["status"], 404);
    env.no_post().await;
    send(&mut client, next).await;
    let peer = env.peer().await;
    assert_eq!(
        peer.body["input"],
        json!([{"role":"user","content":"hello"},{"role":"user","content":"next"}])
    );
    assert!(peer.body.get("previous_response_id").is_none());
    peer.respond(&[completed(json!([]))]);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 240);
}

#[tokio::test]
async fn unsupported_handshake_falls_back_once_and_post_errors_never_retry() {
    let mut env = setup().await;
    env.mode("auto").await;
    let mut client = env.client().await;
    send(&mut client, env.body("main")).await;
    env.peer().await.error(429);
    let failure = read(&mut client).await;
    assert_eq!(failure["status"], 429);
    assert_eq!(failure["error"]["code"], "fixture_failure");
    assert_eq!(
        env.amount(&failure).await,
        (0, Some(429), Some("upstream_status_429".into()))
    );
    assert_eq!(env.gets.load(Ordering::SeqCst), 1);
    assert_eq!(env.posts.load(Ordering::SeqCst), 1);
    env.no_post().await;
    send(&mut client, env.body("main")).await;
    env.peer().await.respond(&[completed(json!([]))]);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 240);
    assert_eq!(env.gets.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn explicit_native_override_and_auth_failures_do_not_http_fallback() {
    let mut env = setup().await;
    sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1")
        .bind(env.channel)
        .bind(json!({"responses_ws_transport":"native"}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut client = env.client().await;
    send(&mut client, env.body("main")).await;
    assert_eq!(env.amount(&read(&mut client).await).await.0, 0);
    env.no_post().await;
    env.mode("auto").await;
    sqlx::query("UPDATE channels SET settings='{}' WHERE id=$1")
        .bind(env.channel)
        .execute(&env.state.pg)
        .await
        .unwrap();
    for status in [401, 429, 503] {
        env.get_status.store(status, Ordering::SeqCst);
        // Fresh eligibility still excludes cooldowns from the preceding controlled error.
        sqlx::query("UPDATE channel_keys SET status=1, cooldown_until=NULL WHERE id=$1")
            .bind(env.channel_key)
            .execute(&env.state.pg)
            .await
            .unwrap();
        send(&mut client, env.body("main")).await;
        assert_eq!(env.amount(&read(&mut client).await).await.0, 0);
        env.no_post().await;
    }
    assert_eq!(env.posts.load(Ordering::SeqCst), 0);
    assert_eq!(env.gets.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn late_headers_and_disconnect_drain_usage_without_resending() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body("main")).await;
    let peer = env.peer().await;
    let timed_out = read(&mut client).await;
    assert_eq!(timed_out["status"], 504);
    peer.respond(&[completed(json!([]))]);
    assert_eq!(
        env.amount(&timed_out).await,
        (240, Some(504), Some("upstream_timeout".into()))
    );
    send(&mut client, env.body("main")).await;
    let stream = env.peer().await.stream();
    stream
        .send(Ok(Bytes::from(frame(
            &json!({"type":"response.output_text.delta","delta":"hello"}),
        ))))
        .await
        .unwrap();
    let delta = read(&mut client).await;
    client.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    stream
        .send(Ok(Bytes::from(frame(&completed(json!([]))))))
        .await
        .unwrap();
    assert_eq!(env.amount(&delta).await.0, 240);
    assert_eq!(env.posts.load(Ordering::SeqCst), 2);
    env.no_post().await;
    let permit = okapi::gateway::sched_redis::channel_permit::ChannelPermit::acquire_key(
        &env.state.sched,
        env.channel_key,
        Some(4),
    )
    .await
    .unwrap()
    .unwrap();
    permit.release().await;
}

#[tokio::test]
async fn named_lanes_are_parallel_but_each_lane_waits_for_its_settlement() {
    let mut env = setup().await;
    let mut client = env.client().await;
    let mut a1 = env.body("a");
    a1["input"] = "a1".into();
    send(&mut client, a1).await;
    let mut a2 = env.body("a");
    a2["input"] = "a2".into();
    send(&mut client, a2).await;
    let mut b = env.body("b");
    b["input"] = "b".into();
    send(&mut client, b).await;
    let first = env.peer().await;
    let second = env.peer().await;
    let (a, b) = if first.body["input"][0]["content"] == "a1" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(b.body["input"][0]["content"], "b");
    env.no_post().await;
    b.respond(&[completed(json!([]))]);
    assert_eq!(read(&mut client).await["stream_id"], "b");
    a.respond(&[completed(json!([]))]);
    let a = read(&mut client).await;
    assert_eq!(a["stream_id"], "a");
    let peer = env.peer().await;
    assert_eq!(peer.body["input"][0]["content"], "a2");
    peer.respond(&[completed(json!([]))]);
    let second = read(&mut client).await;
    assert_ne!(a["okapi_request_id"], second["okapi_request_id"]);
    assert_eq!(env.amount(&second).await.0, 240);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        10_000_000 - 720
    );
}

#[tokio::test]
async fn external_stored_parent_is_preserved_and_another_key_cannot_use_it() {
    let mut env = setup().await;
    let mut client = env.client().await;
    let mut body = env.body("main");
    body["store"] = true.into();
    send(&mut client, body).await;
    let original = completed(json!([]));
    env.peer().await.respond(std::slice::from_ref(&original));
    env.amount(&read(&mut client).await).await;
    let mut other = env.client().await;
    let mut next = env.body("new");
    next["previous_response_id"] = original["response"]["id"].clone();
    next["input"] = "part2".into();
    send(&mut other, next.clone()).await;
    let peer = env.peer().await;
    assert_eq!(
        peer.body["previous_response_id"],
        original["response"]["id"]
    );
    assert_eq!(
        peer.body["input"],
        json!([{"role":"user","content":"part2"}])
    );
    let second = completed(json!([{"type":"message","role":"assistant","content":[]} ]));
    peer.respond(std::slice::from_ref(&second));
    env.amount(&read(&mut other).await).await;
    next["previous_response_id"] = second["response"]["id"].clone();
    next["input"] = "part3".into();
    send(&mut other, next.clone()).await;
    let peer = env.peer().await;
    assert_eq!(
        peer.body["previous_response_id"],
        original["response"]["id"]
    );
    assert_eq!(
        peer.body["input"],
        json!([{"role":"user","content":"part2"},second["response"]["output"][0],{"role":"user","content":"part3"}])
    );
    peer.respond(&[completed(json!([]))]);
    env.amount(&read(&mut other).await).await;
    env.token = format!("sk-other-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(&env.state.pg, env.user, &hash(&env.token), "other")
        .await
        .unwrap();
    let mut denied = env.client().await;
    send(&mut denied, next).await;
    assert_eq!(read(&mut denied).await["status"], 404);
    env.no_post().await;
}

#[tokio::test]
async fn cross_lane_failure_keeps_parent_same_lane_failure_evicts_it() {
    let mut env = setup().await;
    let mut client = env.client().await;
    let mut warmup = env.body("main");
    warmup["generate"] = false.into();
    send(&mut client, warmup).await;
    read(&mut client).await;
    let parent = read(&mut client).await;
    env.amount(&parent).await;
    let mut next = env.body("fork");
    next["previous_response_id"] = parent["response"]["id"].clone();
    send(&mut client, next.clone()).await;
    env.peer().await.error(400);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 0);
    next["stream_id"] = "main".into();
    send(&mut client, next.clone()).await;
    let peer = env.peer().await;
    assert!(peer.body.get("previous_response_id").is_none());
    peer.error(400);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 0);
    send(&mut client, next).await;
    assert_eq!(read(&mut client).await["status"], 404);
    env.no_post().await;
}

#[tokio::test]
async fn bounded_context_rejects_before_post_or_drains_known_usage_after_overflow() {
    let mut env = setup().await;
    env.state
        .settings_cache
        .insert(
            "responses_ws_context_bytes".into(),
            Arc::new(Some(json!(400))),
        )
        .await;
    let mut client = env.client().await;
    let mut body = env.body("main");
    body["input"] = "x".repeat(800).into();
    send(&mut client, body).await;
    let error = read(&mut client).await;
    assert_eq!(error["status"], 413);
    assert_eq!(env.amount(&error).await.0, 0);
    env.no_post().await;
    send(&mut client, env.body("main")).await;
    env.peer().await.respond(&[completed(
        json!([{"type":"message","content":[{"type":"output_text","text":"x".repeat(800)}]}]),
    )]);
    let error = read(&mut client).await;
    assert_eq!(error["status"], 413);
    assert_eq!(env.amount(&error).await.0, 240);
    env.no_post().await;
}

#[tokio::test]
async fn output_item_done_retains_complete_items_when_terminal_output_is_omitted() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body("main")).await;
    let mut terminal = completed(json!([]));
    terminal["response"]
        .as_object_mut()
        .unwrap()
        .remove("output");
    let item = json!({"type":"reasoning","encrypted_content":"opaque","summary":[]});
    env.peer().await.respond(&[
        json!({"type":"response.created","response":{"id":terminal["response"]["id"],"status":"in_progress"}}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}), terminal.clone()
    ]);
    read(&mut client).await;
    read(&mut client).await;
    env.amount(&read(&mut client).await).await;
    let mut next = env.body("main");
    next["previous_response_id"] = terminal["response"]["id"].clone();
    send(&mut client, next).await;
    let peer = env.peer().await;
    assert_eq!(peer.body["input"][1], item);
    peer.respond(&[completed(json!([]))]);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 240);
}

#[tokio::test]
async fn key_permissions_and_channel_transport_are_rechecked_each_turn() {
    let mut env = setup().await;
    env.mode("native").await;
    sqlx::query("UPDATE channels SET settings=$2, capabilities=$3 WHERE id=$1")
        .bind(env.channel)
        .bind(json!({"responses_ws_transport":"http"}))
        .bind(json!({"responses_websocket":false}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut client = env.client().await;
    send(&mut client, env.body("main")).await;
    env.peer().await.respond(&[completed(json!([]))]);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 240);
    sqlx::query("UPDATE api_keys SET model_allowlist=$2 WHERE id=$1")
        .bind(env.key)
        .bind(json!(["denied-model"]))
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state.sched.auth_del(&hash(&env.token)).await;
    send(&mut client, env.body("main")).await;
    assert_eq!(read(&mut client).await["status"], 403);
    env.idle().await;
    env.no_post().await;
    sqlx::query("UPDATE api_keys SET model_allowlist=$2 WHERE id=$1")
        .bind(env.key)
        .bind(json!([env.model]))
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state.sched.auth_del(&hash(&env.token)).await;
    sqlx::query("UPDATE channels SET capabilities=$2 WHERE id=$1")
        .bind(env.channel)
        .bind(json!({"responses_http":false}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    send(&mut client, env.body("main")).await;
    let error = read(&mut client).await;
    assert_eq!(error["status"], 503);
    assert_eq!(env.amount(&error).await.0, 0);
    env.no_post().await;
    assert_eq!(env.gets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_warmup_does_not_charge_the_flat_model_fee() {
    let mut env = setup().await;
    okapi_store::admin::upsert_model_per_call(&env.state.pg, &env.model, 5000)
        .await
        .unwrap();
    let snapshot = serde_json::to_value(
        okapi_store::pricing::load_pricing_source_rows(&env.state.pg)
            .await
            .unwrap(),
    )
    .unwrap();
    okapi_store::admin::publish_epoch(&env.state.pg, env.user, &snapshot)
        .await
        .unwrap();
    assert!(
        gateway::refresh_pricebook_if_newer(&env.state)
            .await
            .unwrap()
    );
    let mut client = env.client().await;
    let mut warmup = env.body("main");
    warmup["generate"] = false.into();
    send(&mut client, warmup).await;
    read(&mut client).await;
    let parent = read(&mut client).await;
    assert_eq!(env.amount(&parent).await.0, 0);
    env.no_post().await;
    let mut next = env.body("main");
    next["previous_response_id"] = parent["response"]["id"].clone();
    send(&mut client, next).await;
    env.peer().await.respond(&[completed(json!([]))]);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 5000);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        10_000_000 - 5000
    );
}

#[tokio::test]
async fn codex_http_uses_oauth_identity_and_preserves_encrypted_items() {
    let mut env = setup().await;
    let credential = okapi_store::credential::OAuthCredential {
        access_token: "bridge-access".into(),
        refresh_token: "bridge-refresh".into(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        account_id: Some("bridge-account".into()),
        account_label: None,
        scope: None,
    };
    sqlx::query("UPDATE channels SET provider='codex' WHERE id=$1")
        .bind(env.channel)
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channel_keys SET credential_ciphertext=$2 WHERE id=$1")
        .bind(env.channel_key)
        .bind(credential.to_plaintext().into_bytes())
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut client = env.client().await;
    let mut body = env.body("main");
    body["input"] = json!([{"type":"reasoning","encrypted_content":"opaque"},{"type":"message","role":"system","content":"instruction"}]);
    send(&mut client, body).await;
    let peer = env.peer().await;
    assert_eq!(peer.headers["authorization"], "Bearer bridge-access");
    assert_eq!(peer.headers["chatgpt-account-id"], "bridge-account");
    assert_eq!(peer.body["input"][0]["encrypted_content"], "opaque");
    assert_eq!(peer.body["input"][1]["role"], "developer");
    assert_eq!(peer.body["store"], false);
    assert_eq!(peer.body["stream"], true);
    assert_eq!(peer.body["instructions"], "");
    peer.respond(&[completed(json!([]))]);
    assert_eq!(env.amount(&read(&mut client).await).await.0, 240);
}

#[tokio::test]
async fn invalid_sse_and_redirects_fail_without_replaying_the_post() {
    let mut env = setup().await;
    let mut client = env.client().await;
    for (status, content_type, body) in [
        (200, "application/json", "{}"),
        (
            200,
            "text/event-stream",
            "event: response.completed\ndata: broken-json\n\n",
        ),
        (302, "application/json", "{}"),
        (
            400,
            "application/json",
            r#"{"stream_id":"wrong-lane","error":{"code":"bad_input"}}"#,
        ),
    ] {
        let mut request = env.body("main");
        if status == 400 {
            request.as_object_mut().unwrap().remove("stream_id");
        }
        send(&mut client, request).await;
        env.peer()
            .await
            .reply
            .send(
                Response::builder()
                    .status(status)
                    .header("content-type", content_type)
                    .header("location", "/v1/responses")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .unwrap();
        let error = read(&mut client).await;
        assert_eq!(error["status"], if status == 400 { 400 } else { 502 });
        if status == 400 {
            assert!(error.get("stream_id").is_none());
        }
        assert_eq!(env.amount(&error).await.0, 0);
        env.no_post().await;
    }
    assert_eq!(env.posts.load(Ordering::SeqCst), 4);
    assert_eq!(env.gets.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn inconsistent_response_ids_or_terminal_items_fail_but_reported_usage_is_billed() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body("main")).await;
    env.peer().await.respond(&[
        json!({"type":"response.created","response":{"id":format!("resp_{}",Uuid::new_v4().simple())}}),
        completed(json!([])),
    ]);
    read(&mut client).await;
    let error = read(&mut client).await;
    assert_eq!(error["status"], 502);
    assert_eq!(env.amount(&error).await.0, 240);
    send(&mut client, env.body("main")).await;
    env.peer().await.respond(&[
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","encrypted_content":"must-not-drop"}}),
        completed(json!([{"type":"message","role":"assistant","content":[]}])),
    ]);
    read(&mut client).await;
    let error = read(&mut client).await;
    assert_eq!(error["status"], 502);
    assert_eq!(env.amount(&error).await.0, 240);
    assert_eq!(env.posts.load(Ordering::SeqCst), 2);
    env.no_post().await;
}

/// WS 轮次与 HTTP 同一规则：没写输出上限、模型 max_output 又大于预扣封顶时，
/// 转发前补上预扣用的 32768；写了的原样转发。
#[tokio::test]
async fn omitted_output_cap_is_bounded_on_websocket_turns() {
    let mut env = setup().await;
    env.mode("http").await;
    sqlx::query("UPDATE models SET max_output = 128000 WHERE model_name = $1")
        .bind(&env.model)
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut client = env.client().await;
    let mut open = env.body("main");
    open.as_object_mut().unwrap().remove("max_output_tokens");
    send(&mut client, open).await;
    let peer = env.peer().await;
    assert_eq!(peer.body["max_output_tokens"], 32_768, "{}", peer.body);
    peer.respond(&[completed(json!([]))]);
    read(&mut client).await;
    send(&mut client, env.body("main")).await;
    let peer = env.peer().await;
    assert_eq!(peer.body["max_output_tokens"], 64, "{}", peer.body);
    peer.respond(&[completed(json!([]))]);
    read(&mut client).await;
}
