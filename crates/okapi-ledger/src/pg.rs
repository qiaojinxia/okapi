//! PG 记账：billing_records + billing_events + 余额快照 + outbox，单事务。

use crate::error::LedgerError;
use crate::redis::Pool;
use okapi_domain::{BillingState, Money, TokenUsage};
use sqlx::PgPool;
use uuid::Uuid;

/// 统计维度独立于计费模型；空值表示未采集，禁止用计费名冒充请求或上游名。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UsageDimensions {
    pub requested_model: String,
    pub upstream_model: String,
    pub endpoint: String,
    pub upstream_endpoint: String,
    /// Bounded request diagnostics; absent on historical/replayed legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<serde_json::Value>,
}

impl UsageDimensions {
    pub fn new(requested: &str, upstream: &str, endpoint: &str, upstream_endpoint: &str) -> Self {
        Self {
            requested_model: requested.into(),
            upstream_model: upstream.into(),
            endpoint: endpoint.into(),
            upstream_endpoint: upstream_endpoint.into(),
            diagnostics: None,
        }
    }

    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Option<serde_json::Value>) -> Self {
        self.diagnostics = diagnostics;
        self
    }
}

/// 一笔请求的结算输入（终态写入）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SettlementInput<'a> {
    pub dimensions: UsageDimensions,
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
    /// 官方价（乘分组倍率前；`Quote::list_price`）——上游成本 = 官方价 × 渠道相对成本系数。
    /// 失败 / 退款记录为零。
    pub list_price: Money,
    /// 上游成本（§11.18）：由 `settle_write` 按渠道 `relative_cost_milli` 折算后填入；
    /// None = 无渠道（未选路即失败）或无法折算，CH 侧记 0 且不计入毛利分母。
    pub upstream_cost: Option<Money>,
    pub pricing_epoch: Option<i64>,
    pub pricing_snapshot: Option<serde_json::Value>,
    pub latency_ms: i32,
    pub ttft_ms: Option<i32>,
    pub is_stream: bool,
    pub retry_count: i16,
    pub failover_count: i16,
    pub upstream_status: Option<i16>,
    #[serde(borrow)]
    pub error_code: Option<&'a str>,
    #[serde(borrow)]
    pub upstream_request_id: Option<&'a str>,
    /// 处理节点（gateway 实例名）。
    pub node: &'a str,
    /// 粘性命中层：0 无 / 1 response_id / 2 session / 3 打分（docs/database.md §1.5）。
    pub sticky_layer: i16,
    /// UA 识别的客户端类型（#5277）。
    pub client_type: &'a str,
    /// 客户端 IP（CDN 头按序解析，§14.2；统计列）。
    /// 来源 IP（§14.2 信任闸判定后的值）。**PG 与 CH 两处都要落**：此前只进了 outbox
    /// 载荷（→ CH），`billing_records.client_ip` 这一列建了却从没写过，永远是 NULL——
    /// 没接 ClickHouse 的部署因此完全查不到来源 IP，而 docs/database.md 写的是「PG + CH」。
    #[serde(borrow)]
    pub client_ip: Option<&'a str>,
    /// 余额净变动（消费 = −amount；退款/失败 = 0），billing_events 锚点。
    pub delta_micro: i64,
    pub balance_after: Option<Money>,
    /// commit | refund。
    pub event_type: &'a str,
    /// 这笔由哪个池付（IMPLEMENTATION §11.28）：records / events / outbox 三处同写；
    /// `users.balance_micro` 快照只随钱包池动。
    pub pool: Pool,
    /// Immutable subscription window selected at admission; None for wallet/legacy.
    pub source_window: Option<String>,
}

