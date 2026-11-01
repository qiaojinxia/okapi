//! 三方对账：billing_events 重放 ↔ Redis 热余额（含在途）↔ users.balance_micro。
//! 网关的热余额自愈、控制台的手动对账与 worker 的定时对账共用；发现漂移后的告警在 worker（`reconcile_and_notify`）。
use super::repair::SETTLE_WINDOW;
use okapi_ledger::{BalanceLedger, Pool};
use sqlx::PgPool;

/// 对账差异行（两池任一不平即入列，IMPLEMENTATION §11.28）。
#[derive(Debug)]
pub struct BalanceDrift {
    pub user_id: i64,
    /// Wallet truth: current events plus carried financial history (pool=0).
    pub events_sum_micro: i64,
    /// Redis 钱包有效余额 = avail + 钱包在途（消除在途噪声后应等于事件和）。
    pub redis_effective_micro: i64,
    /// users.balance_micro 快照列。
    pub pg_snapshot_micro: i64,
    /// Subscription truth: current events plus carried financial history (pool=1).
    pub sub_events_sum_micro: i64,
    /// 订阅池：Redis sub + 订阅在途。
    pub sub_redis_effective_micro: i64,
}

impl BalanceDrift {
    /// 任一池的 Redis 有效值、或钱包展示快照，与账本总额对不上。
    fn is_drifting(&self) -> bool {
        self.redis_effective_micro != self.events_sum_micro
            || self.pg_snapshot_micro != self.events_sum_micro
            || self.sub_redis_effective_micro != self.sub_events_sum_micro
    }
}

/// Redis 两池有效值：`(avail + 钱包在途, sub + 订阅在途)`。
pub(super) async fn redis_effective(
    ledger: &BalanceLedger,
    user_id: i64,
) -> anyhow::Result<(i64, i64)> {
    let avail = ledger.balance(user_id).await?.as_micros();
    let (sub, _) = ledger.sub_balance(user_id).await?;
    let epoch = ledger.subscription_window(user_id).await?;
    let mut inflight = (0_i64, 0_i64);
    for r in ledger.list_reservations(user_id).await? {
        match r.pool {
            Pool::Wallet => inflight.0 = inflight.0.saturating_add(r.amount.as_micros()),
            Pool::Subscription => {
                if r.source_window.is_none() || r.source_window == epoch {
                    inflight.1 = inflight.1.saturating_add(r.amount.as_micros());
                }
            }
        }
    }
    let (held_wallet, held_sub) = okapi_ledger::holds::inflight(ledger, user_id).await?;
    inflight.0 = inflight.0.saturating_add(held_wallet.as_micros());
    inflight.1 = inflight.1.saturating_add(held_sub.as_micros());
    Ok((
        avail.saturating_add(inflight.0),
        sub.as_micros().saturating_add(inflight.1),
    ))
}

/// 一轮对账最多逐条记这么多个用户的差异，也是 `drift` 通知里带的用户数。
pub(crate) const DRIFT_LOG_LIMIT: usize = 20;

/// 差异逐条记 error 日志，超出 [`DRIFT_LOG_LIMIT`] 的合并成一条汇总。Redis 被清空 / 回滚到旧快照时
/// 所有用户一起漂移，逐条记就是每轮（增量 5 分钟一次、全量每日）写出用户数那么多行 error，
/// 把真正要看的日志淹掉；完整名单走 `GET /admin/reconciliation`。
pub(crate) fn log_drifts(drifts: &[BalanceDrift]) {
    for d in drifts.iter().take(DRIFT_LOG_LIMIT) {
        tracing::error!(
            user_id = d.user_id,
            events_sum = d.events_sum_micro,
            redis_effective = d.redis_effective_micro,
            pg_snapshot = d.pg_snapshot_micro,
            "对账差异（不会自愈：按账本修复走 /admin/reconciliation/repair）"
        );
    }
    if drifts.len() > DRIFT_LOG_LIMIT {
        tracing::error!(
            count = drifts.len(),
            logged = DRIFT_LOG_LIMIT,
            "对账差异用户数超过逐条日志上限；完整名单见 GET /admin/reconciliation"
        );
    }
}

/// 三方对账（docs/database.md §5）：返回不一致的用户。遍历全部用户。
pub async fn reconcile_balances(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
) -> anyhow::Result<Vec<BalanceDrift>> {
    reconcile_scoped(pg, ledger, limit, None).await
}

