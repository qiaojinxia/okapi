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

/// 管理操作审计留痕。
pub async fn record_audit(
    pool: &PgPool,
    actor: &str,
    action: &str,
    target: &str,
    detail: serde_json::Value,
) -> Result<(), StoreError> {
    record_audit_in(&mut *pool.acquire().await?, actor, action, target, detail).await
}

pub async fn record_audit_in(
    connection: &mut sqlx::PgConnection,
    actor: &str,
    action: &str,
    target: &str,
    detail: serde_json::Value,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"INSERT INTO audit_logs (actor, action, target, detail) VALUES ($1, $2, $3, $4)"#,
        actor,
        action,
        target,
        detail
    )
    .execute(connection)
    .await?;
    Ok(())
}