/// Owned journal payload; quoted/escaped metadata must survive JSON replay.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnedSettlementInput {
    pub dimensions: UsageDimensions,
    pub request_id: Uuid,
    /// 1充值 2消费 3管理 4系统 5错误 6退款 7登录。
    pub log_type: i16,
    pub user_id: i64,
    pub api_key_id: i64,
    pub group_code: String,
    pub model_name: String,
    pub channel_id: Option<i64>,
    pub channel_key_id: Option<i64>,
    pub state: BillingState,
    pub usage: TokenUsage,
    pub amount: Money,
    pub original: Money,
    pub discount: Money,
    /// 官方价（乘分组倍率前；`Quote::list_price`）——上游成本 = 官方价 × 渠道相对成本系数。
    /// 失败 / 退款记录为零。
    pub list_price: Money,
    /// 上游成本（§11.18）：由 `settle_write` 按渠道 `relative_cost_milli` 折算后填入；
    /// None = 无渠道（未选路即失败）或无法折算，CH 侧记 0 且不计入毛利分母。
    pub upstream_cost: Option<Money>,
    pub pricing_epoch: Option<i64>,
    pub pricing_snapshot: Option<serde_json::Value>,
    pub latency_ms: i32,
    pub ttft_ms: Option<i32>,
    pub is_stream: bool,
    pub retry_count: i16,
    pub failover_count: i16,
    pub upstream_status: Option<i16>,
    pub error_code: Option<String>,
    pub upstream_request_id: Option<String>,
    /// 处理节点（gateway 实例名）。
    pub node: String,
    /// 粘性命中层：0 无 / 1 response_id / 2 session / 3 打分（docs/database.md §1.5）。
    pub sticky_layer: i16,
    /// UA 识别的客户端类型（#5277）。
    pub client_type: String,
    /// 客户端 IP（CDN 头按序解析，§14.2；统计列）。
    /// 来源 IP（§14.2 信任闸判定后的值）。**PG 与 CH 两处都要落**：此前只进了 outbox
    /// 载荷（→ CH），`billing_records.client_ip` 这一列建了却从没写过，永远是 NULL——
    /// 没接 ClickHouse 的部署因此完全查不到来源 IP，而 docs/database.md 写的是「PG + CH」。
    pub client_ip: Option<String>,
    /// 余额净变动（消费 = −amount；退款/失败 = 0），billing_events 锚点。
    pub delta_micro: i64,
    pub balance_after: Option<Money>,
    /// commit | refund。
    pub event_type: String,
    /// 这笔由哪个池付（IMPLEMENTATION §11.28）：records / events / outbox 三处同写；
    /// `users.balance_micro` 快照只随钱包池动。
    pub pool: Pool,
    /// Immutable subscription window selected at admission; None for wallet/legacy.
    pub source_window: Option<String>,
}

impl From<SettlementInput<'_>> for OwnedSettlementInput {
    fn from(input: SettlementInput<'_>) -> Self {
        Self {
            dimensions: input.dimensions,
            request_id: input.request_id,
            log_type: input.log_type,
            user_id: input.user_id,
            api_key_id: input.api_key_id,
            group_code: input.group_code.to_owned(),
            model_name: input.model_name.to_owned(),
            channel_id: input.channel_id,
            channel_key_id: input.channel_key_id,
            state: input.state,
            usage: input.usage,
            amount: input.amount,
            original: input.original,
            discount: input.discount,
            list_price: input.list_price,
            upstream_cost: input.upstream_cost,
            pricing_epoch: input.pricing_epoch,
            pricing_snapshot: input.pricing_snapshot,
            latency_ms: input.latency_ms,
            ttft_ms: input.ttft_ms,
            is_stream: input.is_stream,
            retry_count: input.retry_count,
            failover_count: input.failover_count,
            upstream_status: input.upstream_status,
            error_code: input.error_code.map(str::to_owned),
            upstream_request_id: input.upstream_request_id.map(str::to_owned),
            node: input.node.to_owned(),
            sticky_layer: input.sticky_layer,
            client_type: input.client_type.to_owned(),
            client_ip: input.client_ip.map(str::to_owned),
            delta_micro: input.delta_micro,
            balance_after: input.balance_after,
            event_type: input.event_type.to_owned(),
            pool: input.pool,
            source_window: input.source_window,
        }
    }
}

