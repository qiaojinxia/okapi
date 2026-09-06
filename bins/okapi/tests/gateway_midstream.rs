//! 首字后断流语义（IMPLEMENTATION §3.6 / §3.7）：上游已经吐出内容后连接被掐，
//! 网关不得换渠道重放（客户端已收到半截回答，重放会得到拼接的两段）、不得同 key 重试，
//! 只按已产出结算并把余额精确收口。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use futures::StreamExt;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::json;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Default)]
struct Calls {
    cut: Arc<AtomicUsize>,
    ok: Arc<AtomicUsize>,
}

fn chunk(text: &str) -> String {
    let v = json!({"id":"c","object":"chat.completion.chunk",
        "choices":[{"index":0,"delta":{"content":text}}]});
    format!("data: {v}\n\n")
}

/// 吐两段正文后把连接掐断：body 流在第三步返回 Err，hyper 不会写终止 chunk，
/// 客户端（网关）读到传输错误而不是正常 EOF。两段之间留间隔，保证网关已把首字转发出去。
async fn mock_cut(State(calls): State<Calls>) -> axum::response::Response {
    calls.cut.fetch_add(1, Ordering::SeqCst);
    let steps = futures::stream::unfold(0_u8, |step| async move {
        match step {
            0 => Some((Ok(Bytes::from(chunk("Hello"))), 1)),
            1 => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Some((Ok(Bytes::from(chunk(" wor"))), 2))
            }
            2 => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "cut",
                    )),
                    3,
                ))
            }
            _ => None,
        }
    });
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(steps),
    )
        .into_response()
}

/// 备用渠道：若网关在首字后仍去 failover，这里会被打到，客户端会看到 " world" 与 [DONE]。
async fn mock_ok(State(calls): State<Calls>) -> axum::response::Response {
    calls.ok.fetch_add(1, Ordering::SeqCst);
    let usage = json!({"id":"c","object":"chat.completion.chunk","choices":[],
        "usage":{"prompt_tokens":100,"completion_tokens":20}});
    let body = format!(
        "{}{}data: {usage}\n\ndata: [DONE]\n\n",
        chunk("Hello"),
        chunk(" world")
    );
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

struct TestEnv {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
    calls: Calls,
}

async fn setup() -> TestEnv {
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

    let calls = Calls::default();
    let router = Router::new()
        .route("/cut/v1/chat/completions", post(mock_cut))
        .route("/ok/v1/chat/completions", post(mock_ok))
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // 掐流渠道优先级高先被选中；备用渠道压在后面等着接 failover
    for (i, (path, priority)) in [("/cut/v1", 10), ("/ok/v1", 0)].into_iter().enumerate() {
        let (channel_id, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("ch{i}-{suffix}"),
            "openai",
            &format!("http://{mock}{path}"),
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

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(10_000_000))
        .await
        .unwrap();
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
        calls,
    }
}

struct Record {
    status: i16,
    amount_micro: i64,
    prompt_tokens: i32,
    completion_tokens: i32,
    failover_count: i16,
    error_code: Option<String>,
}

async fn wait_committed(pg: &PgPool, request_id: Uuid) -> Record {
    for _ in 0..50 {
        let row = sqlx::query!(
            r#"SELECT status, amount_micro, prompt_tokens, completion_tokens, failover_count, error_code
               FROM billing_records WHERE request_id = $1"#,
            request_id
        )
        .fetch_optional(pg)
        .await
        .unwrap();
        if let Some(r) = row.filter(|r| r.status != 10) {
            return Record {
                status: r.status,
                amount_micro: r.amount_micro,
                prompt_tokens: r.prompt_tokens,
                completion_tokens: r.completion_tokens,
                failover_count: r.failover_count,
                error_code: r.error_code,
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("billing_records 未在 5s 内结算 request_id={request_id}");
}

#[tokio::test]
async fn upstream_cut_after_first_token_settles_partial_output_without_retry() {
    let env = setup().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({
            "model": env.model, "stream": true, "max_tokens": 64,
            "messages": [{"role":"user","content":"hi there"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "首字已出，状态码早已发出");
    let request_id: Uuid = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap();

    // 逐块读到网关关流为止：网关侧读到传输错误后直接结束 SSE，不发 [DONE]
    let mut stream = resp.bytes_stream();
    let mut text = String::new();
    while let Some(piece) = stream.next().await {
        match piece {
            Ok(bytes) => text.push_str(&String::from_utf8_lossy(&bytes)),
            Err(_) => break,
        }
    }
    assert!(text.contains("Hello"), "{text}");
    assert!(text.contains(" wor"), "{text}");
    assert!(!text.contains("[DONE]"), "半截流不能伪装成正常完成：{text}");
    assert!(
        !text.contains(" world"),
        "首字后不得 failover 到备用渠道拼接第二段：{text}"
    );

    let rec = wait_committed(&env.pg, request_id).await;
    assert_eq!(rec.status, 20, "按已产出结算为 committed");
    assert_eq!(rec.failover_count, 0);
    assert!(rec.error_code.is_none(), "{:?}", rec.error_code);
    assert!(rec.prompt_tokens > 0, "prompt 用本地估算");
    assert!(
        rec.completion_tokens > 0,
        "已产出的 9 个字符必须计入 completion（本地密度估算）"
    );
    // 单倍率 $2/1M：金额 = (prompt + completion) × 2 micro，且预扣多余部分全部退回
    let expected = i64::from(rec.prompt_tokens + rec.completion_tokens) * 2;
    assert_eq!(rec.amount_micro, expected);
    let balance = env.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance.as_micros(), 10_000_000 - rec.amount_micro);
    assert!(
        env.ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty(),
        "预扣不得悬置"
    );

    assert_eq!(env.calls.cut.load(Ordering::SeqCst), 1, "同 key 不得重试");
    assert_eq!(
        env.calls.ok.load(Ordering::SeqCst),
        0,
        "不得 failover 到备用渠道"
    );
}
