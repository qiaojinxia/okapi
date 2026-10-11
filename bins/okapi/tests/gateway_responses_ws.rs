//! Public WS gateway with real PG/Redis, real client upgrades and controlled upstream frames.
use axum::{
    Router,
    extract::ws::{Message as ServerMessage, WebSocket, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
};
use futures::{SinkExt, StreamExt};
use okapi::gateway::sched_redis::channel_permit::ChannelPermit;
use okapi::{gateway, gateway::state::AppState};
use okapi_domain::Money;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type Accepted = (WebSocket, HeaderMap, String);
const WAIT: Duration = Duration::from_secs(10);

fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

struct Env {
    state: AppState,
    address: SocketAddr,
    accepted: mpsc::Receiver<Accepted>,
    handshake_failure: Arc<AtomicU16>,
    model: String,
    token: String,
    user: i64,
    key: i64,
    channel: i64,
    channel_key: i64,
}
async fn publish_pricing(pg: &sqlx::PgPool, user: i64) {
    let snapshot = serde_json::to_value(
        okapi_store::pricing::load_pricing_source_rows(pg)
            .await
            .unwrap(),
    )
    .unwrap();
    okapi_store::admin::publish_epoch(pg, user, &snapshot)
        .await
        .unwrap();
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let tag = Uuid::new_v4().simple().to_string();
    let model = format!("ws-{tag}");
    let user = okapi_store::provision::create_user(&pg, &model)
        .await
        .unwrap();
    let token = format!("sk-{model}");
    let key = okapi_store::provision::create_api_key(&pg, user, &hash(&token), "ws")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    publish_pricing(&pg, user).await;
    let (send, accepted) = mpsc::channel(8);
    let handshake_failure = Arc::new(AtomicU16::new(0));
    let failure = handshake_failure.clone();
    let upstream = serve(Router::new().fallback(get(
        move |headers: HeaderMap, uri: Uri, ws: WebSocketUpgrade| {
            let send = send.clone();
            let failure = failure.clone();
            async move {
                let code = failure.load(Ordering::SeqCst);
                if uri.path().starts_with("/a/") && code != 0 {
                    return (
                        StatusCode::from_u16(code).unwrap(),
                        axum::Json(json!({"error":{"code":"fixture"}})),
                    )
                        .into_response();
                }
                ws.on_upgrade(move |socket| async move {
                    let _ = send.send((socket, headers, uri.path().to_owned())).await;
                })
            }
        },
    )))
    .await;
    let (channel, channel_key) = okapi_store::provision::create_channel(
        &pg,
        &format!("a-{model}"),
        "openai",
        &format!("http://{upstream}/a"),
        "credential-a",
        &[&model],
        false,
        None,
    )
    .await
    .unwrap();
    let (secondary, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("b-{model}"),
        "openai",
        &format!("http://{upstream}/b"),
        "credential-b",
        &[&model],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query(r#"UPDATE channels SET priority=CASE WHEN id=$1 THEN 100 ELSE 1 END, retry_policy='{"same_key_retries":0,"first_output_timeout_secs":5}' WHERE id=ANY($2)"#)
        .bind(channel).bind(vec![channel, secondary]).execute(&pg).await.unwrap();
    sqlx::query("UPDATE channel_keys SET max_concurrency=4 WHERE id=$1")
        .bind(channel_key)
        .execute(&pg)
        .await
        .unwrap();
    let state = gateway::build_state(&database, &redis, &model, None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let address = serve(gateway::router(state.clone())).await;
    Env {
        state,
        address,
        accepted,
        handshake_failure,
        model,
        token,
        user,
        key,
        channel,
        channel_key,
    }
}
impl Env {
    fn request(&self) -> tokio_tungstenite::tungstenite::http::Request<()> {
        let mut request = format!("ws://{}/v1/responses", self.address)
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        request
    }
    async fn client(&self) -> Client {
        timeout(WAIT, tokio_tungstenite::connect_async(self.request()))
            .await
            .unwrap()
            .unwrap()
            .0
    }
    async fn peer(&mut self) -> Accepted {
        timeout(WAIT, self.accepted.recv()).await.unwrap().unwrap()
    }
    fn body(&self, lane: Option<&str>) -> Value {
        let mut value = json!({"type":"response.create","model":self.model,"input":"hello","store":false,"max_output_tokens":64});
        if let Some(lane) = lane {
            value["stream_id"] = lane.into();
        }
        value
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
    async fn balance(&self) -> i64 {
        self.state
            .ledger
            .balance(self.user)
            .await
            .unwrap()
            .as_micros()
    }
    async fn amount(&self, request: &Value) -> (i64, Option<i16>, Option<String>) {
        self.idle().await;
        sqlx::query_as("SELECT amount_micro, upstream_status, error_code FROM billing_records WHERE user_id=$1 AND request_id=$2")
            .bind(self.user).bind(Uuid::parse_str(request["okapi_request_id"].as_str().unwrap()).unwrap()).fetch_one(&self.state.pg).await.unwrap()
    }
}
async fn send(client: &mut Client, value: Value) {
    client
        .send(Message::Text(value.to_string().into()))
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
            Message::Text(raw) => return serde_json::from_str(&raw).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected text, got {other:?}"),
        }
    }
}
async fn upstream_read(peer: &mut WebSocket) -> Value {
    let ServerMessage::Text(raw) = timeout(WAIT, peer.recv()).await.unwrap().unwrap().unwrap()
    else {
        panic!("expected request")
    };
    serde_json::from_str(&raw).unwrap()
}
async fn emit(peer: &mut WebSocket, mut value: Value, lane: Option<&str>) {
    if let Some(lane) = lane {
        value["stream_id"] = lane.into();
    }
    peer.send(ServerMessage::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn completed(peer: &mut WebSocket, lane: Option<&str>, output: u32) -> String {
    let id = format!("resp_{}", Uuid::new_v4().simple());
    emit(peer, json!({"type":"response.completed","response":{"id":id,"object":"response","model":"observed-ws-model","status":"completed","usage":{"input_tokens":100,"output_tokens":output}}}), lane).await;
    id
}

#[tokio::test]
async fn upgrade_authentication_connection_cap_and_release() {
    let env = setup().await;
    let unauth = format!("ws://{}/v1/responses", env.address);
    let error = tokio_tungstenite::connect_async(unauth).await.unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == 401)
    );
    let mut clients = Vec::new();
    for _ in 0..4 {
        clients.push(env.client().await);
    }
    let error = tokio_tungstenite::connect_async(env.request())
        .await
        .unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == 429)
    );
    for mut client in clients {
        client.close(None).await.unwrap();
    }
    let mut replacement = timeout(WAIT, async {
        loop {
            if let Ok((client, _)) = tokio_tungstenite::connect_async(env.request()).await {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    replacement.close(None).await.unwrap();
    assert_eq!(env.balance().await, 10_000_000);
}

#[tokio::test]
async fn warmup_history_and_per_turn_authorization_and_billing() {
    let mut env = setup().await;
    let mut client = env.client().await;
    let mut warmup = env.body(Some("main"));
    warmup["generate"] = false.into();
    send(&mut client, warmup.clone()).await;
    let (mut peer, headers, path) = env.peer().await;
    assert_eq!(path, "/a/responses");
    assert_eq!(headers["authorization"], "Bearer credential-a");
    assert_eq!(upstream_read(&mut peer).await, warmup);
    let id = completed(&mut peer, Some("main"), 0).await;
    let first = read(&mut client).await;
    assert_eq!(first["response"]["id"], id);
    assert_eq!(env.amount(&first).await.0, 200);
    let mut next = env.body(Some("main"));
    next["previous_response_id"] = id.into();
    send(&mut client, next.clone()).await;
    assert_eq!(upstream_read(&mut peer).await, next);
    completed(&mut peer, Some("main"), 20).await;
    let second = read(&mut client).await;
    assert_ne!(first["okapi_request_id"], second["okapi_request_id"]);
    assert_eq!(env.amount(&second).await.0, 240);
    let record: (String, Value) =
        sqlx::query_as("SELECT model_name,usage_details FROM billing_records WHERE request_id=$1")
            .bind(Uuid::parse_str(second["okapi_request_id"].as_str().unwrap()).unwrap())
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(record.0, env.model);
    assert_eq!(
        record.1["diagnostics"]["response_model"],
        "observed-ws-model"
    );
    assert_eq!(record.1["diagnostics"]["attempts"][0]["outcome"], "success");
    assert_eq!(env.balance().await, 10_000_000 - 440);
    sqlx::query("UPDATE api_keys SET model_allowlist=$2 WHERE id=$1")
        .bind(env.key)
        .bind(json!(["not-allowed"]))
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state.sched.auth_del(&hash(&env.token)).await;
    send(&mut client, env.body(Some("main"))).await;
    let denied = read(&mut client).await;
    assert_eq!(denied["status"], 403);
    assert_eq!(denied["stream_id"], "main");
    assert!(
        timeout(Duration::from_millis(100), peer.recv())
            .await
            .is_err()
    );
    env.idle().await;
    assert_eq!(env.balance().await, 10_000_000 - 440);
}

#[tokio::test]
async fn same_lane_fifo_other_lanes_parallel_and_independent_settlement() {
    let mut env = setup().await;
    let mut client = env.client().await;
    let mut a = env.body(Some("a"));
    a["input"] = "a1".into();
    send(&mut client, a.clone()).await;
    a["input"] = "a2".into();
    send(&mut client, a).await;
    send(&mut client, env.body(Some("b"))).await;
    let (mut peer, _, _) = env.peer().await;
    let one = upstream_read(&mut peer).await;
    let two = upstream_read(&mut peer).await;
    assert!(one["input"] == "a1" || two["input"] == "a1");
    assert!(one["stream_id"] == "b" || two["stream_id"] == "b");
    assert!(
        timeout(Duration::from_millis(100), peer.recv())
            .await
            .is_err()
    );
    completed(&mut peer, Some("b"), 20).await;
    let b = read(&mut client).await;
    assert_eq!(b["stream_id"], "b");
    completed(&mut peer, Some("a"), 20).await;
    let a1 = read(&mut client).await;
    assert_eq!(upstream_read(&mut peer).await["input"], "a2");
    completed(&mut peer, Some("a"), 20).await;
    let a2 = read(&mut client).await;
    for record in [&a1, &a2, &b] {
        assert_eq!(env.amount(record).await.0, 240);
    }
    assert_ne!(a1["okapi_request_id"], a2["okapi_request_id"]);
    assert_eq!(env.balance().await, 10_000_000 - 720);
    assert!(env.accepted.try_recv().is_err());
}

#[tokio::test]
async fn root_error_refunds_failed_usage_is_billed_and_neither_is_replayed() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    emit(
        &mut peer,
        json!({"type":"error","status":429,"error":{"code":"fixture_rate_limit","message":"busy"}}),
        None,
    )
    .await;
    let failure = read(&mut client).await;
    assert_eq!(failure["error"]["code"], "fixture_rate_limit");
    assert_eq!(env.amount(&failure).await.0, 0);
    assert_eq!(env.balance().await, 10_000_000);
    send(&mut client, env.body(None)).await;
    upstream_read(&mut peer).await;
    emit(&mut peer, json!({"type":"response.failed","response":{"id":format!("resp_{}",Uuid::new_v4()),"status":"failed","usage":{"input_tokens":100,"output_tokens":20},"error":{"code":"server_error"}}}), None).await;
    let failure = read(&mut client).await;
    assert_eq!(failure["type"], "response.failed");
    assert_eq!(
        env.amount(&failure).await,
        (240, Some(502), Some("upstream_error".into()))
    );
    assert_eq!(env.balance().await, 10_000_000 - 240);
    assert!(env.accepted.try_recv().is_err());
}

#[tokio::test]
async fn downstream_disconnect_drains_terminal_usage_and_releases_slots() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    emit(
        &mut peer,
        json!({"type":"response.output_text.delta","delta":"hello"}),
        None,
    )
    .await;
    let event = read(&mut client).await;
    client.close(None).await.unwrap();
    drop(client);
    tokio::time::sleep(Duration::from_millis(100)).await;
    completed(&mut peer, None, 20).await;
    assert_eq!(env.amount(&event).await.0, 240);
    assert_eq!(env.balance().await, 10_000_000 - 240);
    let mut permits = Vec::new();
    for _ in 0..4 {
        permits.push(
            ChannelPermit::acquire_key(&env.state.sched, env.channel_key, Some(4))
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(
        ChannelPermit::acquire_key(&env.state.sched, env.channel_key, Some(4))
            .await
            .unwrap()
            .is_none()
    );
    for permit in permits {
        permit.release().await;
    }
}

#[tokio::test]
async fn live_channel_changes_cannot_reuse_pinned_account_or_change_protocol_fields() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    completed(&mut peer, None, 20).await;
    env.amount(&read(&mut client).await).await;
    sqlx::query("UPDATE channel_keys SET credential_ciphertext='rotated' WHERE id=$1")
        .bind(env.channel_key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    send(&mut client, env.body(None)).await;
    let failure = read(&mut client).await;
    assert_eq!(failure["status"], 503);
    assert_eq!(env.amount(&failure).await.0, 0);
    assert!(env.accepted.try_recv().is_err());
    sqlx::query("UPDATE channel_keys SET credential_ciphertext='credential-a' WHERE id=$1")
        .bind(env.channel_key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET settings=$2 WHERE id=$1")
        .bind(env.channel)
        .bind(json!({"inject_request_fields":{"generate":true}}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let mut warmup = env.body(None);
    warmup["generate"] = false.into();
    send(&mut client, warmup).await;
    let failure = read(&mut client).await;
    assert_eq!(failure["status"], 400);
    assert_eq!(env.amount(&failure).await.0, 0);
    assert!(
        timeout(Duration::from_millis(100), peer.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn first_event_timeout_is_not_replayed_and_late_usage_still_settles() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    let failure = read(&mut client).await;
    assert_eq!(failure["status"], 504);
    completed(&mut peer, None, 20).await;
    assert_eq!(
        env.amount(&failure).await,
        (240, Some(504), Some("upstream_timeout".into()))
    );
    assert!(env.accepted.try_recv().is_err());
    assert!(
        timeout(Duration::from_millis(100), peer.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handshake_failover_is_allowed_before_create_and_stops_after_create() {
    let mut env = setup().await;
    env.handshake_failure.store(503, Ordering::SeqCst);
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, headers, path) = env.peer().await;
    assert_eq!(path, "/b/responses");
    assert_eq!(headers["authorization"], "Bearer credential-b");
    upstream_read(&mut peer).await;
    completed(&mut peer, None, 20).await;
    let result = read(&mut client).await;
    assert_eq!(env.amount(&result).await.0, 240);
    let failover: i16 =
        sqlx::query_scalar("SELECT failover_count FROM billing_records WHERE user_id=$1")
            .bind(env.user)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(failover, 1);
    send(&mut client, env.body(None)).await;
    upstream_read(&mut peer).await;
    peer.close().await.unwrap();
    let failure = read(&mut client).await;
    assert_eq!(env.amount(&failure).await.0, 0);
    assert!(env.accepted.try_recv().is_err());
}

#[tokio::test]
async fn malformed_frames_and_warmup_without_usage_do_not_fabricate_charges() {
    let mut env = setup().await;
    let mut client = env.client().await;
    let mut bad = env.body(Some("lane"));
    bad["stream"] = true.into();
    send(&mut client, bad).await;
    let rejected = read(&mut client).await;
    assert_eq!(rejected["status"], 400);
    assert_eq!(rejected["stream_id"], "lane");
    assert!(env.accepted.try_recv().is_err());
    let mut warmup = env.body(None);
    warmup["generate"] = false.into();
    send(&mut client, warmup).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    emit(&mut peer, json!({"type":"response.completed","response":{"id":format!("resp_{}",Uuid::new_v4()),"status":"completed"}}), None).await;
    let error = read(&mut client).await;
    assert_eq!(error["error"]["param"], "usage_missing");
    assert_eq!(env.amount(&error).await.0, 0);
    assert_eq!(env.balance().await, 10_000_000);
}

#[tokio::test]
async fn codex_credentials_headers_and_opaque_input_use_native_ws_shape() {
    let mut env = setup().await;
    let credential = okapi_store::credential::OAuthCredential {
        access_token: "access".into(),
        refresh_token: "refresh".into(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        account_id: Some("account".into()),
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
    let mut body = env.body(None);
    body["input"] = json!([{"type":"reasoning","encrypted_content":"opaque"},{"type":"message","role":"system","content":"test"}]);
    send(&mut client, body).await;
    let (mut peer, headers, _) = env.peer().await;
    assert_eq!(headers["authorization"], "Bearer access");
    assert_eq!(headers["chatgpt-account-id"], "account");
    let request = upstream_read(&mut peer).await;
    assert_eq!(request["input"][0]["encrypted_content"], "opaque");
    assert_eq!(request["input"][1]["role"], "developer");
    assert_eq!(request["store"], false);
    assert!(request.get("stream").is_none());
    completed(&mut peer, None, 20).await;
    assert_eq!(env.amount(&read(&mut client).await).await.0, 240);
}

#[tokio::test]
async fn broken_lease_storage_fails_closed_without_upstream_or_balance_changes() {
    use fred::interfaces::KeysInterface;
    let mut env = setup().await;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let key = format!("ws:responses:k:{}", env.key);
    let _: () = redis
        .set(&key, "wrongtype", None, None, false)
        .await
        .unwrap();
    let error = tokio_tungstenite::connect_async(env.request())
        .await
        .unwrap_err();
    assert!(
        matches!(error, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == 503)
    );
    assert!(env.accepted.try_recv().is_err());
    assert_eq!(env.balance().await, 10_000_000);
    let _: i64 = redis.del(&key).await.unwrap();
    let mut client = env.client().await;
    client.close(None).await.unwrap();
}

#[tokio::test]
async fn history_is_key_scoped_and_revoked_keys_cannot_start_another_turn() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    let id = completed(&mut peer, None, 20).await;
    env.amount(&read(&mut client).await).await;
    let foreign_token = format!("sk-foreign-{}", Uuid::new_v4());
    okapi_store::provision::create_api_key(
        &env.state.pg,
        env.user,
        &hash(&foreign_token),
        "foreign",
    )
    .await
    .unwrap();
    let mut request = env.request();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {foreign_token}").parse().unwrap(),
    );
    let mut foreign = tokio_tungstenite::connect_async(request).await.unwrap().0;
    for previous in [id.as_str(), "resp_unknown"] {
        let mut body = env.body(Some("fork"));
        body["previous_response_id"] = previous.into();
        send(&mut foreign, body).await;
        let error = read(&mut foreign).await;
        assert_eq!(error["status"], 404);
        assert_eq!(error["error"]["param"], "previous_response_id");
    }
    sqlx::query("UPDATE api_keys SET status=2 WHERE id=$1")
        .bind(env.key)
        .execute(&env.state.pg)
        .await
        .unwrap();
    env.state.sched.auth_del(&hash(&env.token)).await;
    send(&mut client, env.body(None)).await;
    assert_eq!(read(&mut client).await["status"], 401);
    env.idle().await;
    assert_eq!(env.balance().await, 10_000_000 - 240);
    assert!(env.accepted.try_recv().is_err());
    assert!(
        timeout(Duration::from_millis(100), peer.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disabled_pinned_channel_cannot_be_replaced_by_another_live_account() {
    let mut env = setup().await;
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    completed(&mut peer, None, 20).await;
    env.amount(&read(&mut client).await).await;
    sqlx::query("UPDATE channels SET status=2 WHERE id=$1")
        .bind(env.channel)
        .execute(&env.state.pg)
        .await
        .unwrap();
    send(&mut client, env.body(None)).await;
    let error = read(&mut client).await;
    assert_eq!(error["status"], 503);
    assert_eq!(env.amount(&error).await.0, 0);
    assert!(env.accepted.try_recv().is_err());
    assert!(
        timeout(Duration::from_millis(100), peer.recv())
            .await
            .is_err()
    );
    assert_eq!(env.balance().await, 10_000_000 - 240);
}

#[tokio::test]
async fn active_events_do_not_extend_turn_beyond_ledger_safe_limit() {
    let mut env = setup().await;
    env.state
        .settings_cache
        .insert(
            "responses_ws_turn_timeout_secs".into(),
            Arc::new(Some(json!(4))),
        )
        .await;
    let mut client = env.client().await;
    send(&mut client, env.body(None)).await;
    let (mut peer, _, _) = env.peer().await;
    upstream_read(&mut peer).await;
    for _ in 0..3 {
        emit(
            &mut peer,
            json!({"type":"response.in_progress","response":{}}),
            None,
        )
        .await;
        assert_eq!(read(&mut client).await["type"], "response.in_progress");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let error = read(&mut client).await;
    assert_eq!(error["status"], 504);
    assert_eq!(error["error"]["param"], "responses_ws_turn_timeout");
    completed(&mut peer, None, 20).await;
    assert_eq!(
        env.amount(&error).await,
        (240, Some(504), Some("upstream_timeout".into()))
    );
    assert_eq!(env.balance().await, 10_000_000 - 240);
    assert!(env.accepted.try_recv().is_err());
}
