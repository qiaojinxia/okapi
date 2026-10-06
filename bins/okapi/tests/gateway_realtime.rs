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

#[path = "support/realtime_usage.rs"]
mod usage_tests;

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
                        let done = json!({"type": "response.done", "response": {"id": v["mock_response_id"], "usage": v.get("mock_usage").cloned().unwrap_or_else(|| json!({
                            "input_tokens": 100,
                            "output_tokens": 50,
                            "input_token_details": {"cached_tokens": 20, "audio_tokens": 0},
                            "output_token_details": {"audio_tokens": 30}
                        }))}});
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
    serve(router).await
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

#[path = "support/published_pricing.rs"]
mod published_pricing;

// ---- 测试环境 ----

struct TestEnv {
    pg: PgPool,
    state: gateway::state::AppState,
    ledger: okapi_ledger::BalanceLedger,
    gateway: SocketAddr,
    console: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
}

async fn setup(balance: Money) -> TestEnv {
    okapi_store::test_support::assert_isolated();
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
        None,
    )
    .await
    .unwrap();

    published_pricing::publish(&pg, user_id).await;
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    if !balance.is_zero() {
        state.ledger.credit(user_id, balance).await.unwrap();
    }
    let ledger = state.ledger.clone();

    let addr = serve(gateway::router(state.clone())).await;
    let console = serve(okapi::console::router(state.clone())).await;

    TestEnv {
        pg,
        state,
        ledger,
        gateway: addr,
        console,
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

/// 原始 TCP 正向代理：收绝对形式的请求行（明文 ws:// 经 HTTP 代理不走 CONNECT），
/// 改写成源站形式转给目标，之后双向透传（含 101 之后的 WS 帧）。记下每条请求行。
async fn spawn_raw_proxy(seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let seen = std::sync::Arc::clone(&seen);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = client.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    head.extend_from_slice(&buf[..n]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let line = text.lines().next().unwrap_or_default().to_owned();
                seen.lock().unwrap().push(line.clone());
                let mut parts = line.split(' ');
                let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let Ok(url) = reqwest::Url::parse(target) else {
                    return;
                };
                let origin_form = format!(
                    "{}{}",
                    url.path(),
                    url.query().map(|q| format!("?{q}")).unwrap_or_default()
                );
                let rewritten =
                    text.replacen(&line, &format!("{method} {origin_form} HTTP/1.1"), 1);
                let Ok(mut upstream) = tokio::net::TcpStream::connect((
                    url.host_str().unwrap_or("127.0.0.1"),
                    url.port().unwrap_or(80),
                ))
                .await
                else {
                    return;
                };
                if upstream.write_all(rewritten.as_bytes()).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
}

/// 渠道绑了出口代理（§11.41）时，Realtime 的上游 WebSocket 也经它握手、经它收发，
/// 不再是 HTTP 走代理、WS 却直连的旁路。
#[tokio::test]
async fn realtime_upstream_websocket_goes_through_the_bound_proxy() {
    let env = setup(Money::from_micros(50_000_000)).await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let proxy = spawn_raw_proxy(std::sync::Arc::clone(&seen)).await;
    let channel: i64 = sqlx::query_scalar("SELECT id FROM channels WHERE models @> $1")
        .bind(json!([env.model]))
        .fetch_one(&env.pg)
        .await
        .unwrap();
    let url = format!("http://{proxy}");
    let endpoint = okapi_providers::http::ProxyEndpoint::parse(&url).unwrap();
    let proxy_id = okapi_store::egress::create_proxy(
        &env.pg,
        &okapi_store::egress::NewProxy {
            name: "realtime-proxy",
            url: &url,
            endpoint: okapi_store::egress::Endpoint {
                scheme: &endpoint.scheme,
                host: &endpoint.host,
                port: i32::from(endpoint.port),
                username: None,
            },
            max_keys: None,
            max_concurrency: None,
            note: None,
            status: 1,
            owner_id: None,
        },
        None,
    )
    .await
    .unwrap();
    okapi_store::egress::set_channel_binding(
        &env.pg,
        channel,
        &okapi_store::egress::Binding::Proxy { proxy_id },
    )
    .await
    .unwrap()
    .unwrap();

    let mut ws = connect(&env).await.expect("握手应成功");
    let created = recv_text(&mut ws).await;
    assert_eq!(created["type"], "session.created", "经代理连到了上游");
    ws.send(CliMsg::text(json!({"type": "response.create"}).to_string()))
        .await
        .unwrap();
    assert_eq!(
        recv_text(&mut ws).await["type"],
        "response.output_text.delta"
    );
    let lines = seen.lock().unwrap().clone();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("GET http://") && lines[0].contains("/v1/realtime?model="),
        "{lines:?}"
    );
    ws.close(None).await.unwrap();
}

/// 双向泵 + 计费闭环：事件转发、二进制回显、usage 累计、断开 commit、余额一致。
#[tokio::test]
async fn realtime_bridge_bills_on_disconnect() {
    let initial = Money::from_micros(50_000_000);
    let env = setup(initial).await;

    let admitted_epoch = env.state.pricebook.load().epoch();
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

    // Publishing another mode/rate during an open session must not reprice it.
    let replacement = okapi_pricing::book::compile(okapi_pricing::book::PriceBookSource {
        epoch: admitted_epoch + 1000,
        models: vec![okapi_pricing::book::ModelEntry {
            model: okapi_domain::ModelCode::from(env.model.as_str()),
            pricing: okapi_pricing::PricingMode::PerCall {
                price: Money::from_micros(999_000_000),
            },
            tier_ratios: vec![],
        }],
        groups: vec![okapi_pricing::book::GroupEntry {
            group: okapi_domain::GroupCode::from("default"),
            ratio: okapi_pricing::RatioFp::ONE,
        }],
        overrides: vec![],
        rules: vec![],
    })
    .unwrap();
    env.state.pricebook.replace(replacement);
    ws.close(None).await.unwrap();
    drop(ws);

    let (status, amount, pt, ct) = wait_record(&env.pg, env.user_id, &env.model)
        .await
        .expect("断开后应产生计费记录");
    assert_eq!(status, 20, "应为 committed");
    assert_eq!((pt, ct), (100, 50), "usage 应按 response.done 累计");
    assert!(amount > 0, "有产出必须计费");
    let recorded_epoch: i64=sqlx::query_scalar("SELECT pricing_epoch FROM billing_records WHERE user_id=$1 AND log_type=2 ORDER BY created_at DESC LIMIT 1").bind(env.user_id).fetch_one(&env.pg).await.unwrap();
    assert_eq!(recorded_epoch, admitted_epoch);
    assert_ne!(amount, 999_000_000);

    // 余额一致性：初始 − 记录金额 = 最终（预扣差额已退）
    let final_balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(
        final_balance.as_micros(),
        initial.as_micros() - amount,
        "余额变动必须等于记账金额"
    );
}

#[tokio::test]
async fn realtime_cost_keeps_selected_config_until_disconnect() {
    let env = setup(Money::from_micros(50_000_000)).await;
    let channel: i64 = sqlx::query_scalar(
        "UPDATE channels SET upstream_unit_cost=$2 WHERE models @> $1 RETURNING id",
    )
    .bind(json!([env.model]))
    .bind(json!({"relative_cost_milli":1250}))
    .fetch_one(&env.pg)
    .await
    .unwrap();
    let mut ws = connect(&env).await.unwrap();
    assert_eq!(recv_text(&mut ws).await["type"], "session.created");
    sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE id=$1")
        .bind(channel)
        .bind(json!({"relative_cost_milli":2500}))
        .execute(&env.pg)
        .await
        .unwrap();
    env.state.channel_cost_cache.invalidate_all();
    ws.send(CliMsg::text(json!({"type":"response.create"}).to_string()))
        .await
        .unwrap();
    assert_eq!(
        recv_text(&mut ws).await["type"],
        "response.output_text.delta"
    );
    assert_eq!(recv_text(&mut ws).await["type"], "response.done");
    ws.close(None).await.unwrap();
    drop(ws);
    let (_, amount, _, _) = wait_record(&env.pg, env.user_id, &env.model).await.unwrap();
    assert_eq!(amount, 1160);
    let row: (i64,i64,i64,Option<i64>,Value,uuid::Uuid) = sqlx::query_as("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,request_id FROM billing_records WHERE user_id=$1 AND log_type=2")
        .bind(env.user_id).fetch_one(&env.pg).await.unwrap();
    assert_eq!((row.0, row.1, row.2, row.3), (1160, 1160, 0, Some(1450)));
    assert_eq!(
        row.4["upstream_cost_basis"],
        json!({"version":1,"source":"selected_channel","channel_id":channel,"relative_cost_milli":1250,"list_price_micro":1160})
    );
    let event: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(row.5.to_string()).fetch_one(&env.pg).await.unwrap();
    assert_eq!(event["upstream_cost_micro"], 1450);
    assert_eq!(event["upstream_cost_known"], true);
    assert_eq!(
        serde_json::from_str::<Value>(event["ratio_snapshot"].as_str().unwrap()).unwrap(),
        row.4
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        50_000_000 - 1160
    );
}

/// 零产出会话：全额退款 + 失败留痕，余额不变。
/// 连接预扣只够一份 max_output；会话内累计用量超过已覆盖额度就追加预扣，
/// 余额不够时下发 insufficient_quota 并断开。此前会话能一直跑，结算「多退少补」把余额扣成大负数。
#[tokio::test]
async fn realtime_session_stops_when_usage_outgrows_the_balance() {
    let env = setup(Money::from_micros(50_000_000)).await;
    let initial = env.ledger.balance(env.user_id).await.unwrap();
    let mut ws = connect(&env).await.expect("握手应成功");
    assert_eq!(recv_text(&mut ws).await["type"], "session.created");

    let mut completed = 0_u32;
    let mut stopped = None;
    'session: for i in 0..50 {
        let create = json!({"type": "response.create", "mock_response_id": format!("r{i}"),
            "mock_usage": {"input_tokens": 3_000_000, "output_tokens": 0}});
        if ws.send(CliMsg::text(create.to_string())).await.is_err() {
            break;
        }
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("等待 WS 消息超时");
            let Some(Ok(CliMsg::Text(t))) = msg else {
                break 'session;
            };
            let v: Value = serde_json::from_str(&t).unwrap();
            match v["type"].as_str() {
                Some("response.done") => {
                    completed += 1;
                    continue 'session;
                }
                Some("error") => {
                    stopped = v["error"]["code"].as_str().map(str::to_owned);
                    break 'session;
                }
                _ => {}
            }
        }
    }
    drop(ws);
    assert_eq!(
        stopped.as_deref(),
        Some("insufficient_quota"),
        "余额耗尽时断开（已完成 {completed} 个 response）"
    );

    let (status, amount, pt, _) = wait_record(&env.pg, env.user_id, &env.model)
        .await
        .expect("断开后按累计用量结算");
    assert_eq!(status, 20);
    assert_eq!(i64::from(pt), 3_000_000 * i64::from(completed));
    let per_response = amount / i64::from(completed);
    assert!(
        amount - initial.as_micros() < per_response,
        "透支不超过最后一个 response：扣 {amount}，余额 {}，每个 {per_response}",
        initial.as_micros()
    );
    for _ in 0..50 {
        if env
            .ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        env.ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty(),
        "追加预扣全部退回"
    );
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), initial.as_micros() - amount);
}