impl OwnedSettlementInput {
    pub fn as_input(&self) -> SettlementInput<'_> {
        SettlementInput {
            dimensions: self.dimensions.clone(),
            request_id: self.request_id,
            log_type: self.log_type,
            user_id: self.user_id,
            api_key_id: self.api_key_id,
            group_code: self.group_code.as_str(),
            model_name: self.model_name.as_str(),
            channel_id: self.channel_id,
            channel_key_id: self.channel_key_id,
            state: self.state,
            usage: self.usage,
            amount: self.amount,
            original: self.original,
            discount: self.discount,
            list_price: self.list_price,
            upstream_cost: self.upstream_cost,
            pricing_epoch: self.pricing_epoch,
            pricing_snapshot: self.pricing_snapshot.clone(),
            latency_ms: self.latency_ms,
            ttft_ms: self.ttft_ms,
            is_stream: self.is_stream,
            retry_count: self.retry_count,
            failover_count: self.failover_count,
            upstream_status: self.upstream_status,
            error_code: self.error_code.as_deref(),
            upstream_request_id: self.upstream_request_id.as_deref(),
            node: self.node.as_str(),
            sticky_layer: self.sticky_layer,
            client_type: self.client_type.as_str(),
            client_ip: self.client_ip.as_deref(),
            delta_micro: self.delta_micro,
            balance_after: self.balance_after,
            event_type: self.event_type.as_str(),
            pool: self.pool,
            source_window: self.source_window.clone(),
        }
    }
}

/// `record_settlement` 的 advisory lock 命名空间（"SETL"）。
const SETTLEMENT_LOCK_NS: i32 = 0x5345_544C;

fn token_i32(v: u32) -> Result<i32, LedgerError> {
    // Never persist a clamped count while outbox and usage_details keep the original.
    i32::try_from(v).map_err(|_| LedgerError::InvalidSettlement)
}

/// 单事务落账（IMPLEMENTATION §2.2 步骤 13：记录 + 事件 + 快照列 + outbox）。
///
/// 幂等：同一 request_id 已有记录即整笔跳过（docs/database.md §1.5）。分区表给不了
/// request_id 唯一约束，而调用方 `settle_write` 会在失败后重试——COMMIT 已成功但回包
/// 丢失的那一次重试若真写进去，事件流就多一笔 −amount，对账修复还会照着它把 Redis 也
/// 改成双扣。
///
/// 光有 EXISTS 不够：READ COMMITTED 下两个并发事务会同时看到"不存在"、各写一遍。
/// 所以先拿以 request_id 为键的事务级 advisory lock，同一笔的并发结算在 PG 内串行，
/// 后到的一方必然看见前者已提交的记录。此前 PG 自己不设防，只靠"Redis commit 闸只放行
/// 一次"——而超时重试可能与服务端仍在执行的上一次并发，溢出重放（`settle_write`）也会与
/// 网关自己的重试并发。
// 五条 SQL 的直线事务，拆分会破坏事务边界的可读性
#[allow(clippy::too_many_lines)]
pub async fn record_settlement(
    pool: &PgPool,
    input: SettlementInput<'_>,
) -> Result<(), LedgerError> {
    let mut tx = pool.begin().await?;
    record_settlement_in_tx(&mut tx, input).await?;
    tx.commit().await?;
    Ok(())
}