/// 只对账 `since` 之后有过账本事件的用户（周期任务的增量轮）。
///
/// 账本总额是终身事件求和，每 5 分钟对全部用户各求一遍等于反复扫整张 billing_events；
/// 没有新事件的用户 PG 侧两个数都不会变，只剩 Redis 被外部改动这一种漂移来源，交给
/// 每日一次的全量轮（[`RECONCILE_FULL_INTERVAL`]）兜底。活跃用户从 created_at 上的 BRIN
/// 索引取，只读近期数据块。
pub async fn reconcile_active_balances(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
    since: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<BalanceDrift>> {
    reconcile_scoped(pg, ledger, limit, Some(since)).await
}

pub(super) struct ReconcileRow {
    pub(super) user_id: i64,
    pub(super) balance_micro: i64,
    pub(super) events_sum: i64,
    pub(super) sub_events_sum: i64,
}

pub(super) async fn reconcile_page(
    conn: &mut sqlx::PgConnection,
    limit: i64,
    cursor: i64,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> sqlx::Result<Vec<ReconcileRow>> {
    let Some(since) = since else {
        let rows = sqlx::query!(
            r#"
        WITH page AS (
            SELECT id, balance_micro FROM users
            WHERE deleted_at IS NULL AND id > $2
            ORDER BY id LIMIT $1
        )
        SELECT u.id AS user_id,
               u.balance_micro,
               COALESCE(e.wallet_sum, 0)::bigint AS "events_sum!",
               COALESCE(e.sub_sum, 0)::bigint AS "sub_events_sum!"
        FROM page u
        LEFT JOIN LATERAL (
            SELECT SUM(delta_micro) FILTER (WHERE pool = 0) AS wallet_sum,
                   SUM(delta_micro) FILTER (WHERE pool = 1) AS sub_sum
            FROM billing_balance_totals
            WHERE user_id = u.id
        ) e ON true
        ORDER BY u.id
        "#,
            limit,
            cursor
        )
        .fetch_all(conn)
        .await?;
        return Ok(rows
            .into_iter()
            .map(|r| ReconcileRow {
                user_id: r.user_id,
                balance_micro: r.balance_micro,
                events_sum: r.events_sum,
                sub_events_sum: r.sub_events_sum,
            })
            .collect());
    };
    let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
        r"
        WITH active AS (
            SELECT DISTINCT user_id FROM billing_events
            WHERE created_at >= $3 AND user_id > $2
        ), page AS (
            SELECT u.id, u.balance_micro FROM users u JOIN active a ON a.user_id = u.id
            WHERE u.deleted_at IS NULL
            ORDER BY u.id LIMIT $1
        )
        SELECT u.id, u.balance_micro,
               COALESCE(e.wallet_sum, 0)::bigint,
               COALESCE(e.sub_sum, 0)::bigint
        FROM page u
        LEFT JOIN LATERAL (
            SELECT SUM(delta_micro) FILTER (WHERE pool = 0) AS wallet_sum,
                   SUM(delta_micro) FILTER (WHERE pool = 1) AS sub_sum
            FROM billing_balance_totals
            WHERE user_id = u.id
        ) e ON true
        ORDER BY u.id
        ",
    )
    .bind(limit)
    .bind(cursor)
    .bind(since)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(user_id, balance_micro, events_sum, sub_events_sum)| ReconcileRow {
                user_id,
                balance_micro,
                events_sum,
                sub_events_sum,
            },
        )
        .collect())
}

