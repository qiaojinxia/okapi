//! PG 记账：billing_records + billing_events + 余额快照 + outbox，单事务。

use crate::error::LedgerError;
use okapi_domain::{BillingState, Money, TokenUsage};
use sqlx::PgPool;
use uuid::Uuid;

/// 一笔请求的结算输入（终态写入）。
#[derive(Debug, Clone)]
pub struct SettlementInput<'a> {
    pub request_id: Uuid,
    /// 1充值 2消费 3管理 4系统 5错误 6退款 7登录。
    pub log_type: i16,
    pub user_id: i64,
    pub api_key_id: i64,
    pub group_code: &'a str,
    pub model_name: &'a str,
    pub channel_id: Option<i64>,
    pub channel_key_id: Option<i64>,
    pub state: BillingState,
    pub usage: TokenUsage,
    pub amount: Money,
    pub original: Money,
    pub discount: Money,
    pub pricing_epoch: Option<i64>,
    pub pricing_snapshot: Option<serde_json::Value>,
    pub latency_ms: i32,
    pub ttft_ms: Option<i32>,
    pub is_stream: bool,
    pub retry_count: i16,
    pub failover_count: i16,
    pub upstream_status: Option<i16>,
    pub error_code: Option<&'a str>,
    pub upstream_request_id: Option<&'a str>,
    /// 处理节点（gateway 实例名）。
    pub node: &'a str,
    /// 粘性命中层：0 无 / 1 response_id / 2 session / 3 打分（docs/database.md §1.5）。
    pub sticky_layer: i16,
    /// UA 识别的客户端类型（#5277）。
    pub client_type: &'a str,
    /// 客户端 IP（CDN 头按序解析，§14.2；统计列）。
    pub client_ip: Option<&'a str>,
    /// 余额净变动（消费 = −amount；退款/失败 = 0），billing_events 锚点。
    pub delta_micro: i64,
    pub balance_after: Option<Money>,
    /// commit | refund。
    pub event_type: &'a str,
}

