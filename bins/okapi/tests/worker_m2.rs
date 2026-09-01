//! M2 第一批验收：悬置预扣清理 / 三方对账 / 分区维护 / 冷却恢复 / epoch 热更。

use chrono::{Datelike, Duration as ChronoDuration, Utc};
use okapi::{gateway, worker};
use okapi_domain::Money;
use okapi_ledger::{BalanceLedger, LimitCaps, ReserveOutcome};
use sqlx::PgPool;
use uuid::Uuid;

async fn setup() -> (PgPool, BalanceLedger) {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    (pg, BalanceLedger::new(redis))
}

async fn new_user(pg: &PgPool) -> i64 {
    let suffix = Uuid::new_v4().simple().to_string();
    okapi_store::provision::create_user(pg, &format!("w-{suffix}"))
        .await
        .unwrap()
}

/// 双侧入账（Redis + PG 事件），对账应干净。
async fn credit_both(pg: &PgPool, ledger: &BalanceLedger, user_id: i64, micros: i64) {
    ledger
        .credit(user_id, Money::from_micros(micros))
        .await
        .unwrap();
    okapi_ledger::pg::record_credit(
        pg,
        user_id,
        Money::from_micros(micros),
        "recharge",
        "test",
        serde_json::json!({"reason":"test_seed"}),
    )
    .await
    .unwrap();
}

/// 悬置预扣：deadline 过期后被 worker 释放且幂等，余额恢复，事件留痕。
/// 与 replay-commit 场景合并顺序执行：两者都做全库 future 时钟扫描，
/// 并行会互清对方未过期的预扣（历史 flaky 根因）。
#[tokio::test]
// 两段 sweep 场景顺序脚本
#[allow(clippy::too_many_lines)]
async fn sweep_scenarios_sequential() {
    sweep_releases_expired_reservations().await;
    sweep_replays_commit_when_record_committed().await;
}

async fn sweep_releases_expired_reservations() {
    let (pg, ledger) = setup().await;
    let user = new_user(&pg).await;
    credit_both(&pg, &ledger, user, 10_000).await;

    let request_id = Uuid::new_v4();
    let outcome = ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: user,
                api_key_id: 1,
                request_id,
                est: Money::from_micros(500),
                caps: LimitCaps::default(),
                est_tokens: 100,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, ReserveOutcome::Reserved { .. }));
    assert_eq!(ledger.balance(user).await.unwrap().as_micros(), 9500);

    // 未过期：不清理（余额保持预扣后状态）
    let _ = worker::sweep_expired_reservations(&pg, &ledger, Utc::now())
        .await
        .unwrap();
    assert_eq!(ledger.balance(user).await.unwrap().as_micros(), 9500);

    // 越过 deadline（预扣 TTL 10min）：清理并全额释放。
    // 注意：并行测试也在跑全局 sweep，本测试以最终状态（余额/事件行）为证据，
    // 不依赖本次调用的返回列表归属。
    let future = Utc::now() + ChronoDuration::minutes(11);
    let _ = worker::sweep_expired_reservations(&pg, &ledger, future)
        .await
        .unwrap();
    assert_eq!(ledger.balance(user).await.unwrap().as_micros(), 10_000);

    // 幂等：再扫余额不变
    let _ = worker::sweep_expired_reservations(&pg, &ledger, future)
        .await
        .unwrap();
    assert_eq!(ledger.balance(user).await.unwrap().as_micros(), 10_000);

    let events = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events
           WHERE user_id = $1 AND event_type = 'refund' AND actor = 'system:worker'"#,
        user
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(events, 1, "清理必须留且只留一条事件痕迹");
}

/// 悬置预扣 + PG 终态 committed：sweep 必须重放 commit 补扣（而非免费放行）。
/// 场景：结算事务已提交但 Redis commit 丢失（进程崩溃窗口）。
async fn sweep_replays_commit_when_record_committed() {
    let (pg, ledger) = setup().await;
    let user = new_user(&pg).await;
    credit_both(&pg, &ledger, user, 10_000).await;

    let request_id = Uuid::new_v4();
    let outcome = ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: user,
                api_key_id: 1,
                request_id,
                est: Money::from_micros(500),
                caps: LimitCaps::default(),
                est_tokens: 100,
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, ReserveOutcome::Reserved { .. }));

    // 伪造"PG 已结算 240、Redis commit 丢失"的现场
    sqlx::query!(
        r#"
        INSERT INTO billing_records (request_id, user_id, api_key_id, group_code, model_name,
                                     status, amount_micro, log_type)
        VALUES ($1, $2, 1, 'default', 'm-sweep', 20, 240, 2)
        "#,
        request_id,
        user
    )
    .execute(&pg)
    .await
    .unwrap();

    let future = Utc::now() + ChronoDuration::minutes(11);
    let swept = worker::sweep_expired_reservations(&pg, &ledger, future)
        .await
        .unwrap();
    // 并行安全：本次调用可能被并行 sweep 抢先，若归属本次则校验动作类型
    if let Some(mine) = swept.iter().find(|s| s.user_id == user) {
        assert_eq!(mine.action, "commit_replayed");
        assert_eq!(mine.released_micro, 260, "预扣 500 − 实际 240 = 净退 260");
    }
    // 权威证据：余额 = 10000 − 240（补扣生效，而非免费放行）
    assert_eq!(
        ledger.balance(user).await.unwrap().as_micros(),
        10_000 - 240
    );
}