pub(crate) async fn reconcile_scoped(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> anyhow::Result<Vec<BalanceDrift>> {
    let mut drifts = Vec::new();
    let mut cursor = 0_i64;
    let (mut skipped, mut last_error) = (0_u64, None::<anyhow::Error>);
    loop {
        let mut history = okapi_store::history::read(pg).await?;
        let rows = reconcile_page(&mut history, limit.clamp(1, 1000), cursor, since).await?;
        // Release the retention lock and PG connection before any Redis IO.
        history.commit().await?;

        if rows.is_empty() {
            break;
        }
        cursor = rows.last().map_or(cursor, |row| row.user_id);
        for row in rows {
            // 一个用户的 Redis 读失败只跳过他：整轮报错会把前面几页已扫出的漂移一起丢掉，告警晚一轮。
            // 跳过的人数最后汇总成一条日志（Redis 整个不可用时不必每个用户刷一行）
            let (wallet, sub) = match redis_effective(ledger, row.user_id).await {
                Ok(effective) => effective,
                Err(error) => {
                    skipped += 1;
                    last_error = Some(error);
                    continue;
                }
            };
            let drift = BalanceDrift {
                user_id: row.user_id,
                events_sum_micro: row.events_sum,
                redis_effective_micro: wallet,
                pg_snapshot_micro: row.balance_micro,
                sub_events_sum_micro: row.sub_events_sum,
                sub_redis_effective_micro: sub,
            };
            if drift.is_drifting() {
                drifts.push(drift);
            }
        }
    }
    if let Some(error) = last_error {
        tracing::warn!(skipped, %error, "reconcile skipped users whose Redis balance could not be read");
    }
    Ok(drifts)
}

/// 单个用户此刻的账本总额、展示快照与 Redis 有效值。None = 用户不存在或已删除。
pub(super) async fn sample_drift(
    pg: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
) -> anyhow::Result<Option<BalanceDrift>> {
    let mut history = okapi_store::history::read(pg).await?;
    let row: Option<(i64, i64, i64)> = sqlx::query_as(
        r"
        SELECT u.balance_micro,
               COALESCE((SELECT SUM(delta_micro) FROM billing_balance_totals e WHERE e.user_id = u.id AND e.pool = 0), 0)::bigint,
               COALESCE((SELECT SUM(delta_micro) FROM billing_balance_totals e WHERE e.user_id = u.id AND e.pool = 1), 0)::bigint
        FROM users u WHERE u.id = $1 AND u.deleted_at IS NULL
        ",
    )
    .bind(user_id)
    .fetch_optional(&mut *history)
    .await?;
    // Release the retention lock and PG connection before any Redis IO.
    history.commit().await?;
    let Some((pg_snapshot_micro, events_sum_micro, sub_events_sum_micro)) = row else {
        return Ok(None);
    };
    let (redis_effective_micro, sub_redis_effective_micro) =
        redis_effective(ledger, user_id).await?;
    Ok(Some(BalanceDrift {
        user_id,
        events_sum_micro,
        redis_effective_micro,
        pg_snapshot_micro,
        sub_events_sum_micro,
        sub_redis_effective_micro,
    }))
}

/// 首轮扫出的差异隔一个结算窗口再看一眼，仍不平的才算漂移（报告复核时的读数）。
///
/// 结算是响应返回后的后台任务：Redis commit 与 PG 事件落库不在同一瞬间（[`SETTLE_WINDOW`]），
/// 一遍扫描恰好落在两者之间，就会看到「Redis 比账本低一笔」。线上 2026-10-29 因此记过一条
/// 「不会自愈」的 error，差额正好是同一秒结算的那笔请求，随后十几小时零差异。真漂移
/// （Redis 被清空、回滚到旧快照）一直都在，不会被复核放过；复核读失败时保留首轮读数，宁可多报。
pub(crate) async fn confirm_drifts(
    pg: &PgPool,
    ledger: &BalanceLedger,
    first: Vec<BalanceDrift>,
) -> Vec<BalanceDrift> {
    if first.is_empty() {
        return first;
    }
    tokio::time::sleep(SETTLE_WINDOW).await;
    let mut confirmed = Vec::with_capacity(first.len());
    for earlier in first {
        match sample_drift(pg, ledger, earlier.user_id).await {
            Ok(Some(now)) if now.is_drifting() => confirmed.push(now),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(user_id = earlier.user_id, %error, "reconcile re-check failed; keeping the first reading");
                confirmed.push(earlier);
            }
        }
    }
    confirmed
}

#[cfg(test)]
mod drift_log_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tracing_subscriber::prelude::*;

    struct CountErrors(Arc<AtomicUsize>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountErrors {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() == tracing::Level::ERROR {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn drift(user_id: i64) -> BalanceDrift {
        BalanceDrift {
            user_id,
            events_sum_micro: 10,
            redis_effective_micro: 20,
            pg_snapshot_micro: 10,
            sub_events_sum_micro: 0,
            sub_redis_effective_micro: 0,
        }
    }

    fn error_lines(count: i64) -> usize {
        let seen = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(CountErrors(seen.clone()));
        tracing::subscriber::with_default(subscriber, || {
            log_drifts(&(1..=count).map(drift).collect::<Vec<_>>());
        });
        seen.load(Ordering::SeqCst)
    }

    /// 全员漂移（Redis 被清空 / 回滚）时每轮只写有限几行：逐条前 20 个 + 一条汇总。
    #[test]
    fn a_mass_drift_writes_a_bounded_number_of_error_lines() {
        assert_eq!(error_lines(0), 0);
        assert_eq!(error_lines(3), 3, "差异少时仍是逐条，没有汇总行");
        assert_eq!(error_lines(20), 20, "刚好到上限：逐条，没有汇总行");
        assert_eq!(error_lines(21), 21, "超出一个：20 条逐条 + 1 条汇总");
        assert_eq!(error_lines(100_000), 21, "再多也是这 21 行");
    }
}
