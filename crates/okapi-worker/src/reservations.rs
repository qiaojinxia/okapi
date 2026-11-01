//! 悬置预扣清理：超时未结算的 Redis 预扣按 deadline 释放并记事件。

use okapi_ledger::BalanceLedger;
use sqlx::{Connection, PgPool};

/// 已清理的悬置预扣。
#[derive(Debug)]
pub struct SweptReservation {
    pub user_id: i64,
    pub request_id: uuid::Uuid,
    /// refund 路径 = 释放金额；commit 补偿路径 = 补扣后的净释放（reserved − actual）。
    pub released_micro: i64,
    /// "refund" | "commit_replayed"。
    pub action: &'static str,
}

/// 悬置预扣清理（docs/database.md §5）：deadline 已过的在途预扣，
/// **按 billing_records 终态决定补偿方向**。
///
/// - 终态 committed（PG 结算成功、Redis commit 丢失）→ 重放 commit 补扣实际金额；
/// - 终态 refunded/failed 或无记录（请求死亡）→ 全额 refund。
///
/// 场景：gateway 崩溃/断电/Redis 抖动导致 reserve 后未终结。幂等。
// 终态判定 + 两种补偿路径的完整语义放同一视野更可读
#[allow(clippy::too_many_lines)]
pub async fn sweep_expired_reservations(
    pg: &PgPool,
    ledger: &BalanceLedger,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<SweptReservation>> {
    okapi_ledger::sync::recover_pending(pg, ledger, 100).await?;
    okapi_ledger::transfers::recover_pending(pg, ledger, 100).await?;
    let now_ms = now.timestamp_millis();
    let mut swept = Vec::new();
    let mut cursor = 0i64;
    loop {
        let user_ids = sqlx::query_scalar!(
            "SELECT id FROM users WHERE deleted_at IS NULL AND id>$1 ORDER BY id LIMIT 1000",
            cursor
        )
        .fetch_all(pg)
        .await?;
        if user_ids.is_empty() {
            break;
        }
        // 一页用户的预扣一次往返读完：此前逐用户 HGETALL，每分钟要扫全部用户就是用户数那么多次往返。
        // 读到处理之间预扣可能已被正常结算，但下面的 commit / refund 本就幂等（`closed=false` 即跳过）；
        // 预扣的 deadline 创建后不会被延长，读到的过期判断不会变成误判。
        let held = ledger.list_reservations_many(&user_ids).await?;
        for (user_id, reservations) in user_ids.into_iter().zip(held) {
            cursor = user_id;
            // 单个用户的锁超时、坏回执或 Redis 抖动只跳过这个用户：此前一处 `?` 就中止整轮，
            // 下一轮又从头卡在同一个用户上，id 更大的用户的过期预扣永远收不回来。
            let user_sweep: anyhow::Result<()> = async {
            let reservations = reservations?;
            if !reservations.iter().any(|r| r.deadline_ms < now_ms) {
                return Ok(());
            }
            let mut guard = okapi_ledger::holds::UserGuard::acquire(pg, user_id).await?;
            if let Err(error) = guard.synchronize(ledger).await {
                tracing::error!(user_id, %error, "skip expiry while durable settlement is pending");
                return Ok(());
            }
            for reservation in reservations {
                if reservation.deadline_ms >= now_ms {
                    continue;
                }
                // Serialize an image result/ledger commit with expiry. Otherwise the result
                // transaction could commit immediately after this sweep had refunded its hold.
                let mut tx = guard.connection()?.begin().await?;
                okapi_store::image_tasks::lock_for_balance_in_tx(&mut tx, reservation.request_id)
                    .await?;
                okapi_store::history::read_lock(&mut tx).await?;
                let terminal_query = sqlx::query!(
                    r#"
                SELECT status AS "status!", amount_micro AS "amount_micro!" FROM billing_financial_records
                WHERE request_id = $1
                ORDER BY created_at DESC
                LIMIT 1
                "#,
                    reservation.request_id
                );
                let terminal = terminal_query.fetch_optional(&mut *tx).await?;

                if let Some(rec) = &terminal
                    && rec.status == 20
                {
                    // PG 已结算：重放 Redis commit（补扣 actual，多退少补）；
                    // commit 事件已在结算事务中写过，此处不重复记账。
                    let outcome = ledger
                        .commit(
                            user_id,
                            reservation.api_key_id,
                            reservation.request_id,
                            okapi_domain::Money::from_micros(rec.amount_micro),
                        )
                        .await?;
                    if let okapi_ledger::CommitOutcome::Committed { refund_delta, .. } = outcome {
                        swept.push(SweptReservation {
                            user_id,
                            request_id: reservation.request_id,
                            released_micro: refund_delta.as_micros(),
                            action: "commit_replayed",
                        });
                    }
                    continue;
                }

                let refund = ledger
                    .refund(user_id, reservation.api_key_id, reservation.request_id)
                    .await?;
                let released = refund.released;
                if !refund.closed {
                    continue; // 竞争：已被正常终结
                }
                let event_payload = serde_json::json!({
                    "reason": "reservation_expired",
                    "released_micro": released.as_micros(),
                });
                let event_pool = refund.pool.as_i16();
                let event_query = sqlx::query!(
                    r#"
                INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, payload, actor, pool)
                VALUES ($1, $2, 'refund', 0, $3, 'system:worker', $4)
                "#,
                    user_id,
                    reservation.request_id,
                    event_payload,
                    event_pool
                );
                event_query.execute(&mut *tx).await?;
                tx.commit().await?;
                swept.push(SweptReservation {
                    user_id,
                    request_id: reservation.request_id,
                    released_micro: released.as_micros(),
                    action: "refund",
                });
            }
            Ok(())
            }
            .await;
            if let Err(error) = user_sweep {
                tracing::error!(user_id, %error, "expired reservations of this user deferred");
            }
        }
    }
    Ok(swept)
}
