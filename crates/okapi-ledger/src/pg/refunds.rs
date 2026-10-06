use super::{LedgerError, Money, PgPool, Pool, SETTLEMENT_LOCK_NS, Uuid};

/// 管理员按日志退款的结果。
#[derive(Debug, Clone)]
pub struct AdminRefund {
    pub user_id: i64,
    pub amount: Money,
    /// Spendable credit may be zero when the original subscription period ended.
    pub credit: Money,
    /// 原请求由哪个池付：Redis 侧回补要回到同一池。
    pub pool: Pool,
}

async fn lock_refundable(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request_id: Uuid,
) -> Result<(), LedgerError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(SETTLEMENT_LOCK_NS)
        .bind(request_id.to_string())
        .execute(&mut **tx)
        .await?;
    let pending = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM billing_sync WHERE request_id=$1) AS "pending!""#,
        request_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if pending {
        return Err(LedgerError::HoldRecoveryRequired);
    }
    Ok(())
}

/// 管理员按日志退款（IMPLEMENTATION §5.3，#1790-10）。
///
/// PG 单事务完成：状态翻转（committed→refunded，幂等闸）、refund 事件、快照列回补、
/// key 用量回冲、outbox 负额冲销行（chsink 消费后 CH/MV 口径自动一致）。
/// PG-only helper. Online callers use operations::refund to atomically enqueue
/// the Redis recovery intent in the same transaction.
pub async fn admin_refund(
    pool: &PgPool,
    request_id: Uuid,
    reason: &str,
    actor: &str,
) -> Result<Option<AdminRefund>, LedgerError> {
    let mut tx = pool.begin().await?;
    let result = admin_refund_in_tx(&mut tx, request_id, reason, actor).await?;
    tx.commit().await?;
    Ok(result)
}

/// The caller owns the transaction and must hold the user lock through Redis credit.
pub async fn admin_refund_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request_id: Uuid,
    reason: &str,
    actor: &str,
) -> Result<Option<AdminRefund>, LedgerError> {
    okapi_store::history::read_lock(tx).await?;
    lock_refundable(tx, request_id).await?;

    let Some((rec, archived)) = refundable(tx, request_id).await? else {
        return Ok(None);
    };
    let pool = Pool::from_i16(rec.pool);
    let reversal = refund_payload(request_id, &rec, pool, archived)?;
    let expired =
        crate::windows::expired_refund(tx, rec.user_id, pool, rec.source_window.as_deref()).await?;
    let credit = if expired {
        Money::ZERO
    } else {
        Money::from_micros(rec.amount_micro)
    };
    if archived {
        sqlx::query!(
            "UPDATE billing_record_receipts SET status=30 WHERE request_id=$1 AND status=20",
            request_id
        )
        .execute(&mut **tx)
        .await?;
    } else {
        sqlx::query!(
            "UPDATE billing_records SET status=30 WHERE request_id=$1 AND status=20",
            request_id
        )
        .execute(&mut **tx)
        .await?;
    }

    sqlx::query!(
        r#"
        INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, payload, actor, pool)
        VALUES ($1, $2, 'refund', $3, $4, $5, $6)
        "#,
        rec.user_id,
        request_id,
        rec.amount_micro,
        serde_json::json!({ "reason": reason, "tags": ["admin_refund"] }),
        actor,
        pool.as_i16()
    )
    .execute(&mut **tx)
    .await?;

    if expired && rec.amount_micro != 0 {
        crate::windows::revise_expiry(
            tx,
            rec.user_id,
            request_id,
            rec.amount_micro
                .checked_neg()
                .ok_or(LedgerError::InvalidSettlement)?,
            rec.source_window.as_deref(),
            "expired_window_admin_refund",
        )
        .await?;
    }
    if pool == Pool::Wallet {
        sqlx::query!(
            r#"UPDATE users SET balance_micro = balance_micro + $2, updated_at = now() WHERE id = $1"#,
            rec.user_id,
            rec.amount_micro
        )
        .execute(&mut **tx)
        .await?;
    }

    if let Some(key_id) = rec.api_key_id {
        sqlx::query!(
            r#"UPDATE api_keys SET used_micro = used_micro - $2 WHERE id = $1"#,
            key_id,
            rec.amount_micro
        )
        .execute(&mut **tx)
        .await?;
    }

    reverse_outbox(tx, reversal).await?;

    Ok(Some(AdminRefund {
        user_id: rec.user_id,
        amount: Money::from_micros(rec.amount_micro),
        credit,
        pool,
    }))
}

