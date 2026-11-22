//! 管理面写路径（console 角色用）：模型/定价 upsert、渠道管理、epoch 发布、审计。
//!
//! 本文件保留跨资源的全局设置与审计留痕；按领域拆出的子模块在此原路径再导出，
//! 保持 `okapi_store::admin::*` 调用面不变。

use crate::error::StoreError;
use sqlx::PgPool;

mod billing;
mod channels;
mod pricing;
mod users;

pub use self::billing::{
    ClaimedRedemption, NewOrder, PaidOrder, PlanSpec, RedemptionOptions, RedemptionPrecheck,
    claim_redemption_in_tx, create_plan, create_recharge_order, create_redemption_codes,
    redemption_precheck,
};
pub use self::channels::{
    CREDENTIAL_KIND_OAUTH, ChannelFilter, ChannelKeyPatch, ChannelKeyRow, ChannelList,
    ChannelPatch, ChannelRow, DeleteChannelKeyOutcome, NewChannelKey, PoolMember, RotateOutcome,
    add_channel_key, channel_cost_milli, channel_owner, delete_channel_key, list_channel_keys,
    list_channel_keys_for, list_channels, patch_channel, patch_channel_key, read_key_credential,
    rotate_channel_credential, set_channel_owner, set_channel_pinned, set_channel_pool_codes,
    set_channel_pools, set_channel_status, upsert_channel_pool, write_key_credential,
};
pub use self::pricing::{
    PriceGroupInput, PricingRuleInput, PricingRuleRow, RatioAxes, delete_pricing_rule,
    list_pricing_rules, price_group_exists, publish_epoch, upsert_model_per_call,
    upsert_model_per_call_in, upsert_model_ratio, upsert_model_ratio_in, upsert_model_tiered,
    upsert_price_group, upsert_pricing_rule,
};
pub use self::users::{
    ApiKeyPatch, SelectableGroupRow, TouchedApiKey, add_user_group, assign_user_role,
    create_admin_role, patch_api_key, selectable_groups, set_user_groups, set_user_multiplier,
    soft_delete_api_key, soft_delete_session_keys,
};

/// 全局设置写入（settings KV）。
pub async fn set_setting(
    pool: &PgPool,
    key: &str,
    value: &serde_json::Value,
    updated_by: i64,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"
        INSERT INTO settings (key, value, updated_by)
        VALUES ($1, $2, $3)
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
            updated_by = EXCLUDED.updated_by, updated_at = now()
        "#,
        key,
        value,
        updated_by
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// `audit_logs` 各文本列最大字符数（对齐 PostgreSQL `varchar(N)`，防 SQLSTATE 22001 静默丢审计）。
pub const AUDIT_ACTOR_MAX_CHARS: usize = 64;
pub const AUDIT_ACTION_MAX_CHARS: usize = 64;
pub const AUDIT_TARGET_MAX_CHARS: usize = 128;
pub const AUDIT_UA_MAX_CHARS: usize = 256;
pub const AUDIT_TRACE_MAX_CHARS: usize = 64;

/// 审计记录的请求上下文元数据（来源 IP、User-Agent、链路 trace_id、操作凭证 key_id）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditMeta {
    pub ip: Option<std::net::IpAddr>,
    pub user_agent: Option<String>,
    pub trace_id: Option<String>,
    pub actor_key_id: Option<i64>,
}

/// 按 Unicode 字符数截断字符串切片（零分配、保持合法 UTF-8 边界）。
#[must_use]
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// 管理操作审计留痕（无请求上下文的后台/兼容入口）。
pub async fn record_audit(
    pool: &PgPool,
    actor: &str,
    action: &str,
    target: &str,
    detail: serde_json::Value,
) -> Result<(), StoreError> {
    record_audit_with_meta(pool, actor, action, target, detail, &AuditMeta::default()).await
}

pub async fn record_audit_in(
    connection: &mut sqlx::PgConnection,
    actor: &str,
    action: &str,
    target: &str,
    detail: serde_json::Value,
) -> Result<(), StoreError> {
    record_audit_with_meta_in(
        connection,
        actor,
        action,
        target,
        detail,
        &AuditMeta::default(),
    )
    .await
}

/// 带完整请求上下文（IP / UA / trace_id / actor_key_id）写入审计日志。
pub async fn record_audit_with_meta(
    pool: &PgPool,
    actor: &str,
    action: &str,
    target: &str,
    detail: serde_json::Value,
    meta: &AuditMeta,
) -> Result<(), StoreError> {
    record_audit_with_meta_in(
        &mut *pool.acquire().await?,
        actor,
        action,
        target,
        detail,
        meta,
    )
    .await
}

/// 在既有事务/连接内写入带完整上下文的审计日志；自动对所有定长文本列做安全截断，
/// 避免超长目标/UA/动作名触发 PG 22001 导致审计丢失。
pub async fn record_audit_with_meta_in(
    connection: &mut sqlx::PgConnection,
    actor: &str,
    action: &str,
    target: &str,
    detail: serde_json::Value,
    meta: &AuditMeta,
) -> Result<(), StoreError> {
    let actor = truncate_chars(actor, AUDIT_ACTOR_MAX_CHARS);
    let action = truncate_chars(action, AUDIT_ACTION_MAX_CHARS);
    let target = truncate_chars(target, AUDIT_TARGET_MAX_CHARS);
    let ip_str = meta.ip.map(|ip| ip.to_string());
    let ua = meta
        .user_agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, AUDIT_UA_MAX_CHARS));
    let trace_id = meta
        .trace_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, AUDIT_TRACE_MAX_CHARS));
    sqlx::query(
        r"INSERT INTO audit_logs (actor, action, target, detail, ip, user_agent, trace_id, actor_key_id)
          VALUES ($1, $2, $3, $4, $5::inet, $6, $7, $8)",
    )
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(detail)
    .bind(ip_str)
    .bind(ua)
    .bind(trace_id)
    .bind(meta.actor_key_id)
    .execute(connection)
    .await?;
    Ok(())
}

#[cfg(test)]
mod audit_truncate_tests {
    use super::*;

    #[test]
    fn truncate_chars_respects_unicode_boundaries() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hello", 3), "hel");
        assert_eq!(truncate_chars("审计系统测试", 4), "审计系统");
        assert_eq!(truncate_chars("🚀✨🔥abc", 2), "🚀✨");
        let long_email = format!("{}@example.com", "a".repeat(200));
        assert_eq!(
            truncate_chars(&long_email, AUDIT_TARGET_MAX_CHARS)
                .chars()
                .count(),
            AUDIT_TARGET_MAX_CHARS
        );
    }
}