async fn failed_record(pg: &PgPool, user_id: i64) -> Option<(i16, i64, Option<String>)> {
    for _ in 0..50 {
        let row = sqlx::query_as::<_, (i16, i64, Option<String>)>(
            "SELECT status, amount_micro, error_code FROM billing_records
             WHERE user_id = $1 AND log_type = 5",
        )
        .bind(user_id)
        .fetch_optional(pg)
        .await
        .unwrap();
        if row.is_some() {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// 握手请求发出就断开：预扣发生在升级之前，升级失败（或建连 handler 被取消）时
/// 当场退款、留 `client_closed_request` 失败痕并释放连接租约。此前要等约 10 分钟的过期清理。
#[tokio::test]
async fn abandoned_handshake_refunds_and_releases_the_connection_slot() {
    use tokio::io::AsyncWriteExt as _;
    let env = setup(Money::from_micros(50_000_000)).await;
    let initial = env.ledger.balance(env.user_id).await.unwrap();
    // 选路查询卡在表锁上：预扣已建立、handler 还没返回
    let mut lock = env.pg.begin().await.unwrap();
    sqlx::query("LOCK TABLE channels IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let mut tcp = tokio::net::TcpStream::connect(env.gateway).await.unwrap();
    let request = format!(
        "GET /v1/realtime?model={} HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade\r\n\
         Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: realtime\r\n\
         Authorization: Bearer {}\r\n\r\n",
        env.model, env.gateway, env.token
    );
    tcp.write_all(request.as_bytes()).await.unwrap();
    for _ in 0..50 {
        if !env
            .ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        env.ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .len(),
        1,
        "admitted before the client leaves"
    );
    drop(tcp);
    tokio::time::sleep(Duration::from_millis(300)).await;
    lock.rollback().await.unwrap();

    assert_eq!(
        failed_record(&env.pg, env.user_id).await,
        Some((40, 0, Some("client_closed_request".to_owned())))
    );
    for _ in 0..50 {
        if env
            .ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        env.ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(env.ledger.balance(env.user_id).await.unwrap(), initial);
    let leases: i64 = {
        use fred::interfaces::SortedSetsInterface as _;
        let key_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id = $1")
            .bind(env.user_id)
            .fetch_one(&env.pg)
            .await
            .unwrap();
        let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
            .await
            .unwrap();
        redis.zcard(format!("ws:lease:k:{key_id}")).await.unwrap()
    };
    assert_eq!(leases, 0, "the connection slot is released");
}

/// 网关下线：升级后的会话不在 HTTP 排水里，收到下线信号即收尾、按已产出结算，
/// 下线等待覆盖它的落账。此前进程退出直接掐断会话，账只能等过期退款（免费）。
#[tokio::test]
async fn shutdown_settles_open_realtime_sessions() {
    let env = setup(Money::from_micros(50_000_000)).await;
    let mut ws = connect(&env).await.expect("握手应成功");
    assert_eq!(recv_text(&mut ws).await["type"], "session.created");
    ws.send(CliMsg::text(json!({"type": "response.create"}).to_string()))
        .await
        .unwrap();
    loop {
        if recv_text(&mut ws).await["type"] == "response.done" {
            break;
        }
    }
    assert!(
        env.state.settlements.in_flight() >= 1,
        "the session is tracked"
    );

    env.state.settlements.drain();
    tokio::time::timeout(
        Duration::from_secs(5),
        env.state.settlements.wait_idle(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(CliMsg::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the client sees the session end");
    let (status, amount, pt, ct) = wait_record(&env.pg, env.user_id, &env.model)
        .await
        .expect("会话按已产出结算");
    assert_eq!((status, pt, ct), (20, 100, 50));
    assert!(amount > 0);
}

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

#[tokio::test]
async fn realtime_refund_failure_preserves_subscription_and_recovers_once()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use fred::interfaces::{HashesInterface, KeysInterface};
    use std::collections::BTreeMap;
    let initial = Money::from_micros(50_000_000);
    let env = setup(initial).await;
    env.ledger
        .sub_set(env.user_id, initial, chrono::Utc::now().timestamp() + 3600)
        .await?;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?;
    let mut ws = connect(&env).await?;
    assert_eq!(recv_text(&mut ws).await["type"], "session.created");
    let held = env.ledger.list_reservations(env.user_id).await?;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].pool, okapi_ledger::Pool::Subscription);
    let id = held[0].request_id;
    let balance_key = format!("bal:{{{}}}", env.user_id);
    let key = format!("conc:{{{}}}:k:{}", env.user_id, held[0].api_key_id);
    let before: BTreeMap<String, String> = redis.hgetall(&balance_key).await?;
    redis.del::<(), _>(&key).await?;
    redis.hset::<(), _, _>(&key, ("invalid", "counter")).await?;
    ws.close(None).await?;
    drop(ws);
    let (status, amount, _, _) = wait_record(&env.pg, env.user_id, &env.model)
        .await
        .ok_or("missing failure record")?;
    assert_eq!((status, amount), (40, 0));
    let pool: i16 = sqlx::query_scalar("SELECT pool FROM billing_records WHERE request_id=$1")
        .bind(id)
        .fetch_one(&env.pg)
        .await?;
    assert_eq!(pool, 1);
    let after: BTreeMap<String, String> = redis.hgetall(&balance_key).await?;
    assert_eq!(after, before);
    redis.del::<(), _>(&key).await?;
    redis.set::<(), _, _>(&key, "1", None, None, false).await?;
    let future = chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::minutes(11))
        .ok_or("test time overflow")?;
    let recovered = okapi::worker::sweep_expired_reservations(&env.pg, &env.ledger, future).await?;
    assert!(recovered.iter().any(|r| r.request_id == id
        && r.action == "refund"
        && r.released_micro == held[0].amount.as_micros()));
    assert!(
        !okapi::worker::sweep_expired_reservations(&env.pg, &env.ledger, future)
            .await?
            .iter()
            .any(|r| r.request_id == id)
    );
    let sub: i64 = redis.hget(&balance_key, "sub").await?;
    assert_eq!(sub, initial.as_micros());
    assert_eq!(env.ledger.balance(env.user_id).await?, initial);
    let concurrency: i64 = redis.get(&key).await?;
    assert_eq!(concurrency, 0);
    Ok(())
}

#[tokio::test]
async fn repeated_realtime_refund_preserves_the_original_subscription_pool()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use fred::interfaces::{HashesInterface, KeysInterface};
    use std::collections::BTreeMap;
    let initial = Money::from_micros(50_000_000);
    let env = setup(initial).await;
    env.ledger
        .sub_set(env.user_id, initial, chrono::Utc::now().timestamp() + 3600)
        .await?;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?;
    let balance_key = format!("bal:{{{}}}", env.user_id);
    let before: BTreeMap<String, String> = redis.hgetall(&balance_key).await?;
    let mut ws = connect(&env).await?;
    assert_eq!(recv_text(&mut ws).await["type"], "session.created");
    let held = env.ledger.list_reservations(env.user_id).await?;
    assert_eq!(held.len(), 1);
    let id = held[0].request_id;
    let refunded = env
        .ledger
        .refund(env.user_id, held[0].api_key_id, id)
        .await?;
    assert_eq!(refunded.pool, okapi_ledger::Pool::Subscription);
    ws.close(None).await?;
    drop(ws);
    let (status, amount, _, _) = wait_record(&env.pg, env.user_id, &env.model)
        .await
        .ok_or("missing failure record")?;
    assert_eq!((status, amount), (40, 0));
    let pool: i16 = sqlx::query_scalar("SELECT pool FROM billing_records WHERE request_id=$1")
        .bind(id)
        .fetch_one(&env.pg)
        .await?;
    assert_eq!(pool, 1);
    let after: BTreeMap<String, String> = redis.hgetall(&balance_key).await?;
    assert_eq!(after, before, "repeated refund must not credit again");
    let concurrency: i64 = redis
        .get(format!("conc:{{{}}}:k:{}", env.user_id, held[0].api_key_id))
        .await?;
    assert_eq!(concurrency, 0);
    Ok(())
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

/// 缺 `?model=`：握手前就被 `Query` 提取器拒绝，且回的是 error_code JSON 壳而不是
/// axum 缺省的英文纯文本（i18n 红线，与 console 的 400 同一契约）。
#[tokio::test]
async fn realtime_without_model_is_rejected_with_error_code() {
    let env = setup(Money::from_micros(50_000_000)).await;

    let resp = reqwest::Client::new()
        .get(format!("http://{}/v1/realtime", env.gateway))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.expect("400 应是 JSON 壳，不是纯文本");
    assert_eq!(body["error"]["code"], "bad_request");
    assert_eq!(body["error"]["param"], "query");
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