/// 对账：双侧一致不报；Redis 单边入账必须被抓出来。
#[tokio::test]
async fn reconcile_detects_one_sided_credit() {
    let (pg, ledger) = setup().await;

    let clean_user = new_user(&pg).await;
    credit_both(&pg, &ledger, clean_user, 7000).await;

    let drift_user = new_user(&pg).await;
    // 只进 Redis，不记 PG 事件（模拟丢事件/黑手改余额）
    ledger
        .credit(drift_user, Money::from_micros(3000))
        .await
        .unwrap();

    let drifts = worker::reconcile_balances(&pg, &ledger, 1_000_000)
        .await
        .unwrap();
    assert!(
        !drifts.iter().any(|d| d.user_id == clean_user),
        "双侧一致的用户不应报差异"
    );
    let hit = drifts
        .iter()
        .find(|d| d.user_id == drift_user)
        .expect("单边入账必须被对账抓出");
    assert_eq!(hit.redis_effective_micro, 3000);
    assert_eq!(hit.events_sum_micro, 0);
}

/// 分区维护：下月分区提前建好且幂等。
#[tokio::test]
async fn next_month_partitions_created_idempotently() {
    let (pg, _ledger) = setup().await;
    let now = Utc::now();
    let _ = worker::ensure_next_month_partitions(&pg, now)
        .await
        .unwrap();
    // 第二次调用必然为空（已存在）
    let second = worker::ensure_next_month_partitions(&pg, now)
        .await
        .unwrap();
    assert!(second.is_empty());

    let (y, m) = (now.year(), now.month());
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    for table in ["billing_records", "billing_events"] {
        let name = format!("{table}_y{ny}m{nm:02}");
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM pg_class WHERE relname = $1) AS "e!""#,
            name
        )
        .fetch_one(&pg)
        .await
        .unwrap();
        assert!(exists, "{name} 应已创建");
    }
}

/// 冷却恢复：cooldown 到期的 key 自动回 active。
#[tokio::test]
async fn cooled_keys_recover_after_deadline() {
    let (pg, _ledger) = setup().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let (_channel_id, key_id) = okapi_store::provision::create_channel(
        &pg,
        &format!("cool-{suffix}"),
        "openai",
        "http://127.0.0.1:9/v1",
        "cred",
        &[&format!("m-cool-{suffix}")],
        false,
        None,
    )
    .await
    .unwrap();

    sqlx::query!(
        r#"UPDATE channel_keys SET status = 2, cooldown_until = now() - interval '1 second' WHERE id = $1"#,
        key_id
    )
    .execute(&pg)
    .await
    .unwrap();

    let recovered = worker::recover_cooled_keys(&pg).await.unwrap();
    assert!(recovered >= 1);

    let status = sqlx::query_scalar!(r#"SELECT status FROM channel_keys WHERE id = $1"#, key_id)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(status, 1, "冷却到期应回 active");
}

/// epoch 热更：发布新 epoch 后 30s 轮询通道能感知并原子替换。
#[tokio::test]
async fn pricebook_hot_reloads_on_new_epoch() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let before = state.pricebook.epoch();

    let new_epoch = sqlx::query_scalar!(
        r#"INSERT INTO pricing_epochs (snapshot) VALUES ('{}'::jsonb) RETURNING epoch"#
    )
    .fetch_one(&state.pg)
    .await
    .unwrap();
    assert!(new_epoch > before);

    // 并发安全断言：其他并行测试可能同时发布 epoch，只保证单调追平
    let _ = gateway::refresh_pricebook_if_newer(&state).await.unwrap();
    assert!(
        state.pricebook.epoch() >= new_epoch,
        "热更后 epoch 必须追平已发布值（got {}, want >= {new_epoch}）",
        state.pricebook.epoch()
    );

    // 稳定后再次刷新不应回退且幂等
    let epoch_now = state.pricebook.epoch();
    let _ = gateway::refresh_pricebook_if_newer(&state).await.unwrap();
    assert!(state.pricebook.epoch() >= epoch_now, "epoch 不得回退");
}

