//! 原生 Responses WS 传输层合同测试：真实 HTTP upgrade 与 WebSocket 帧。
//! 不经过 gateway，不作为 /v1/responses WS 鉴权/计费已完成的证据。

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
};
use bytes::Bytes;
use futures::StreamExt;
use okapi_providers::{
    ChatEvent, HttpPool, Outbound, StreamHandle, UpstreamError,
    responses_ws::{ResponsesSocket, SocketTimeouts},
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, time::timeout};

const WAIT: Duration = Duration::from_secs(3);
type Accepted = (WebSocket, HeaderMap, Uri);

async fn server() -> (SocketAddr, mpsc::Receiver<Accepted>) {
    let (send, receiver) = mpsc::channel(8);
    let app = Router::new().fallback(get(
        move |headers: HeaderMap, uri: Uri, ws: WebSocketUpgrade| {
            let send = send.clone();
            async move {
                let mut response = ws.on_upgrade(move |socket| async move {
                    let _ = send.send((socket, headers, uri)).await;
                });
                response
                    .headers_mut()
                    .insert("x-request-id", "ws-handshake".parse().unwrap());
                response
            }
        },
    ));
    (listen(app).await, receiver)
}

async fn listen(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn connect(addr: SocketAddr) -> ResponsesSocket {
    ResponsesSocket::connect(
        &HttpPool::new().unwrap(),
        &format!("ws://{addr}/v1/responses"),
        &[("authorization", "Bearer fixture-secret")],
        &Outbound::default(),
        SocketTimeouts::default(),
    )
    .await
    .unwrap()
}

async fn accept(receiver: &mut mpsc::Receiver<Accepted>) -> WebSocket {
    timeout(WAIT, receiver.recv()).await.unwrap().unwrap().0
}

fn request(lane: Option<&str>, input: &str) -> Bytes {
    let mut value =
        json!({"type":"response.create", "model":"fixture-model", "store":false, "input":input});
    if let Some(lane) = lane {
        value["stream_id"] = json!(lane);
    }
    Bytes::from(value.to_string())
}

async fn read(peer: &mut WebSocket) -> Value {
    match timeout(WAIT, peer.recv()).await.unwrap().unwrap().unwrap() {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected request frame, got {other:?}"),
    }
}

async fn emit(peer: &mut WebSocket, mut value: Value, lane: Option<&str>) {
    if let Some(lane) = lane {
        value["stream_id"] = json!(lane);
    }
    peer.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn terminal(peer: &mut WebSocket, lane: Option<&str>, id: &str, output: u32) {
    emit(peer, json!({"type":"response.completed", "response":{"id":id, "usage":{
        "input_tokens":10,"output_tokens":output,"input_tokens_details":{"cached_tokens":4},"output_tokens_details":{"reasoning_tokens":2}
    }}}), lane).await;
}

async fn collect(handle: StreamHandle) -> Vec<Result<ChatEvent, UpstreamError>> {
    timeout(WAIT, handle.events.collect())
        .await
        .expect("turn must finish")
}

fn raws(events: &[Result<ChatEvent, UpstreamError>]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event {
            Ok(ChatEvent::Data { raw, .. }) => Some(serde_json::from_str(raw).unwrap()),
            _ => None,
        })
        .collect()
}

fn finished(events: &[Result<ChatEvent, UpstreamError>]) {
    assert!(events.iter().all(Result::is_ok));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Ok(ChatEvent::Done)))
            .count(),
        1
    );
}

fn failed(events: &[Result<ChatEvent, UpstreamError>], code: &str) {
    assert!(!events.iter().any(|e| matches!(e, Ok(ChatEvent::Done))));
    let error = events
        .iter()
        .find_map(|e| e.as_ref().err())
        .expect("transport error required");
    assert!(
        matches!(error, UpstreamError::Session { reason, .. } if *reason == code),
        "{error:?}"
    );
    assert!(
        !error.retriable_before_first_token(),
        "uncertain execution cannot be replayed"
    );
    assert!(!error.is_transient());
}