/// Allows a durable task result and its ledger entry to share one commit.
/// The caller must commit/rollback; false means the request was already recorded.
#[allow(clippy::too_many_lines)]
pub async fn record_settlement_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: SettlementInput<'_>,
) -> Result<bool, LedgerError> {
    let (input_unit, input_characters) = input_units(input.usage, input.pricing_snapshot.as_ref())?;
    // 两参形式与单参 bigint 形式的锁互不相撞（sqlx 迁移锁用的是单参）；
    // 不同 request_id 撞上同一个 hashtext 只是短暂串行，不影响正确性
    okapi_store::history::read_lock(tx).await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(SETTLEMENT_LOCK_NS)
        .bind(input.request_id.to_string())
        .execute(&mut **tx)
        .await?;

    let already_recorded = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM billing_financial_records WHERE request_id = $1) AS "exists!""#,
        input.request_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if already_recorded {
        tracing::warn!(request_id = %input.request_id, "结算重放：request_id 已落账，跳过");
        return Ok(false);
    }

    sqlx::query(
        r"
        INSERT INTO billing_records (
            request_id, upstream_request_id, log_type, user_id, api_key_id,
            group_code, model_name, channel_id, channel_key_id, status,
            prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
            amount_micro, original_amount_micro, discount_micro,
            pricing_epoch, pricing_snapshot,
            latency_ms, ttft_ms, is_stream, retry_count, failover_count,
            upstream_status, error_code, node, sticky_layer, client_type,
            upstream_cost_micro, client_ip, pool, usage_details, source_window
        ) VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10,
            $11, $12, $13, $14,
            $15, $16, $17,
            $18, $19,
            $20, $21, $22, $23, $24,
            $25, $26, $27, $28, $29,
            -- client_ip 是 INET。写成 $31::text::inet 而非 $31::inet：后者会让 PG 把入参
            -- 类型报成 inet，sqlx 就要求打开 ipnetwork feature——为一列拉一整套网络类型
            -- 不值当。先按 text 绑定再转，值只可能来自 `clients::client_ip`（已 parse 过）。
            $30, $31::text::inet, $32, $33, $34
        )
        ",
    )
    .bind(input.request_id)
    .bind(input.upstream_request_id)
    .bind(input.log_type)
    .bind(input.user_id)
    .bind(input.api_key_id)
    .bind(input.group_code)
    .bind(input.model_name)
    .bind(input.channel_id)
    .bind(input.channel_key_id)
    .bind(input.state.as_i16())
    .bind(token_i32(input.usage.prompt_tokens)?)
    .bind(token_i32(input.usage.cached_tokens)?)
    .bind(token_i32(input.usage.completion_tokens)?)
    .bind(token_i32(input.usage.reasoning_tokens)?)
    .bind(input.amount.as_micros())
    .bind(input.original.as_micros())
    .bind(input.discount.as_micros())
    .bind(input.pricing_epoch)
    .bind(&input.pricing_snapshot)
    .bind(input.latency_ms)
    .bind(input.ttft_ms)
    .bind(input.is_stream)
    .bind(input.retry_count)
    .bind(input.failover_count)
    .bind(input.upstream_status)
    .bind(input.error_code)
    .bind(input.node)
    .bind(input.sticky_layer)
    .bind(input.client_type)
    .bind(input.upstream_cost.map(Money::as_micros))
    .bind(input.client_ip)
    .bind(input.pool.as_i16())
    .bind(serde_json::json!({
        "tokens": input.usage,
        "input_unit": input_unit,
        "input_characters": input_characters,
        "prompt_source": input.usage.prompt_source(),
        "completion_source": input.usage.completion_source(),
        "requested_model": input.dimensions.requested_model,
        "endpoint": input.dimensions.endpoint,
        "diagnostics": input.dimensions.diagnostics,
    }))
    .bind(&input.source_window)
    .execute(&mut **tx)
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, balance_after_micro, payload, actor, pool)
        VALUES ($1, $2, $3, $4, $5, $6, 'system:gateway', $7)
        "#,
        input.user_id,
        input.request_id,
        input.event_type,
        input.delta_micro,
        input.balance_after.map(Money::as_micros),
        serde_json::json!({
            "model": input.model_name,
            "requested_model": input.dimensions.requested_model,
            "upstream_model": input.dimensions.upstream_model,
            "endpoint": input.dimensions.endpoint,
            "upstream_endpoint": input.dimensions.upstream_endpoint,
            "billing_type": input.pricing_snapshot.as_ref().and_then(|s| s.get("mode")).and_then(serde_json::Value::as_str),
            "upstream_cost_known": input.upstream_cost.is_some(),
            "amount_micro": input.amount.as_micros(),
            "source_window": input.source_window,
            "error_code": input.error_code,
        }),
        input.pool.as_i16()
    )
    .execute(&mut **tx)
    .await?;

    // 钱包余额快照列（展示用；真理源 = 事件流，M2 reconciler 校准）。订阅池不落 PG 快照。
    // 两条快照 UPDATE 都必须命中一行：应用不硬删用户与 key，命中 0 行只能是库被绕过应用改过。
    // 整笔回滚并报 InvalidSettlement（重试日志按永久失败隔离待修），不留下事件与快照对不上的账。
    if input.pool == Pool::Wallet {
        let updated = sqlx::query!(
            r#"UPDATE users SET balance_micro = balance_micro + $2, updated_at = now() WHERE id = $1"#,
            input.user_id,
            input.delta_micro
        )
        .execute(&mut **tx)
        .await?;
        if updated.rows_affected() != 1 {
            tracing::error!(user_id = input.user_id, request_id = %input.request_id, "settlement user row missing");
            return Err(LedgerError::InvalidSettlement);
        }
    }

    let updated = sqlx::query!(
        r#"UPDATE api_keys SET used_micro = used_micro + $2, last_used_at = now() WHERE id = $1"#,
        input.api_key_id,
        input.amount.as_micros()
    )
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        tracing::error!(api_key_id = input.api_key_id, request_id = %input.request_id, "settlement api key row missing");
        return Err(LedgerError::InvalidSettlement);
    }

    sqlx::query!(
        r#"INSERT INTO billing_outbox (topic, payload) VALUES ('billing.completed', $1)"#,
        with_request_diagnostics(with_usage_source(serde_json::json!({
            "request_id": input.request_id,
            "user_id": input.user_id,
            "api_key_id": input.api_key_id,
            "group": input.group_code,
            "model": input.model_name,
            "requested_model": input.dimensions.requested_model,
            "upstream_model": input.dimensions.upstream_model,
            "endpoint": input.dimensions.endpoint,
            "upstream_endpoint": input.dimensions.upstream_endpoint,
            "billing_type": input.pricing_snapshot.as_ref().and_then(|s| s.get("mode")).and_then(serde_json::Value::as_str),
            "upstream_cost_known": input.upstream_cost.is_some(),
            "channel_id": input.channel_id,
            "channel_key_id": input.channel_key_id,
            "log_type": input.log_type,
            "status": input.state.as_i16(),
            "prompt_tokens": input.usage.prompt_tokens,
            "cached_tokens": input.usage.cached_tokens,
            "cache_write_tokens": input.usage.cache_write_tokens,
            "cache_read_reported": input.usage.cache_read_reported,
            "cache_write_reported": input.usage.cache_write_reported,
            "completion_tokens": input.usage.completion_tokens,
            "reasoning_tokens": input.usage.reasoning_tokens,
            "amount_micro": input.amount.as_micros(),
            "original_amount_micro": input.original.as_micros(),
            "discount_micro": input.discount.as_micros(),
            "upstream_cost_micro": input.upstream_cost.map_or(0, Money::as_micros),
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
            "pool": input.pool.as_i16(),
        }), input.usage, input_unit, input_characters), input.dimensions.diagnostics.as_ref())
    )
    .execute(&mut **tx)
    .await?;

    Ok(true)
}

