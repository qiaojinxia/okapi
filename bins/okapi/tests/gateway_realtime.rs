//! Realtime WS 桥接验收（IMPLEMENTATION §4.4 M4 + §14.4 治理）：
//! 双向泵与计费闭环 / 零产出退款 / per-key 限连 / 子协议鉴权。
//! 依赖 .env 中的 DATABASE_URL 与 OKAPI_REDIS_URL（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::ws::{Message as SrvMsg, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as CliMsg;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use uuid::Uuid;

// ---- mock 上游 WS ----

/// 行为：连上先发 session.created；response.create → delta + response.done(usage)；
/// 二进制帧原样回显（音频通路验证）。凭证透传校验失败直接 401。
async fn mock_realtime(headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let authed = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "Bearer mock-credential");
    if !authed {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(|mut sock: WebSocket| async move {
        let created = json!({"type": "session.created", "session": {"id": "sess_mock"}});
        if sock
            .send(SrvMsg::Text(created.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
        while let Some(Ok(msg)) = sock.recv().await {
            match msg {
                SrvMsg::Text(t) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or_default();
                    if v["type"] == "response.create" {
                        let delta = json!({"type": "response.output_text.delta", "delta": "hi"});
                        let done = json!({"type": "response.done", "response": {"usage": {
                            "input_tokens": 100,
                            "output_tokens": 50,
                            "input_token_details": {"cached_tokens": 20, "audio_tokens": 0},
                            "output_token_details": {"audio_tokens": 30}
                        }}});
                        let _ = sock.send(SrvMsg::Text(delta.to_string().into())).await;
                        let _ = sock.send(SrvMsg::Text(done.to_string().into())).await;
                    }
                }
                SrvMsg::Binary(b) => {
                    let _ = sock.send(SrvMsg::Binary(b)).await;
                }
                SrvMsg::Close(_) => break,
                _ => {}
            }
        }
    })
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new().route("/v1/realtime", get(mock_realtime));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

// ---- 测试环境 ----

struct TestEnv {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(balance: Money) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("rt-{}", &suffix[..12]);

    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    // 限连测试依赖缺省上限；清掉可能的遗留全局配置
    sqlx::query!("DELETE FROM settings WHERE key = 'realtime_max_conns_per_key'")
        .execute(&pg)
        .await
        .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-rt-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-rt")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&pg, &model, "2.0", "4.0", "0.5")
        .await
        .unwrap();

    let mock = spawn_mock().await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("rt-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        false,
    )
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    if !balance.is_zero() {
        state.ledger.credit(user_id, balance).await.unwrap();
    }
    let ledger = state.ledger.clone();

    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    TestEnv {
        pg,
        ledger,
        gateway: addr,
        token,
        user_id,
        model,
    }
}

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(env: &TestEnv) -> Result<WsClient, tokio_tungstenite::tungstenite::Error> {
    let url = format!("ws://{}/v1/realtime?model={}", env.gateway, env.model);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", env.token).parse().unwrap(),
    );
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

async fn recv_text(ws: &mut WsClient) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("等待 WS 消息超时")
            .expect("WS 流提前结束")
            .expect("WS 读取错误");
        if let CliMsg::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// (status, amount_micro, prompt_tokens, completion_tokens)；status 20=committed 40=failed。
async fn wait_record(pg: &PgPool, user_id: i64, model: &str) -> Option<(i16, i64, i32, i32)> {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens, completion_tokens
               FROM billing_records WHERE user_id = $1 AND model_name = $2"#,
            user_id,
            model
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row {
            return Some((
                r.status,
                r.amount_micro,
                r.prompt_tokens,
                r.completion_tokens,
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

// ---- 用例 ----

/// 双向泵 + 计费闭环：事件转发、二进制回显、usage 累计、断开 commit、余额一致。
#[tokio::test]
async fn realtime_bridge_bills_on_disconnect() {
    let initial = Money::from_micros(50_000_000);
    let env = setup(initial).await;

    let mut ws = connect(&env).await.expect("握手应成功");
    // 上游 session.created 应转发到客户端
    let created = recv_text(&mut ws).await;
    assert_eq!(created["type"], "session.created");

    // 二进制音频帧回显（客户端→上游→回显→客户端）
    ws.send(CliMsg::binary(vec![1u8, 2, 3])).await.unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("等待回显超时")
            .unwrap()
            .unwrap();
        if let CliMsg::Binary(b) = msg {
            assert_eq!(b.as_ref(), &[1u8, 2, 3]);
            break;
        }
    }

    // response.create → delta + response.done（usage 100/20/50）
    ws.send(CliMsg::text(json!({"type": "response.create"}).to_string()))
        .await
        .unwrap();
    let delta = recv_text(&mut ws).await;
    assert_eq!(delta["type"], "response.output_text.delta");
    let done = recv_text(&mut ws).await;
    assert_eq!(done["type"], "response.done");

    ws.close(None).await.unwrap();
    drop(ws);

    let (status, amount, pt, ct) = wait_record(&env.pg, env.user_id, &env.model)
        .await
        .expect("断开后应产生计费记录");
    assert_eq!(status, 20, "应为 committed");
    assert_eq!((pt, ct), (100, 50), "usage 应按 response.done 累计");
    assert!(amount > 0, "有产出必须计费");

    // 余额一致性：初始 − 记录金额 = 最终（预扣差额已退）
    let final_balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(
        final_balance.as_micros(),
        initial.as_micros() - amount,
        "余额变动必须等于记账金额"
    );
}

/// 零产出会话：全额退款 + 失败留痕，余额不变。
#[tokio::test]
async fn realtime_zero_output_refunds_all() {
    let initial = Money::from_micros(50_000_000);
    let env = setup(initial).await;

    let mut ws = connect(&env).await.expect("握手应成功");
    let created = recv_text(&mut ws).await;
    assert_eq!(created["type"], "session.created");
    ws.close(None).await.unwrap();
    drop(ws);

    let (status, amount, _, _) = wait_record(&env.pg, env.user_id, &env.model)
        .await
        .expect("零产出也应留痕");
    assert_eq!(status, 40, "应为 failed");
    assert_eq!(amount, 0, "零产出不得计费");

    let final_balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(
        final_balance.as_micros(),
        initial.as_micros(),
        "零产出余额必须原样退回"
    );
}

/// per-key WS 并发上限（缺省 4）：第 5 条握手拒绝 429。
#[tokio::test]
async fn realtime_conn_limit_rejects_fifth() {
    let env = setup(Money::from_micros(200_000_000)).await;

    let mut held = Vec::new();
    for i in 0..4 {
        held.push(connect(&env).await.unwrap_or_else(|e| {
            panic!("第 {} 条连接应成功: {e}", i + 1);
        }));
    }
    let fifth = connect(&env).await;
    match fifth {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 429, "超限应返回 429");
        }
        other => panic!("第 5 条连接应被 429 拒绝，实际: {other:?}"),
    }
    for mut ws in held {
        let _ = ws.close(None).await;
    }
}

/// OpenAI 客户端子协议鉴权：无 Authorization 头，凭 openai-insecure-api-key.* 握手成功。
#[tokio::test]
async fn realtime_subprotocol_auth_works() {
    let env = setup(Money::from_micros(50_000_000)).await;

    let url = format!("ws://{}/v1/realtime?model={}", env.gateway, env.model);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "sec-websocket-protocol",
        format!("realtime, openai-insecure-api-key.{}", env.token)
            .parse()
            .unwrap(),
    );
    let (mut ws, resp) = tokio_tungstenite::connect_async(req)
        .await
        .expect("子协议鉴权应握手成功");
    assert_eq!(resp.status(), 101);
    let created = recv_text(&mut ws).await;
    assert_eq!(created["type"], "session.created");
    let _ = ws.close(None).await;
}
