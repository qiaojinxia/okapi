//! 漂移修复：结算窗口过后仍不一致的余额按 PG 账本重建（网关的热余额自愈、控制台的手动修复与 worker 的定时对账共用）。
use super::reconcile::{BalanceDrift, reconcile_balances, redis_effective};
use okapi_ledger::BalanceLedger;
use sqlx::{Connection, PgPool};
use std::time::Duration;

/// 一个用户的对账修复结果。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct BalanceRepair {
    pub user_id: i64,
    /// Wallet total from billing_balance_totals (pool=0).
    pub events_sum_micro: i64,
    pub redis_before_micro: i64,
    pub redis_after_micro: i64,
    /// 被保留的钱包在途预扣合计（`avail + 在途 == 账本` 才是对账不变式）。
    pub inflight_micro: i64,
    pub pg_snapshot_before_micro: i64,
    /// 订阅池账本权威值（pool=1 求和）与修复前后值。
    pub sub_events_sum_micro: i64,
    pub sub_redis_before_micro: i64,
    pub sub_redis_after_micro: i64,
}

/// 按账本重建单个用户的两池热余额与展示快照。返回 None = 用户不存在/已删除。
///
/// 先写 Redis 再写 PG 快照：Redis 是**唯一**决定能不能扣费的那份数据（`reserve.lua`
/// 只读它、不回源 PG），中途失败的话宁可留一个滞后的展示快照，也不要留一个仍然
/// 拒服务的热账本。各步都幂等，重跑即可收敛。
pub async fn repair_balance(
    pg: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
) -> anyhow::Result<Option<BalanceRepair>> {
    let mut guard = okapi_ledger::holds::UserGuard::acquire(pg, user_id).await?;
    guard.synchronize(ledger).await?;
    let mut history = guard.connection()?.begin().await?;
    okapi_store::history::read_lock(&mut history).await?;
    let row = sqlx::query!(
        r#"
        SELECT u.balance_micro,
               COALESCE((SELECT SUM(delta_micro) FROM billing_balance_totals e WHERE e.user_id = u.id AND e.pool = 0), 0)::bigint
                   AS "events_sum!",
               COALESCE((SELECT SUM(delta_micro) FROM billing_balance_totals e WHERE e.user_id = u.id AND e.pool = 1), 0)::bigint
                   AS "sub_events_sum!"
        FROM users u WHERE u.id = $1 AND u.deleted_at IS NULL
        "#,
        user_id
    )
    .fetch_optional(&mut *history)
    .await?;
    history.commit().await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let repaired = guard
        .repair(
            ledger,
            okapi_domain::Money::from_micros(row.events_sum),
            okapi_domain::Money::from_micros(row.sub_events_sum),
        )
        .await?;
    let outcome = repaired.wallet;
    let sub_outcome = repaired.subscription;
    sqlx::query!(
        r#"UPDATE users SET balance_micro = $2, updated_at = now() WHERE id = $1"#,
        user_id,
        row.events_sum
    )
    .execute(guard.connection()?)
    .await?;
    Ok(Some(BalanceRepair {
        user_id,
        events_sum_micro: row.events_sum,
        redis_before_micro: outcome.before.as_micros(),
        redis_after_micro: outcome.after.as_micros(),
        inflight_micro: outcome.inflight.as_micros(),
        pg_snapshot_before_micro: row.balance_micro,
        sub_events_sum_micro: row.sub_events_sum,
        sub_redis_before_micro: sub_outcome.before.as_micros(),
        sub_redis_after_micro: sub_outcome.after.as_micros(),
    }))
}

/// 结算窗口：Redis commit 与 PG 事件落库不在同一瞬间（结算是响应返回后的后台任务，
/// 还要过 `settle_gate` 信号量），窗口内 Redis 比账本**低一笔**——对账会把它看成漂移。
///
/// 这对修复是致命的：照着尚未记入那笔扣费的账本去重建，等于把正在结算的钱退回去。
/// 所以修复前采两次样，只有两次完全一致才动手；对账告警同样隔一个窗口复核
/// （[`confirm_drifts`]），免得把窗口里的瞬时态报成「不会自愈」的漂移。
pub(super) const SETTLE_WINDOW: Duration = Duration::from_millis(1500);

/// 漂移是否稳定（不是结算窗口的瞬时态）。稳定则返回该用户的账本权威值。
pub async fn stable_drift(
    pg: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
) -> anyhow::Result<Option<i64>> {
    let sample = |uid: i64| async move {
        let mut history = okapi_store::history::read(pg).await?;
        let row = sqlx::query!(
            r#"
            SELECT COALESCE((SELECT SUM(delta_micro) FROM billing_balance_totals e WHERE e.user_id = $1 AND e.pool = 0), 0)::bigint
                       AS "events_sum!",
                   COALESCE((SELECT SUM(delta_micro) FROM billing_balance_totals e WHERE e.user_id = $1 AND e.pool = 1), 0)::bigint
                       AS "sub_events_sum!"
            FROM users u WHERE u.id = $1 AND u.deleted_at IS NULL
            "#,
            uid
        )
        .fetch_optional(&mut *history)
        .await?;
        history.commit().await?;
        let Some(row) = row else {
            return Ok::<Option<(i64, (i64, i64), i64)>, anyhow::Error>(None);
        };
        let effective = redis_effective(ledger, uid).await?;
        Ok(Some((row.events_sum, effective, row.sub_events_sum)))
    };

    let Some(first) = sample(user_id).await? else {
        return Ok(None);
    };
    tokio::time::sleep(SETTLE_WINDOW).await;
    let Some(second) = sample(user_id).await? else {
        return Ok(None);
    };
    Ok((first == second).then_some(second.0))
}

/// 批量修复当前扫出的全部漂移用户。
///
/// 两遍对账中间隔一个结算窗口，只修**两遍都在漂且数字没变**的用户——有流量时总有请求
/// 正处在「Redis 已扣、PG 事件还没落」的中间态，一遍扫描分不清它和真丢数据。
pub async fn repair_drifted(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
) -> anyhow::Result<Vec<BalanceRepair>> {
    let first = reconcile_balances(pg, ledger, limit).await?;
    if first.is_empty() {
        return Ok(Vec::new());
    }
    tokio::time::sleep(SETTLE_WINDOW).await;
    let second = reconcile_balances(pg, ledger, limit).await?;
    let stable: std::collections::HashMap<i64, &BalanceDrift> =
        second.iter().map(|d| (d.user_id, d)).collect();

    let mut out = Vec::new();
    for d in &first {
        // 两遍数字完全一致才算真漂移；结算中的账户两遍必然不同，跳过
        let Some(again) = stable.get(&d.user_id) else {
            continue;
        };
        if again.events_sum_micro != d.events_sum_micro
            || again.redis_effective_micro != d.redis_effective_micro
            || again.sub_events_sum_micro != d.sub_events_sum_micro
            || again.sub_redis_effective_micro != d.sub_redis_effective_micro
        {
            continue;
        }
        // 一个用户修不了（锁超时、同步冲突）只跳过他，别让后面真漂移的用户等下一轮
        match repair_balance(pg, ledger, d.user_id).await {
            Ok(Some(fixed)) => out.push(fixed),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(user_id = d.user_id, %error, "balance repair failed for this user; skipped");
            }
        }
    }
    Ok(out)
}
