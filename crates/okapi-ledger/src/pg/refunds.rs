use super::{LedgerError, Money, PgPool, Pool, SETTLEMENT_LOCK_NS, Uuid};

/// 管理员按日志退款的结果。
#[derive(Debug, Clone)]
pub struct AdminRefund {
    pub user_id: i64,
    pub amount: Money,
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

    // CH 负额冲销行（log_type=6 退款，对齐 new-api；token 事实保留不冲）
    sqlx::query!(
        r#"INSERT INTO billing_outbox (topic, payload) VALUES ('billing.refunded', $1)"#,
        serde_json::json!({
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
            "amount_micro": -rec.amount_micro,
            "original_amount_micro": -rec.original_amount_micro,
            "discount_micro": -rec.discount_micro,
            "upstream_cost_micro": -rec.upstream_cost_micro.unwrap_or(0),
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
        })
    )
    .execute(&mut **tx)
    .await?;

    Ok(Some(AdminRefund {
        user_id: rec.user_id,
        amount: Money::from_micros(rec.amount_micro),
        pool,
    }))
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
    is_stream: bool,
    node: Option<String>,
    pool: i16,
}
async fn refundable(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request_id: Uuid,
) -> Result<Option<(RefundRecord, bool)>, LedgerError> {
    let live = sqlx::query_as!(
        RefundRecord,
        "SELECT user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,
        amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,node,pool
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
        amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,node,pool
        FROM billing_record_receipts WHERE request_id=$1 AND status=20 FOR UPDATE",
        request_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(archived.map(|rec| (rec, true)))
}