#[tokio::test]
async fn persistent_warmup_continuation_and_fork_preserve_opaque_input() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    let warmup = json!({"type":"response.create", "stream_id":"main", "model":"fixture-model", "generate":false,"store":false,"input":[{"type":"reasoning","encrypted_content":"opaque"}],"tools":[{"type":"function","name":"tool","parameters":{"type":"object"}}]});
    let warm = socket
        .create(Bytes::from(warmup.to_string()))
        .await
        .unwrap();
    assert_eq!(warm.upstream_request_id.as_deref(), Some("ws-handshake"));
    assert_eq!(read(&mut peer).await, warmup);
    terminal(&mut peer, Some("main"), "resp_warm", 0).await;
    finished(&collect(warm).await);
    let next = json!({"type":"response.create", "stream_id":"main", "model":"fixture-model", "store":false,"previous_response_id":"resp_warm","input":[{"type":"function_call_output","call_id":"call_1","output":"result"}]});
    let turn = socket.create(Bytes::from(next.to_string())).await.unwrap();
    assert_eq!(read(&mut peer).await, next);
    terminal(&mut peer, Some("main"), "resp_next", 7).await;
    let events = collect(turn).await;
    finished(&events);
    let usage = events
        .iter()
        .find_map(|event| match event {
            Ok(ChatEvent::Data {
                usage: Some(usage), ..
            }) => Some(usage),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 7);
    assert_eq!(usage.prompt_tokens_details.cached_tokens, 4);
    assert_eq!(usage.completion_tokens_details.reasoning_tokens, 2);
    let fork = json!({"type":"response.create","stream_id":"critic","model":"fixture-model","previous_response_id":"resp_next","input":"review"});
    let branch = socket.create(Bytes::from(fork.to_string())).await.unwrap();
    assert_eq!(read(&mut peer).await, fork);
    terminal(&mut peer, Some("critic"), "resp_fork", 5).await;
    finished(&collect(branch).await);
    assert!(
        incoming.try_recv().is_err(),
        "all turns must reuse one connection"
    );
    socket.close();
}

#[tokio::test]
async fn interleaved_default_and_named_lanes_keep_events_and_errors_isolated() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    let default = socket.create(request(None, "default")).await.unwrap();
    assert!(read(&mut peer).await.get("stream_id").is_none());
    let named = socket
        .create(request(Some("worker"), "named"))
        .await
        .unwrap();
    assert_eq!(read(&mut peer).await["stream_id"], "worker");
    emit(
        &mut peer,
        json!({"type":"response.output_text.delta","delta":"only-worker"}),
        Some("worker"),
    )
    .await;
    emit(&mut peer, json!({"type":"error","status":400,"error":{"code":"previous_response_not_found","param":"previous_response_id"}}), None).await;
    terminal(&mut peer, Some("worker"), "resp_worker", 3).await;
    let default_events = collect(default).await;
    let named_events = collect(named).await;
    finished(&default_events);
    finished(&named_events);
    assert!(
        raws(&default_events)
            .iter()
            .all(|e| e.get("stream_id").is_none())
    );
    assert_eq!(raws(&default_events)[0]["type"], "error");
    assert!(
        raws(&named_events)
            .iter()
            .all(|e| e["stream_id"] == "worker")
    );
    assert_eq!(raws(&named_events)[0]["delta"], "only-worker");
    // A request-scoped error does not destroy the socket or another lane.
    let again = socket.create(request(None, "again")).await.unwrap();
    read(&mut peer).await;
    terminal(&mut peer, None, "resp_again", 1).await;
    finished(&collect(again).await);
}

#[tokio::test]
async fn concurrency_limit_queues_and_same_lane_is_fifo_without_implicit_history() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    let mut turns = Vec::new();
    for i in 0..17 {
        turns.push(
            socket
                .create(request(Some(&format!("lane-{i}")), "one"))
                .await
                .unwrap(),
        );
    }
    for i in 0..16 {
        assert_eq!(read(&mut peer).await["stream_id"], format!("lane-{i}"));
    }
    assert!(
        timeout(Duration::from_millis(80), peer.recv())
            .await
            .is_err(),
        "17th response must queue"
    );
    terminal(&mut peer, Some("lane-3"), "resp_3", 1).await;
    assert_eq!(read(&mut peer).await["stream_id"], "lane-16");
    let second = socket.create(request(Some("lane-0"), "two")).await.unwrap();
    let third = socket
        .create(request(Some("lane-0"), "three"))
        .await
        .unwrap();
    assert!(
        timeout(Duration::from_millis(80), peer.recv())
            .await
            .is_err()
    );
    terminal(&mut peer, Some("lane-0"), "resp_zero_1", 1).await;
    let next = read(&mut peer).await;
    assert_eq!(next["input"], "two");
    assert!(next.get("previous_response_id").is_none());
    assert!(
        timeout(Duration::from_millis(80), peer.recv())
            .await
            .is_err()
    );
    terminal(&mut peer, Some("lane-0"), "resp_zero_2", 1).await;
    assert_eq!(read(&mut peer).await["input"], "three");
    terminal(&mut peer, Some("lane-0"), "resp_zero_3", 1).await;
    finished(&collect(second).await);
    finished(&collect(third).await);
    socket.close();
    for (i, turn) in turns.into_iter().enumerate() {
        let events = collect(turn).await;
        if i == 0 || i == 3 {
            finished(&events);
        } else {
            failed(&events, "responses_ws_closed");
        }
    }
}