/// 余额有效期（#1790-6）：到期清零 + expire 事件留痕 + 重置 NULL 幂等；未到期不动。
#[tokio::test]
async fn balance_expiry_drains_and_records() {
    let (pg, ledger) = setup().await;

    // 未到期用户：不动
    let fresh = new_user(&pg).await;
    credit_both(&pg, &ledger, fresh, 5_000).await;
    sqlx::query!(
        r#"UPDATE users SET balance_expires_at = now() + interval '1 hour' WHERE id = $1"#,
        fresh
    )
    .execute(&pg)
    .await
    .unwrap();

    // 已到期用户：清零
    let stale = new_user(&pg).await;
    credit_both(&pg, &ledger, stale, 7_000).await;
    sqlx::query!(
        r#"UPDATE users SET balance_expires_at = now() - interval '1 minute' WHERE id = $1"#,
        stale
    )
    .execute(&pg)
    .await
    .unwrap();

    let expired = worker::expire_balances(&pg, &ledger, Utc::now())
        .await
        .unwrap();
    let mine: Vec<_> = expired.iter().filter(|e| e.user_id == stale).collect();
    assert_eq!(mine.len(), 1, "到期用户应被清零一次");
    assert_eq!(mine[0].drained_micro, 7_000);

    assert_eq!(
        ledger.balance(stale).await.unwrap().as_micros(),
        0,
        "到期余额应清零"
    );
    assert_eq!(
        ledger.balance(fresh).await.unwrap().as_micros(),
        5_000,
        "未到期余额不得动"
    );

    // 事件留痕：expire，delta 为负；users 快照同步、expires_at 重置
    let event = sqlx::query!(
        r#"SELECT delta_micro, actor FROM billing_events
           WHERE user_id = $1 AND event_type = 'expire'"#,
        stale
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(event.delta_micro, -7_000);
    assert_eq!(event.actor, "system:worker");
    let row = sqlx::query!(
        r#"SELECT balance_micro, balance_expires_at FROM users WHERE id = $1"#,
        stale
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(row.balance_micro, 0, "PG 快照应同步清零");
    assert!(row.balance_expires_at.is_none(), "到期时间应重置防重扫");

    // 幂等：重跑不再产出
    let again = worker::expire_balances(&pg, &ledger, Utc::now())
        .await
        .unwrap();
    assert!(again.iter().all(|e| e.user_id != stale), "重跑不得重复清零");
}

/// 保留策略（#1790-1）：超期月分区被裁剪；关闭（0/缺省）不动；DEFAULT 分区永不动。
#[tokio::test]
async fn retention_drops_expired_partitions() {
    let (pg, _ledger) = setup().await;

    // 造一个远古分区（2020-01，与现网分区无重叠）
    sqlx::query(sqlx::AssertSqlSafe(
        "CREATE TABLE IF NOT EXISTS billing_records_y2020m01 PARTITION OF billing_records \
         FOR VALUES FROM ('2020-01-01') TO ('2020-02-01')"
            .to_owned(),
    ))
    .execute(&pg)
    .await
    .unwrap();

    // 关闭：不裁剪
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'retention_months'"#)
        .execute(&pg)
        .await
        .unwrap();
    let dropped = worker::drop_expired_partitions(&pg, Utc::now())
        .await
        .unwrap();
    assert!(dropped.is_empty(), "缺省必须永久保留");

    // 12 个月保留：2020 分区应被裁剪
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('retention_months', '12'::jsonb)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#
    )
    .execute(&pg)
    .await
    .unwrap();
    let dropped = worker::drop_expired_partitions(&pg, Utc::now())
        .await
        .unwrap();
    assert!(
        dropped.iter().any(|n| n == "billing_records_y2020m01"),
        "远古分区应被裁剪：{dropped:?}"
    );
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM pg_class WHERE relname = 'billing_records_y2020m01') AS "e!""#
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(!exists, "分区表应已 DROP");

    // 近月分区不动（下月分区由维护任务建，裁剪窗口外）
    assert!(
        dropped
            .iter()
            .all(|n| !n.contains(&format!("y{}", chrono::Utc::now().format("%Y")))),
        "近月分区不得被裁剪：{dropped:?}"
    );

    // 清理配置，避免影响并行 worker 逻辑
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'retention_months'"#)
        .execute(&pg)
        .await
        .unwrap();
}
