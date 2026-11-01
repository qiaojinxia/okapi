//! 定时对账的告警：对账本身在 `kernel::reconcile`，这里把复核后仍在的漂移记日志并派发通知。
use super::notify;
use okapi_kernel::reconcile::{
    BalanceDrift, DRIFT_LOG_LIMIT, confirm_drifts, log_drifts, reconcile_scoped,
};
use okapi_ledger::BalanceLedger;
use sqlx::PgPool;

/// 对账一轮 + 告警（`since` 见 [`okapi_kernel::reconcile::reconcile_active_balances`]，None = 全量）：有差异（隔一个结算窗口复核后仍在）就记 error 日志（逐条最多 20 个用户，其余合并成一条汇总）并派发 `drift`（只带前 20 个用户，够运维定位即可）。
///
/// 载荷拼在这里而不是 worker 主循环的 `select!` 臂里，理由同 `notify::balance_low_and_notify`。
pub async fn reconcile_and_notify(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
    since: Option<chrono::DateTime<chrono::Utc>>,
    notifier: &notify::Notifier,
) -> anyhow::Result<Vec<BalanceDrift>> {
    let drifts = reconcile_scoped(pg, ledger, limit, since).await?;
    // 先过结算窗口复核：只在窗口里存在的差异不是漂移，不值得一条「不会自愈」的 error 和一次通知
    let drifts = confirm_drifts(pg, ledger, drifts).await;
    if drifts.is_empty() {
        tracing::debug!("对账零差异");
        return Ok(drifts);
    }
    log_drifts(&drifts);
    let users: Vec<i64> = drifts
        .iter()
        .take(DRIFT_LOG_LIMIT)
        .map(|d| d.user_id)
        .collect();
    notifier
        .dispatch(
            "drift",
            &serde_json::json!({ "count": drifts.len(), "user_ids": users }),
        )
        .await;
    Ok(drifts)
}
