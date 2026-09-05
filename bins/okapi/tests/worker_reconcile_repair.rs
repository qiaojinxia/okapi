//! 对账修复验收（IMPLEMENTATION §11.22）。
//!
//! Redis 是**唯一**热账本，`reserve.lua` 对缺键按余额 0 处理且不回源 PG——实例没开持久化
//! 重启、切到空副本、maxmemory 淘汰 `bal:{}`、或者谁手滑 FLUSHDB，余额就集体归零，
//! 全站付费请求静默拒服务且**不会自愈**：对账任务此前只报不修，全仓也没有第二个入口
//! 能把余额写回去，而运维页上却写着「后台对账任务会按账本自动校准」。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway, worker};
use okapi_domain::Money;
use okapi_ledger::{LimitCaps, ReserveRequest};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    user_id: i64,
    api_key_id: i64,
    admin_token: String,
    plain_token: String,
    console: SocketAddr,
}

/// 建用户 + 走正规入账（PG 事件 + Redis 同步），让账本与热余额起手一致。
async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let user_id = okapi_store::provision::create_user(&pg, &format!("rr-u-{suffix}"))
        .await
        .unwrap();
    let plain_token = format!("sk-okapi-rr-{suffix}");
    let api_key_id =
        okapi_store::provision::create_api_key(&pg, user_id, &hash(&plain_token), "sk-rr")
            .await
            .unwrap();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("rr-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-rr-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-rr-adm")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    // 正规入账：Redis credit + PG 事件 + 展示快照，三处一致
    let amount = Money::from_micros(9_000_000);
    state.ledger.credit(user_id, amount).await.unwrap();
    okapi_ledger::pg::record_credit(&pg, user_id, amount, "adjust", "test", json!({}))
        .await
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = listener.local_addr().unwrap();
    let ledger = state.ledger.clone();
    let app = console::router(state);
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    Bed {
        pg,
        ledger,
        user_id,
        api_key_id,
        admin_token,
        plain_token,
        console: console_addr,
    }
}

/// 只看这一个用户的漂移（对账扫全表，别的用例造的漂移不关我们的事）。
async fn drift_of(bed: &Bed) -> Option<worker::BalanceDrift> {
    worker::reconcile_balances(&bed.pg, &bed.ledger, 100_000)
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.user_id == bed.user_id)
}

/// 模拟「Redis 数据没了」：热余额清零而账本事件原封不动。
/// 用 drain 而非删键——两者对 `reserve.lua` 完全等价（缺键与 avail=0 都读作 0），
/// 但 drain 不需要在用例里拿裸 Redis 连接。
async fn lose_hot_balance(bed: &Bed) {
    bed.ledger.drain(bed.user_id).await.unwrap();
}

#[tokio::test]
async fn repair_rebuilds_hot_balance_from_the_ledger() {
    let bed = setup().await;
    assert!(drift_of(&bed).await.is_none(), "起手三处应一致");

    lose_hot_balance(&bed).await;
    assert_eq!(bed.ledger.balance(bed.user_id).await.unwrap().as_micros(), 0);
    let drift = drift_of(&bed).await.expect("热余额丢了应被对账扫出");
    assert_eq!(drift.events_sum_micro, 9_000_000);
    assert_eq!(drift.redis_effective_micro, 0, "这正是全站拒服务的那个 0");

    let fixed = worker::repair_balance(&bed.pg, &bed.ledger, bed.user_id)
        .await
        .unwrap()
        .expect("用户存在");
    assert_eq!(fixed.redis_before_micro, 0);
    assert_eq!(fixed.redis_after_micro, 9_000_000);
    assert_eq!(fixed.events_sum_micro, 9_000_000);
    assert_eq!(
        bed.ledger.balance(bed.user_id).await.unwrap().as_micros(),
        9_000_000
    );
    assert!(drift_of(&bed).await.is_none(), "修完不该再有差额");

    // 幂等：同一权威值重跑不改变结果
    let again = worker::repair_balance(&bed.pg, &bed.ledger, bed.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.redis_before_micro, 9_000_000);
    assert_eq!(again.redis_after_micro, 9_000_000);

    // 不存在的用户 → None（端点据此转 404）
    assert!(
        worker::repair_balance(&bed.pg, &bed.ledger, 99_999_999)
            .await
            .unwrap()
            .is_none()
    );
}