mod refunds;
pub use refunds::{AdminRefund, admin_refund, admin_refund_in_tx};

/// 订阅池事件（`sub_grant` / `sub_reset` / `sub_expire`，pool=1）：只记事件，**不动**
/// `users.balance_micro`（那是钱包快照）。`delta` = `sub_set` 前后差；Redis 侧由调用方先做。
pub async fn record_sub_event<'e, E>(
    executor: E,
    user_id: i64,
    delta: Money,
    balance_after: Money,
    event_type: &str,
    actor: &str,
    payload: serde_json::Value,
) -> Result<(), LedgerError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, balance_after_micro, payload, actor, pool)
        VALUES ($1, NULL, $2, $3, $4, $5, $6, 1)
        "#,
        user_id,
        event_type,
        delta.as_micros(),
        balance_after.as_micros(),
        payload,
        actor
    )
    .execute(executor)
    .await?;
    Ok(())
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
    record_credit_in_tx(&mut tx, user_id, amount, event_type, actor, payload).await?;
    tx.commit().await?;
    Ok(())
}

/// Event and snapshot participate in the caller's transaction on its locked connection.
pub async fn record_credit_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: i64,
    amount: Money,
    event_type: &str,
    actor: &str,
    payload: serde_json::Value,
) -> Result<(), LedgerError> {
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
    .execute(&mut **tx)
    .await?;
    let updated = sqlx::query!(
        r#"UPDATE users SET balance_micro = balance_micro + $2, updated_at = now() WHERE id = $1"#,
        user_id,
        amount.as_micros()
    )
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(LedgerError::UserNotFound);
    }
    Ok(())
}

