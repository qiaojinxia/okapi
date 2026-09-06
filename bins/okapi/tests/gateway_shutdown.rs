//! 优雅下线验收（IMPLEMENTATION §14.3）：容器编排发的是 SIGTERM。收到后 gateway 必须
//! 停接新连接、把在途 SSE 排完、结算落账，然后以 0 退出——而不是被信号直接掐死，
//! 让客户端看到半截流、账本留一笔悬置预扣。
//! 用真实二进制起子进程（`CARGO_BIN_EXE_okapi`），依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::body::{Body, Bytes};
use axum::response::IntoResponse;
use axum::routing::post;
use futures::StreamExt;
use okapi_domain::Money;
use serde_json::json;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

const CHUNKS: usize = 8;
const CHUNK_GAP: Duration = Duration::from_millis(200);

fn chunk(text: &str) -> String {
    let v = json!({"id":"c","object":"chat.completion.chunk",
        "choices":[{"index":0,"delta":{"content":text}}]});
    format!("data: {v}\n\n")
}

/// 慢流：8 段正文各隔 200ms，再补 usage 与 [DONE]。总时长 ~1.6s，足够在中途发信号。
async fn mock_slow() -> axum::response::Response {
    let steps = futures::stream::unfold(0_usize, |step| async move {
        match step.cmp(&CHUNKS) {
            std::cmp::Ordering::Less => {
                if step > 0 {
                    tokio::time::sleep(CHUNK_GAP).await;
                }
                let text = format!("tok{step} ");
                Some((Ok::<_, std::io::Error>(Bytes::from(chunk(&text))), step + 1))
            }
            std::cmp::Ordering::Equal => {
                let usage = json!({"id":"c","object":"chat.completion.chunk","choices":[],
                    "usage":{"prompt_tokens":100,"completion_tokens":20}});
                Some((
                    Ok(Bytes::from(format!("data: {usage}\n\ndata: [DONE]\n\n"))),
                    step + 1,
                ))
            }
            std::cmp::Ordering::Greater => None,
        }
    });
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(steps),
    )
        .into_response()
}

async fn free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

struct Env {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    token: String,
    user_id: i64,
    model: String,
    database_url: String,
    redis_url: String,
}

async fn seed() -> Env {
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

    let router = Router::new().route("/v1/chat/completions", post(mock_slow));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    okapi_store::provision::create_channel(
        &pg,
        &format!("ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock-credential",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let ledger = okapi_ledger::BalanceLedger::new(redis);
    ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
    Env {
        pg,
        ledger,
        token,
        user_id,
        model,
        database_url,
        redis_url,
    }
}

async fn wait_healthy(addr: SocketAddr) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(r) = client.get(format!("http://{addr}/healthz")).send().await
            && r.status() == 200
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("gateway 子进程 30s 内未就绪");
}

/// 读到首个正文块后立刻给子进程发 SIGTERM（编排层 stop 的第一步），然后把流读完。
/// 排水期间任何一块读失败都算掐断。
async fn read_stream_signalling_after_first_chunk(resp: reqwest::Response, pid: u32) -> String {
    let mut stream = resp.bytes_stream();
    let mut text = String::new();
    let mut signalled = false;
    while let Some(piece) = stream.next().await {
        let bytes = piece.expect("排水期间在途流不得被掐断");
        text.push_str(&String::from_utf8_lossy(&bytes));
        if !signalled && text.contains("tok0") {
            let status = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .unwrap();
            assert!(status.success());
            signalled = true;
        }
    }
    assert!(signalled, "首块未到就结束了：{text}");
    text
}

/// 排水期间不再接新连接：5s 内观察到连接被拒 / 请求失败即为真。
async fn refuses_new_connections(client: &reqwest::Client, addr: SocketAddr) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let probe = client
            .get(format!("http://{addr}/healthz"))
            .timeout(Duration::from_millis(500))
            .send()
            .await;
        if probe.is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn sigterm_drains_in_flight_stream_settles_and_exits_cleanly() {
    let env = seed().await;
    let port = free_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_okapi"))
        .arg("gateway")
        .env("DATABASE_URL", &env.database_url)
        .env("OKAPI_REDIS_URL", &env.redis_url)
        .env("OKAPI_BIND", addr.to_string())
        .env("OKAPI_NODE", "shutdown-test")
        .env("OKAPI_SINGLE_USER_MODE", "false")
        .env("RUST_LOG", "warn")
        .env_remove("OKAPI_CLICKHOUSE_URL")
        .env_remove("OKAPI_NATS_URL")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("启动 okapi gateway 子进程");
    wait_healthy(addr).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "stream": true, "max_tokens": 64,
            "messages": [{"role":"user","content":"hi there"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let request_id: Uuid = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap();

    let text = read_stream_signalling_after_first_chunk(resp, child.id()).await;
    for i in 0..CHUNKS {
        assert!(text.contains(&format!("tok{i} ")), "缺第 {i} 段：{text}");
    }
    assert!(text.contains("[DONE]"), "在途流必须排到终止帧：{text}");

    assert!(
        refuses_new_connections(&client, addr).await,
        "SIGTERM 后 5s 内仍在接新连接"
    );

    // 进程以 0 退出（不是被信号杀死），且在合理时间内
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "SIGTERM 后 15s 仍未退出");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(status.success(), "应正常退出而非被信号终止：{status:?}");

    // 退出前已把这笔在途请求结算落账：committed、金额精确、无悬置预扣
    let row = sqlx::query!(
        "SELECT status, amount_micro FROM billing_records WHERE request_id = $1",
        request_id
    )
    .fetch_optional(&env.pg)
    .await
    .unwrap()
    .expect("退出前必须写入 billing_records");
    assert_eq!(row.status, 20, "在途请求应在退出前结算为 committed");
    // (100 + 20) × 2 micro
    assert_eq!(row.amount_micro, 240);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        10_000_000 - 240
    );
    assert!(
        env.ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty(),
        "预扣不得悬置到对账才清"
    );
}