async fn reverse_outbox(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    payload: serde_json::Value,
) -> Result<(), LedgerError> {
    sqlx::query!(
        r#"INSERT INTO billing_outbox (topic, payload) VALUES ('billing.refunded', $1)"#,
        payload
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn refund_payload(
    request_id: Uuid,
    rec: &RefundRecord,
    pool: Pool,
    archived: bool,
) -> Result<serde_json::Value, LedgerError> {
    // Receipts retain the original snapshot but have no separate epoch column.
    let pricing_epoch = if archived {
        rec.pricing_snapshot
            .as_ref()
            .and_then(|s| s.get("epoch"))
            .and_then(serde_json::Value::as_i64)
    } else {
        rec.pricing_epoch
    };
    // CH 负额冲销行（log_type=6 退款，对齐 new-api；token 事实保留不冲）
    Ok(serde_json::json!({
        "request_id": request_id,
        "user_id": rec.user_id,
        "api_key_id": rec.api_key_id,
        "group": rec.group_code,
        "model": rec.model_name,
        "channel_id": rec.channel_id,
        "channel_key_id": rec.channel_key_id,
        "log_type": 6,
        "status": 30,
        "prompt_tokens": 0,
        "cached_tokens": 0,
        "completion_tokens": 0,
        "reasoning_tokens": 0,
        "amount_micro": reverse_amount(rec.amount_micro)?,
        "original_amount_micro": reverse_amount(rec.original_amount_micro)?,
        "discount_micro": reverse_amount(rec.discount_micro)?,
        "upstream_cost_micro": reverse_amount(rec.upstream_cost_micro.unwrap_or(0))?,
        "upstream_cost_known": rec.upstream_cost_micro.is_some(),
        "pricing_epoch": pricing_epoch,
        "ratio_snapshot": rec.pricing_snapshot.as_ref().map(std::string::ToString::to_string).unwrap_or_default(),
        "is_stream": rec.is_stream,
        "retry_count": 0,
        "failover_count": 0,
        "error_code": null,
        "upstream_status": null,
        "upstream_request_id": null,
        "node": rec.node,
        "sticky_layer": 0,
        "client_type": "",
        "pool": pool.as_i16(),
    }))
}

fn reverse_amount(value: i64) -> Result<i64, LedgerError> {
    value.checked_neg().ok_or(LedgerError::InvalidSettlement)
}

struct RefundRecord {
    user_id: i64,
    api_key_id: Option<i64>,
    group_code: Option<String>,
    model_name: String,
    channel_id: Option<i64>,
    channel_key_id: Option<i64>,
    amount_micro: i64,
    original_amount_micro: i64,
    discount_micro: i64,
    upstream_cost_micro: Option<i64>,
    pricing_epoch: Option<i64>,
    pricing_snapshot: Option<serde_json::Value>,
    is_stream: bool,
    node: Option<String>,
    pool: i16,
    source_window: Option<String>,
}
async fn refundable(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request_id: Uuid,
) -> Result<Option<(RefundRecord, bool)>, LedgerError> {
    let live = sqlx::query_as!(
        RefundRecord,
        "SELECT user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,
        amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_epoch,pricing_snapshot,is_stream,node,pool,source_window
        FROM billing_records WHERE request_id=$1 AND status=20 FOR UPDATE",
        request_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(rec) = live {
        return Ok(Some((rec, false)));
    }
    let archived = sqlx::query_as!(
        RefundRecord,
        "SELECT user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,
        amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,NULL::BIGINT AS pricing_epoch,pricing_snapshot,is_stream,node,pool,source_window
        FROM billing_record_receipts WHERE request_id=$1 AND status=20 FOR UPDATE",
        request_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(archived.map(|rec| (rec, true)))
}