/// 在途预扣必须原样保留：抹掉它们会让那些请求结算时凭空多扣或少扣。
#[tokio::test]
async fn repair_preserves_in_flight_reservations() {
    let bed = setup().await;
    let est = Money::from_micros(2_000_000);
    let outcome = bed
        .ledger
        .reserve(
            ReserveRequest {
                user_id: bed.user_id,
                api_key_id: bed.api_key_id,
                request_id: Uuid::new_v4(),
                est,
                caps: LimitCaps::default(),
                est_tokens: 100,
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        okapi_ledger::ReserveOutcome::Reserved { .. }
    ));
    assert_eq!(
        bed.ledger.balance(bed.user_id).await.unwrap().as_micros(),
        7_000_000,
        "预扣后 avail 少 2"
    );
    // 有在途时三处仍应一致：avail(7) + 在途(2) == 账本(9)
    assert!(drift_of(&bed).await.is_none());

    lose_hot_balance(&bed).await;
    let fixed = worker::repair_balance(&bed.pg, &bed.ledger, bed.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixed.inflight_micro, 2_000_000, "在途要被识别出来");
    assert_eq!(
        fixed.redis_after_micro, 7_000_000,
        "avail = 账本 − 在途，而不是直接写账本值"
    );
    assert!(drift_of(&bed).await.is_none(), "不变式 avail + 在途 == 账本");
}

/// 端点：权限闸、既不指定用户也不给 all 时拒绝、单用户修复、批量修复。
#[tokio::test]
async fn repair_endpoint_guards_and_repairs() {
    let bed = setup().await;
    let client = reqwest::Client::new();
    let url = format!("http://{}/admin/reconciliation/repair", bed.console);

    // 普通用户无 user.balance_adjust → 403
    let denied = client
        .post(&url)
        .bearer_auth(&bed.plain_token)
        .json(&json!({"user_id": bed.user_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);

    // 既没 user_id 也没 all：批量改余额不该是手滑的默认行为
    let vague = client
        .post(&url)
        .bearer_auth(&bed.admin_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(vague.status(), 400);
    let body: Value = vague.json().await.unwrap();
    assert_eq!(body["error"]["param"], "user_id_or_all");

    // 不存在的用户 → 404
    let missing = client
        .post(&url)
        .bearer_auth(&bed.admin_token)
        .json(&json!({"user_id": 99_999_999_i64}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    // 单用户修复
    lose_hot_balance(&bed).await;
    let ok = client
        .post(&url)
        .bearer_auth(&bed.admin_token)
        .json(&json!({"user_id": bed.user_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let body: Value = ok.json().await.unwrap();
    assert_eq!(body["repaired"], 1);
    assert_eq!(body["data"][0]["redis_after_micro"], 9_000_000);
    assert_eq!(
        bed.ledger.balance(bed.user_id).await.unwrap().as_micros(),
        9_000_000
    );

    // 批量修复：把本用户再弄漂，all 模式应把它一并带上
    lose_hot_balance(&bed).await;
    let all = client
        .post(&url)
        .bearer_auth(&bed.admin_token)
        .json(&json!({"all": true, "limit": 100_000}))
        .send()
        .await
        .unwrap();
    assert_eq!(all.status(), 200);
    assert!(drift_of(&bed).await.is_none(), "批量修复后本用户不该再漂");

    // 结算窗口保护：账目正在动时不许修（否则会把正在结算的那笔退回去）
    let moving = bed.user_id;
    let ledger2 = bed.ledger.clone();
    let churn = tokio::spawn(async move {
        // 持续制造「Redis 已扣、账本还没跟上」的中间态
        for _ in 0..40 {
            let _ = ledger2.credit(moving, Money::from_micros(1)).await;
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }
    });
    let unstable = client
        .post(&url)
        .bearer_auth(&bed.admin_token)
        .json(&json!({"user_id": bed.user_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(unstable.status(), 409, "账目在动时应拒绝修复而不是拿半截账本覆写");
    let body: Value = unstable.json().await.unwrap();
    assert_eq!(body["error"]["code"], "reconcile_unstable");
    churn.await.unwrap();
    // churn 只加 Redis 不写账本事件，本身就是一份漂移——用例造的脏数据不能留在开发库里
    // 挂着（运维页会一直显示一个假的差异用户），跑完按账本收干净
    worker::repair_balance(&bed.pg, &bed.ledger, bed.user_id)
        .await
        .unwrap();
    assert!(drift_of(&bed).await.is_none(), "用例不得留下漂移");

    // 留痕：改余额的动作必须可审计
    let audited = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs WHERE action = 'billing.reconcile_repair'"#
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert!(audited >= 2, "单修与批量修都要落审计");
}