#[tokio::test]
async fn stream_validation_and_distinct_name_limit_allow_default_and_reuse() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    for invalid in [
        json!(""),
        json!("a/b"),
        json!("中文"),
        json!(null),
        json!(12),
        json!("a".repeat(257)),
    ] {
        let body = json!({"type":"response.create","stream_id":invalid});
        assert!(
            matches!(socket.create(Bytes::from(body.to_string())).await, Err(UpstreamError::Build(code)) if code == "invalid_stream_id")
        );
    }
    for i in 0..32 {
        let lane = if i == 0 {
            "a".repeat(256)
        } else {
            format!("Agent_{i}.name-x")
        };
        let turn = socket.create(request(Some(&lane), "one")).await.unwrap();
        assert_eq!(read(&mut peer).await["stream_id"], lane);
        terminal(&mut peer, Some(&lane), &format!("resp_{i}"), 1).await;
        finished(&collect(turn).await);
    }
    assert!(
        matches!(socket.create(request(Some("thirty-three"), "one")).await, Err(UpstreamError::Build(code)) if code == "websocket_stream_limit_reached")
    );
    for (lane, id) in [
        (None, "resp_default"),
        (Some("Agent_1.name-x"), "resp_reused"),
    ] {
        let turn = socket.create(request(lane, "one")).await.unwrap();
        read(&mut peer).await;
        terminal(&mut peer, lane, id, 1).await;
        finished(&collect(turn).await);
    }
}

#[tokio::test]
async fn malformed_or_misrouted_events_fail_closed_without_replay() {
    for bad in [
        json!({"type":"response.output_text.delta","stream_id":"other","delta":"secret"}),
        json!({"type":"response.completed","response":{"id":"missing-lane"}}),
        json!({"type":"response.completed","stream_id":"a","response":{"id":"changed"}}),
        json!({"stream_id":"a"}),
    ] {
        let (addr, mut incoming) = server().await;
        let socket = connect(addr).await;
        let mut peer = accept(&mut incoming).await;
        let a = socket.create(request(Some("a"), "a")).await.unwrap();
        let b = socket.create(request(Some("b"), "b")).await.unwrap();
        read(&mut peer).await;
        read(&mut peer).await;
        emit(
            &mut peer,
            json!({"type":"response.created","response":{"id":"resp_a"}}),
            Some("a"),
        )
        .await;
        emit(&mut peer, bad.clone(), None).await;
        let first = collect(a).await;
        let second = collect(b).await;
        assert_eq!(raws(&first).len(), 1);
        assert!(raws(&second).is_empty());
        for events in [&first, &second] {
            let error = events.iter().find_map(|e| e.as_ref().err()).unwrap();
            assert!(!error.retriable_before_first_token());
            assert!(!events.iter().any(|e| matches!(e, Ok(ChatEvent::Done))));
        }
        assert!(socket.is_closed());
        assert!(incoming.try_recv().is_err());
    }
}