fn input_units(
    usage: TokenUsage,
    snapshot: Option<&serde_json::Value>,
) -> Result<(&'static str, Option<u32>), LedgerError> {
    let unit = snapshot.and_then(|s| s.get("input_unit"));
    let characters = snapshot.and_then(|s| s.get("input_characters"));
    match unit.and_then(serde_json::Value::as_str) {
        Some("characters") => {
            let count = characters
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(LedgerError::InvalidSettlement)?;
            if usage != TokenUsage::default() {
                return Err(LedgerError::InvalidSettlement);
            }
            Ok(("characters", Some(count)))
        }
        None | Some("tokens")
            if unit.is_none_or(|v| v.is_null() || v.is_string())
                && characters.is_none_or(serde_json::Value::is_null) =>
        {
            // Empty usage on a per-call or media response is not evidence of a Token unit.
            // Explicit Token metadata or any observed Token axis retains the unit.
            let known = unit.and_then(serde_json::Value::as_str) == Some("tokens")
                || usage.upstream_usage.is_some_and(|u| {
                    u.prompt_tokens.is_some()
                        || u.completion_tokens.is_some()
                        || usage.total_raw() > 0
                })
                || usage.cache_read_reported
                || usage.cache_write_reported
                || usage.cache_write_5m_tokens.is_some()
                || usage.cache_write_1h_tokens.is_some()
                || usage
                    .reported_details
                    .is_some_and(|d| d != okapi_domain::TokenDetailsReported::default());
            Ok((if known { "tokens" } else { "" }, None))
        }
        _ => Err(LedgerError::InvalidSettlement),
    }
}

fn with_request_diagnostics(
    mut payload: serde_json::Value,
    diagnostics: Option<&serde_json::Value>,
) -> serde_json::Value {
    if let Some(diagnostics) = diagnostics {
        payload["diagnostics"] = diagnostics.clone();
    }
    payload
}

fn with_usage_source(
    mut payload: serde_json::Value,
    usage: TokenUsage,
    input_unit: &str,
    input_characters: Option<u32>,
) -> serde_json::Value {
    payload["input_unit"] = input_unit.into();
    payload["input_characters"] = serde_json::json!(input_characters);
    payload["reported_details"] = serde_json::json!(usage.reported_details);
    payload["server_tool_usage"] = serde_json::json!(usage.server_tool_usage);
    payload["cache_read_modalities"] = serde_json::json!(usage.cache_read_modalities);
    payload["cache_write_modalities"] = serde_json::json!(usage.cache_write_modalities);
    payload["audio_prompt_tokens"] = usage.audio_prompt_tokens.into();
    payload["image_prompt_tokens"] = usage.image_prompt_tokens.into();
    payload["audio_completion_tokens"] = usage.audio_completion_tokens.into();
    payload["image_completion_tokens"] = usage.image_completion_tokens.into();
    payload["cache_write_5m_tokens"] = serde_json::json!(usage.cache_write_5m_tokens);
    payload["cache_write_1h_tokens"] = serde_json::json!(usage.cache_write_1h_tokens);
    payload["upstream_usage"] = serde_json::json!(usage.upstream_usage);
    payload["prompt_source"] = usage.prompt_source().into();
    payload["completion_source"] = usage.completion_source().into();
    payload
}