fn token_i32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// 单事务落账（IMPLEMENTATION §2.2 步骤 13：记录 + 事件 + 快照列 + outbox）。
// 五条 SQL 的直线事务，拆分会破坏事务边界的可读性
#[allow(clippy::too_many_lines)]
pub async fn record_settlement(
    pool: &PgPool,
    input: SettlementInput<'_>,
) -> Result<(), LedgerError> {
    let mut tx = pool.begin().await?;

    sqlx::query!(
        r#"
        INSERT INTO billing_records (
            request_id, upstream_request_id, log_type, user_id, api_key_id,
            group_code, model_name, channel_id, channel_key_id, status,
            prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
            amount_micro, original_amount_micro, discount_micro,
            pricing_epoch, pricing_snapshot,
            latency_ms, ttft_ms, is_stream, retry_count, failover_count,
            upstream_status, error_code, node, sticky_layer, client_type
        ) VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10,
            $11, $12, $13, $14,
            $15, $16, $17,
            $18, $19,
            $20, $21, $22, $23, $24,
            $25, $26, $27, $28, $29
        )
        "#,
        input.request_id,
        input.upstream_request_id,
        input.log_type,
        input.user_id,
        input.api_key_id,
        input.group_code,
        input.model_name,
        input.channel_id,
        input.channel_key_id,
        input.state.as_i16(),
        token_i32(input.usage.prompt_tokens),
        token_i32(input.usage.cached_tokens),
        token_i32(input.usage.completion_tokens),
        token_i32(input.usage.reasoning_tokens),
        input.amount.as_micros(),
        input.original.as_micros(),
        input.discount.as_micros(),
        input.pricing_epoch,
        input.pricing_snapshot,
        input.latency_ms,
        input.ttft_ms,
        input.is_stream,
        input.retry_count,
        input.failover_count,
        input.upstream_status,
        input.error_code,
        input.node,
        input.sticky_layer,
        input.client_type
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, balance_after_micro, payload, actor)
        VALUES ($1, $2, $3, $4, $5, $6, 'system:gateway')
        "#,
        input.user_id,
        input.request_id,
        input.event_type,
        input.delta_micro,
        input.balance_after.map(Money::as_micros),
        serde_json::json!({
            "model": input.model_name,
            "amount_micro": input.amount.as_micros(),
            "error_code": input.error_code,
        })
    )
    .execute(&mut *tx)
    .await?;

    // 余额快照列（展示用；真理源 = 事件流，M2 reconciler 校准）
    sqlx::query!(
        r#"UPDATE users SET balance_micro = balance_micro + $2, updated_at = now() WHERE id = $1"#,
        input.user_id,
        input.delta_micro
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"UPDATE api_keys SET used_micro = used_micro + $2, last_used_at = now() WHERE id = $1"#,
        input.api_key_id,
        input.amount.as_micros()
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"INSERT INTO billing_outbox (topic, payload) VALUES ('billing.completed', $1)"#,
        serde_json::json!({
            "request_id": input.request_id,
            "user_id": input.user_id,
            "api_key_id": input.api_key_id,
            "group": input.group_code,
            "model": input.model_name,
            "channel_id": input.channel_id,
            "channel_key_id": input.channel_key_id,
            "log_type": input.log_type,
            "status": input.state.as_i16(),
            "prompt_tokens": input.usage.prompt_tokens,
            "cached_tokens": input.usage.cached_tokens,
            "completion_tokens": input.usage.completion_tokens,
            "reasoning_tokens": input.usage.reasoning_tokens,
            "amount_micro": input.amount.as_micros(),
            "original_amount_micro": input.original.as_micros(),
            "discount_micro": input.discount.as_micros(),
            "pricing_epoch": input.pricing_epoch,
            "ratio_snapshot": input.pricing_snapshot.as_ref().map(std::string::ToString::to_string).unwrap_or_default(),
            "latency_ms": input.latency_ms,
            "ttft_ms": input.ttft_ms,
            "is_stream": input.is_stream,
            "retry_count": input.retry_count,
            "failover_count": input.failover_count,
            "error_code": input.error_code,
            "upstream_status": input.upstream_status,
            "upstream_request_id": input.upstream_request_id,
            "node": input.node,
            "sticky_layer": input.sticky_layer,
            "client_type": input.client_type,
        "client_ip": input.client_ip,
        })
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// 管理员按日志退款的结果。
#[derive(Debug, Clone)]
pub struct AdminRefund {
    pub user_id: i64,
    pub amount: Money,
}

/// 管理员按日志退款（IMPLEMENTATION §5.3，#1790-10）。
///
/// PG 单事务完成：状态翻转（committed→refunded，幂等闸）、refund 事件、快照列回补、
/// key 用量回冲、outbox 负额冲销行（chsink 消费后 CH/MV 口径自动一致）。
/// Redis 热余额回补由调用方在事务成功后执行（崩溃窗口由对账检出修复）。
pub async fn admin_refund(
    pool: &PgPool,
    request_id: Uuid,
    reason: &str,
    actor: &str,
) -> Result<Option<AdminRefund>, LedgerError> {
    let mut tx = pool.begin().await?;

    let Some(rec) = sqlx::query!(
        r#"
        SELECT user_id, api_key_id, group_code, model_name, channel_id, channel_key_id,
               amount_micro, original_amount_micro, discount_micro, is_stream, node
        FROM billing_records
        WHERE request_id = $1 AND status = 20
        FOR UPDATE
        "#,
        request_id
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        // 不存在或已退款/失败：幂等返回 None
        tx.rollback().await?;
        return Ok(None);
    };

    sqlx::query!(
        r#"UPDATE billing_records SET status = 30 WHERE request_id = $1 AND status = 20"#,
        request_id
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, payload, actor)
        VALUES ($1, $2, 'refund', $3, $4, $5)
        "#,
        rec.user_id,
        request_id,
        rec.amount_micro,
        serde_json::json!({ "reason": reason, "tags": ["admin_refund"] }),
        actor
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        r#"UPDATE users SET balance_micro = balance_micro + $2, updated_at = now() WHERE id = $1"#,
        rec.user_id,
        rec.amount_micro
    )
    .execute(&mut *tx)
    .await?;

    if let Some(key_id) = rec.api_key_id {
        sqlx::query!(
            r#"UPDATE api_keys SET used_micro = used_micro - $2 WHERE id = $1"#,
            key_id,
            rec.amount_micro
        )
        .execute(&mut *tx)
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
            "is_stream": rec.is_stream,
            "retry_count": 0,
            "failover_count": 0,
            "error_code": null,
            "upstream_status": null,
            "upstream_request_id": null,
            "node": rec.node,
            "sticky_layer": 0,
            "client_type": "",
        })
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Some(AdminRefund {
        user_id: rec.user_id,
        amount: Money::from_micros(rec.amount_micro),
    }))
}

/// 充值/调整入账（PG 侧：事件 + 快照列；Redis 侧由调用方走 BalanceLedger::credit）。
pub async fn record_credit(
    pool: &PgPool,
    user_id: i64,
    amount: Money,
    event_type: &str,
    actor: &str,
    payload: serde_json::Value,
) -> Result<(), LedgerError> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        r#"
        INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, balance_after_micro, payload, actor)
        VALUES ($1, NULL, $2, $3, NULL, $4, $5)
        "#,
        user_id,
        event_type,
        amount.as_micros(),
        payload,
        actor
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"UPDATE users SET balance_micro = balance_micro + $2, updated_at = now() WHERE id = $1"#,
        user_id,
        amount.as_micros()
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