#[tokio::test]
async fn dropped_consumer_drains_until_terminal_then_runs_queued_turn() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    let old = socket.create(request(Some("a"), "old")).await.unwrap();
    read(&mut peer).await;
    drop(old);
    let next = socket.create(request(Some("a"), "next")).await.unwrap();
    let other = socket.create(request(Some("b"), "other")).await.unwrap();
    assert_eq!(read(&mut peer).await["input"], "other");
    emit(
        &mut peer,
        json!({"type":"response.output_text.delta","delta":"discarded"}),
        Some("a"),
    )
    .await;
    terminal(&mut peer, Some("a"), "resp_old", 4).await;
    assert_eq!(read(&mut peer).await["input"], "next");
    terminal(&mut peer, Some("b"), "resp_other", 2).await;
    terminal(&mut peer, Some("a"), "resp_next", 2).await;
    finished(&collect(next).await);
    finished(&collect(other).await);
}

#[tokio::test]
async fn close_and_queue_overflow_are_bounded_and_never_synthesize_success() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    let mut turns = Vec::new();
    for i in 0..64 {
        turns.push(
            socket
                .create(request(Some("a"), &i.to_string()))
                .await
                .unwrap(),
        );
    }
    read(&mut peer).await;
    assert!(
        matches!(socket.create(request(Some("a"), "overflow")).await, Err(UpstreamError::Build(code)) if code == "responses_ws_queue_full")
    );
    socket.close();
    for turn in turns {
        failed(&collect(turn).await, "responses_ws_closed");
    }
    assert!(socket.is_closed());
    assert!(matches!(
        timeout(WAIT, peer.recv()).await.unwrap(),
        Some(Ok(Message::Close(_))) | None
    ));
}