#[cfg(test)]
mod unit_tests {
    use super::input_units;
    use okapi_domain::{TokenUsage, UpstreamTokenCounts};
    use serde_json::json;

    #[test]
    fn legacy_dimensions_replay_without_diagnostics_and_new_metadata_roundtrips() {
        let legacy = json!({"requested_model":"alias","upstream_model":"provider-model","endpoint":"/v1/chat/completions","upstream_endpoint":"/v1/chat/completions"});
        let dimensions: super::UsageDimensions = serde_json::from_value(legacy.clone()).unwrap();
        assert!(dimensions.diagnostics.is_none());
        assert_eq!(serde_json::to_value(&dimensions).unwrap(), legacy);
        let metadata = json!({"error_message":"quoted \"value\"","request_failed":true});
        let dimensions = dimensions.with_diagnostics(Some(metadata.clone()));
        let replayed: super::UsageDimensions =
            serde_json::from_value(serde_json::to_value(dimensions).unwrap()).unwrap();
        assert_eq!(replayed.diagnostics, Some(metadata));
        assert_eq!(replayed.requested_model, "alias");
    }

    #[test]
    fn character_units_keep_known_zero_and_u32_boundary() {
        for count in [0, 11, u32::MAX] {
            let snapshot = json!({"input_unit":"characters", "input_characters":count});
            assert_eq!(
                input_units(TokenUsage::default(), Some(&snapshot)).unwrap(),
                ("characters", Some(count))
            );
        }
        assert_eq!(
            input_units(TokenUsage::default(), None).unwrap(),
            ("", None)
        );
        let zero = TokenUsage {
            upstream_usage: Some(UpstreamTokenCounts {
                prompt_tokens: Some(0),
                completion_tokens: Some(0),
            }),
            ..Default::default()
        };
        assert_eq!(input_units(zero, None).unwrap(), ("tokens", None));
        let missing = TokenUsage {
            upstream_usage: Some(UpstreamTokenCounts::default()),
            ..Default::default()
        };
        assert_eq!(input_units(missing, None).unwrap(), ("", None));
        let legacy = TokenUsage {
            prompt_tokens: 11,
            ..Default::default()
        };
        assert_eq!(
            input_units(legacy, None).unwrap(),
            ("", None),
            "a legacy count may contain characters; quantity alone proves no unit"
        );
        let estimated = TokenUsage {
            prompt_tokens: 11,
            upstream_usage: Some(UpstreamTokenCounts::default()),
            ..Default::default()
        };
        assert_eq!(input_units(estimated, None).unwrap(), ("tokens", None));
        assert_eq!(
            input_units(TokenUsage::default(), Some(&json!({"input_unit":"tokens"}))).unwrap(),
            ("tokens", None)
        );
    }

    #[test]
    fn inconsistent_or_invalid_character_settlements_are_rejected() {
        for snapshot in [
            json!({"input_unit":"characters"}),
            json!({"input_unit":"characters", "input_characters":null}),
            json!({"input_unit":"characters", "input_characters":-1}),
            json!({"input_unit":"characters", "input_characters":"11"}),
            json!({"input_unit":"characters", "input_characters":true}),
            json!({"input_unit":"characters", "input_characters":u64::from(u32::MAX)+1}),
            json!({"input_unit":"tokens", "input_characters":11}),
            json!({"input_characters":11}),
            json!({"input_unit":"unknown"}),
            json!({"input_unit":false}),
        ] {
            assert!(
                input_units(TokenUsage::default(), Some(&snapshot)).is_err(),
                "{snapshot}"
            );
        }
        let snapshot = json!({"input_unit":"characters", "input_characters":11});
        for usage in [
            TokenUsage {
                prompt_tokens: 11,
                ..Default::default()
            },
            TokenUsage {
                completion_tokens: 1,
                ..Default::default()
            },
            TokenUsage {
                audio_prompt_tokens: 1,
                ..Default::default()
            },
        ] {
            assert!(input_units(usage, Some(&snapshot)).is_err());
        }
    }
}
