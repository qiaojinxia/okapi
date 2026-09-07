//! 结算积压上界（IMPLEMENTATION §12.2）：后台结算排队数超过 `settle_backlog_max` 时，
//! 数据面在鉴权前 503 `overloaded`——不预扣、不碰上游；积压回落即恢复，账一笔不丢。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::State;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use uuid::Uuid;

async fn mock_json(State(calls): State<Arc<AtomicUsize>>) -> axum::Json<Value> {
    calls.fetch_add(1, Ordering::SeqCst);
    axum::Json(json!({
        "id": "c", "object": "chat.completion",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20}
    }))
}

struct TestEnv {
    pg: PgPool,
    state: gateway::state::AppState,
    gateway: SocketAddr,
    token: String,
    user_id: i64,
    model: String,
    upstream_calls: Arc<AtomicUsize>,
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

    let upstream_calls = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route("/v1/chat/completions", post(mock_json))
        .with_state(upstream_calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
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
        false,
        None,
    )
    .await
    .unwrap();

    let mut state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    // 上界压到 1：第三笔起就该被拒（判定是"超过"而非"达到"）
    state.settle_backlog_max = 1;
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
        pg,
        state,
        gateway: addr,
        token,
        user_id,
        model,
        upstream_calls,
    }
}

impl TestEnv {
    async fn chat(&self) -> (u16, Value) {
        let resp = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", self.gateway))
            .bearer_auth(&self.token)
            .json(&json!({
                "model": self.model, "max_tokens": 8,
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap())
    }

    /// 结算任务在响应之后才进入 `settle_write`，等计数追上再发下一笔。
    async fn wait_backlog(&self, expected: usize) {
        for _ in 0..50 {
            if self.state.settle_backlog.load(Ordering::SeqCst) == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "settle_backlog 未在 2.5s 内到达 {expected}（当前 {}）",
            self.state.settle_backlog.load(Ordering::SeqCst)
        );
    }

    async fn billing_rows(&self) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "n!" FROM billing_records WHERE user_id = $1 AND status = 20"#,
            self.user_id
        )
        .fetch_one(&self.pg)
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn backlog_over_cap_sheds_before_reserve_and_recovers() {
    let env = setup().await;
    let gate = env.state.settle_gate.clone();
    // 占满结算写入闸：Redis commit 照常完成，PG 记账在闸前排队——正是压测里积压的形态
    let permits = gate
        .clone()
        .acquire_many_owned(u32::try_from(gate.available_permits()).unwrap())
        .await
        .unwrap();

    let (status, _) = env.chat().await;
    assert_eq!(status, 200);
    env.wait_backlog(1).await;
    let (status, _) = env.chat().await;
    assert_eq!(status, 200, "积压 1 未超过上界 1，仍放行");
    env.wait_backlog(2).await;
    let balance_before = env.state.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(env.billing_rows().await, 0, "闸被占满，PG 尚无一笔");

    let (status, body) = env.chat().await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "overloaded", "{body}");
    assert_eq!(
        body["error"]["param"], "2",
        "param = 当时的积压笔数：{body}"
    );
    assert_eq!(
        env.upstream_calls.load(Ordering::SeqCst),
        2,
        "被拒的请求不得打到上游"
    );
    assert_eq!(
        env.state.ledger.balance(env.user_id).await.unwrap(),
        balance_before,
        "被拒的请求分文未动"
    );
    assert!(
        env.state
            .ledger
            .list_reservations(env.user_id)
            .await
            .unwrap()
            .is_empty(),
        "被拒的请求不得留下预扣"
    );
    assert_eq!(
        env.state.settle_backlog.load(Ordering::SeqCst),
        2,
        "被拒的请求不产生新结算"
    );

    // 泄压：放开闸，积压落账归零，数据面恢复，前两笔一笔不丢
    drop(permits);
    env.wait_backlog(0).await;
    assert_eq!(env.billing_rows().await, 2);
    let (status, _) = env.chat().await;
    assert_eq!(status, 200, "积压回落后放行");
    env.wait_backlog(0).await;
    assert_eq!(env.billing_rows().await, 3);
    // 三笔成功各扣 (100 + 20) × 2 micro（单倍率 $2/1M），与 Redis 余额吻合
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        10_000_000 - 3 * 240
    );

    // `OKAPI_SETTLE_BACKLOG_MAX=0` = 不设限（压测网关自身开销的口径）：准入函数直接验证
    let mut unbounded = env.state.clone();
    unbounded.settle_backlog_max = 0;
    unbounded.settle_backlog.store(1_000_000, Ordering::SeqCst);
    assert!(unbounded.check_settle_backlog().is_ok());
    unbounded.settle_backlog.store(0, Ordering::SeqCst);
}