#[tokio::test]
async fn stalled_consumer_triggers_bounded_backpressure_failure() {
    let (addr, mut incoming) = server().await;
    let socket = ResponsesSocket::connect(
        &HttpPool::new().unwrap(),
        &format!("http://{addr}/v1/responses"),
        &[],
        &Outbound::default(),
        SocketTimeouts {
            io: Duration::from_millis(100),
            ..SocketTimeouts::default()
        },
    )
    .await
    .unwrap();
    let mut peer = accept(&mut incoming).await;
    let turn = socket.create(request(Some("slow"), "hello")).await.unwrap();
    read(&mut peer).await;
    for _ in 0..40 {
        emit(
            &mut peer,
            json!({"type":"response.output_text.delta","delta":"bounded"}),
            Some("slow"),
        )
        .await;
    }
    timeout(WAIT, async {
        while !socket.is_closed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let events = collect(turn).await;
    assert_eq!(raws(&events).len(), 32);
    failed(&events, "responses_ws_backpressure");
}

#[tokio::test]
async fn idle_and_lifetime_timeouts_terminate_active_turns_without_retry() {
    for lifetime in [false, true] {
        let (addr, mut incoming) = server().await;
        let limits = if lifetime {
            SocketTimeouts {
                lifetime: Duration::from_millis(100),
                ..SocketTimeouts::default()
            }
        } else {
            SocketTimeouts {
                idle: Duration::from_millis(100),
                ..SocketTimeouts::default()
            }
        };
        let socket = ResponsesSocket::connect(
            &HttpPool::new().unwrap(),
            &format!("http://{addr}/v1/responses"),
            &[],
            &Outbound::default(),
            limits,
        )
        .await
        .unwrap();
        let mut peer = accept(&mut incoming).await;
        let turn = socket.create(request(None, "hello")).await.unwrap();
        read(&mut peer).await;
        failed(
            &collect(turn).await,
            if lifetime {
                "websocket_connection_limit_reached"
            } else {
                "responses_ws_timeout"
            },
        );
        assert!(socket.is_closed());
    }
}

#[tokio::test]
async fn abrupt_close_and_terminal_usage_are_not_confused() {
    for kind in ["close", "response.failed", "response.incomplete"] {
        let (addr, mut incoming) = server().await;
        let socket = connect(addr).await;
        let mut peer = accept(&mut incoming).await;
        let turn = socket.create(request(None, "hello")).await.unwrap();
        read(&mut peer).await;
        emit(
            &mut peer,
            json!({"type":"response.output_text.delta","delta":"partial"}),
            None,
        )
        .await;
        if kind == "close" {
            drop(peer);
        } else {
            emit(&mut peer, json!({"type":kind,"response":{"id":"resp_partial","usage":{"input_tokens":10,"output_tokens":3}}}), None).await;
        }
        let events = collect(turn).await;
        assert_eq!(raws(&events)[0]["delta"], "partial");
        if kind == "close" {
            let error = events.iter().find_map(|e| e.as_ref().err()).unwrap();
            assert!(!error.retriable_before_first_token());
            assert!(!events.iter().any(|e| matches!(e, Ok(ChatEvent::Done))));
        } else {
            finished(&events);
            assert!(events.iter().any(|e| matches!(e, Ok(ChatEvent::Data { usage: Some(usage), .. }) if usage.completion_tokens == 3)));
        }
    }
}

#[tokio::test]
async fn connections_are_isolated_and_handshake_uses_proxy_and_trusted_headers() {
    let (addr, mut incoming) = server().await;
    let outbound = Outbound {
        proxy_url: Some(format!("http://{addr}")),
        extra_headers: vec![
            ("authorization".into(), "Bearer wrong".into()),
            ("Sec-WebSocket-Key".into(), "wrong".into()),
            (
                "Sec-WebSocket-Extensions".into(),
                "permessage-deflate".into(),
            ),
            ("x-custom".into(), "kept".into()),
            ("chatgpt-account-id".into(), "wrong".into()),
        ],
    };
    let first = ResponsesSocket::connect(
        &HttpPool::new().unwrap(),
        "ws://unresolvable.invalid/v1/responses",
        &[
            ("authorization", "Bearer account-a"),
            ("chatgpt-account-id", "account-a"),
        ],
        &outbound,
        SocketTimeouts::default(),
    )
    .await
    .unwrap();
    let (mut a, headers, uri) = timeout(WAIT, incoming.recv()).await.unwrap().unwrap();
    assert_eq!(uri.to_string(), "http://unresolvable.invalid/v1/responses");
    assert_eq!(headers["authorization"], "Bearer account-a");
    assert_eq!(headers["chatgpt-account-id"], "account-a");
    assert_eq!(headers["x-custom"], "kept");
    assert_eq!(headers["sec-websocket-version"], "13");
    assert_ne!(headers["sec-websocket-key"], "wrong");
    assert!(!headers.contains_key("sec-websocket-extensions"));
    let second = connect(addr).await;
    let mut b = accept(&mut incoming).await;
    let one = first.create(request(Some("same"), "one")).await.unwrap();
    let two = second.create(request(Some("same"), "two")).await.unwrap();
    assert_eq!(read(&mut a).await["input"], "one");
    assert_eq!(read(&mut b).await["input"], "two");
    terminal(&mut a, Some("same"), "resp_same", 2).await;
    terminal(&mut b, Some("same"), "resp_same", 8).await;
    assert_eq!(
        raws(&collect(one).await)[0]["response"]["usage"]["output_tokens"],
        2
    );
    assert_eq!(
        raws(&collect(two).await)[0]["response"]["usage"]["output_tokens"],
        8
    );
}

#[tokio::test]
async fn invalid_handshake_redirect_and_error_bodies_are_bounded() {
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let target = listen(Router::new().fallback(get(move || {
        count.fetch_add(1, Ordering::SeqCst);
        async { StatusCode::OK }
    })))
    .await;
    let app = Router::new()
        .route(
            "/redirect",
            get(move || async move {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", format!("http://{target}/secret"))],
                )
            }),
        )
        .route(
            "/invalid",
            get(|| async {
                (
                    StatusCode::SWITCHING_PROTOCOLS,
                    [
                        ("connection", "upgrade"),
                        ("upgrade", "websocket"),
                        ("sec-websocket-accept", "invalid"),
                    ],
                )
                    .into_response()
            }),
        )
        .route(
            "/limited",
            get(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "3")],
                    "x".repeat(100_000),
                )
            }),
        )
        .route(
            "/stall",
            get(|| async { std::future::pending::<StatusCode>().await }),
        );
    let addr = listen(app).await;
    let pool = HttpPool::new().unwrap();
    let out = Outbound::default();
    for path in ["redirect", "invalid", "limited", "stall"] {
        let result = ResponsesSocket::connect(
            &pool,
            &format!("http://{addr}/{path}"),
            &[("authorization", "Bearer must-not-redirect")],
            &out,
            SocketTimeouts {
                handshake: Duration::from_millis(200),
                ..SocketTimeouts::default()
            },
        )
        .await;
        let Err(error) = result else {
            panic!("invalid handshake accepted");
        };
        match path {
            "redirect" => assert!(matches!(error, UpstreamError::Status { status: 307, .. })),
            "limited" => assert!(
                matches!(error, UpstreamError::Status { status: 429, body, retry_after_secs: Some(3) } if body.len() == 65536)
            ),
            "stall" => assert!(matches!(error, UpstreamError::Timeout)),
            _ => assert!(matches!(error, UpstreamError::Connect(_))),
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_client_frames_are_rejected_without_upstream_side_effects() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    for body in [
        Bytes::from_static(b"invalid"),
        Bytes::from_static(b"[]"),
        Bytes::from_static(b"{\"type\":\"response.cancel\"}"),
        Bytes::from_static(b"{\"type\":\"response.create\",\"stream\":true}"),
        Bytes::from_static(b"{\"type\":\"response.create\",\"background\":false}"),
        Bytes::from(vec![b' '; 64 * 1024 * 1024 + 1]),
    ] {
        assert!(matches!(
            socket.create(body).await,
            Err(UpstreamError::Build(_))
        ));
    }
    let turn = socket.create(request(None, "valid")).await.unwrap();
    assert_eq!(
        read(&mut peer).await["input"],
        "valid",
        "invalid frames must never reach upstream"
    );
    terminal(&mut peer, None, "resp_valid", 1).await;
    finished(&collect(turn).await);
}

#[tokio::test]
async fn ping_pong_binary_invalid_json_and_global_error_have_explicit_semantics() {
    for (message, code) in [
        (Message::Binary(Bytes::from_static(b"binary")), "responses_ws_non_text_event"),
        (Message::Text("not-json".into()), "responses_ws_invalid_event"),
        (Message::Text(json!({"type":"error","status":400,"error":{"code":"websocket_connection_limit_reached"}}).to_string().into()), "websocket_connection_limit_reached"),
    ] {
        let (addr, mut incoming) = server().await;
        let socket = connect(addr).await;
        let mut peer = accept(&mut incoming).await;
        let turn = socket.create(request(Some("a"), "hello")).await.unwrap();
        read(&mut peer).await;
        peer.send(Message::Ping(Bytes::from_static(b"probe"))).await.unwrap();
        assert!(matches!(timeout(WAIT, peer.recv()).await.unwrap(), Some(Ok(Message::Pong(body))) if body.as_ref() == b"probe"));
        peer.send(message).await.unwrap();
        failed(&collect(turn).await, code);
    }
}

#[tokio::test]
async fn explicit_close_interrupts_backpressure_and_last_owner_drop_closes_socket() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    let turn = socket.create(request(None, "slow")).await.unwrap();
    read(&mut peer).await;
    for _ in 0..40 {
        emit(
            &mut peer,
            json!({"type":"response.output_text.delta","delta":"buffered"}),
            None,
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    socket.close();
    // The normal I/O deadline is 10s; explicit close must interrupt it promptly.
    let events = timeout(Duration::from_secs(1), collect(turn))
        .await
        .unwrap();
    failed(&events, "responses_ws_closed");

    let owner = connect(addr).await;
    let clone = owner.clone();
    let mut peer = accept(&mut incoming).await;
    let turn = owner.create(request(None, "in-flight")).await.unwrap();
    read(&mut peer).await;
    drop(owner);
    assert!(!clone.is_closed());
    drop(clone);
    failed(&collect(turn).await, "responses_ws_closed");
    assert!(matches!(
        timeout(WAIT, peer.recv()).await.unwrap(),
        Some(Ok(Message::Close(_))) | None
    ));
}

#[tokio::test]
async fn late_terminal_from_previous_turn_cannot_finish_the_next_turn() {
    let (addr, mut incoming) = server().await;
    let socket = connect(addr).await;
    let mut peer = accept(&mut incoming).await;
    let old = socket.create(request(Some("a"), "old")).await.unwrap();
    read(&mut peer).await;
    terminal(&mut peer, Some("a"), "resp_old", 1).await;
    finished(&collect(old).await);
    let next = socket.create(request(Some("a"), "next")).await.unwrap();
    read(&mut peer).await;
    terminal(&mut peer, Some("a"), "resp_old", 100).await;
    let events = collect(next).await;
    assert!(
        raws(&events).is_empty(),
        "old usage must not be delivered to the new turn"
    );
    failed(&events, "responses_ws_response_id_mismatch");
}
