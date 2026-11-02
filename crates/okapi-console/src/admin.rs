//! 管理 API 处理器（M2 第一批）。
//! 鉴权：Bearer api key → users.role >= 10；写操作全量 audit_logs 留痕。
//!
//! 本文件只放各处理器共用的守卫（权限点、own/all 范围、渠道属主、超管）与审计；处理器按资源
//! 分在子模块里，并从这里原路径再导出（`console/mod.rs` 的路由与其它模块的引用不变）。

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_gateway::state::AppState;
use okapi_kernel::auth::authenticate;
use okapi_kernel::error::AppError;
use okapi_store::AuthedKey;
use okapi_store::auth::PermScope;
use serde_json::Value;
use std::sync::Arc;

mod balance;
mod channel_probe;
mod channel_settings;
mod channels;
mod groups;
mod keys;
mod models;
mod ops;
mod plans;
mod pricing;
mod roles;
mod settings;
mod users;

use self::balance::RATIO_COLUMN_MAX_SCALED;
pub use self::balance::{
    BalanceExpiryReq, CreditReq, RefundReq, SetMultiplierReq, billing_record_lookup, credit_user,
    refund_by_request, set_balance_expiry, set_user_multiplier,
};
pub(super) use self::balance::{ensure_adjust_amount, ensure_redemption_amount, ratio_fits};
pub(crate) use self::channel_probe::probe_channel;
pub use self::channel_probe::{
    DraftModelsReq, TestChannelReq, fetch_channel_models, fetch_draft_models, test_channel,
};
pub(super) use self::channel_settings::{
    ensure_account_control, ensure_azure_api_base, validate_channel_settings,
};
use self::channel_settings::{
    ensure_request_extensions, ensure_settings_api_version, ensure_settings_mimic,
    ensure_settings_oauth_token_url, ensure_settings_outbound, ensure_settings_tool_loss_policy,
};
pub use self::channels::{
    ChannelUsageQuery, CreateChannelReq, PatchChannelKeyReq, PatchChannelReq, PoolMemberReq,
    RotateCredentialReq, SetPoolsReq, SetStatusReq, channel_providers, channel_usage,
    create_channel, delete_channel, list_channels, rotate_channel_credential, set_channel_pools,
    set_channel_status, update_channel, update_channel_key,
};
pub(super) use self::channels::{
    ensure_cost_milli, ensure_max_concurrency, ensure_pool_scope, normalize_members,
    normalize_models,
};
use self::groups::ensure_group_usable;
pub use self::groups::{
    UpsertGroupReq, UpsertPoolReq, UserGroupItem, UserGroupsReq, set_user_groups, upsert_group,
    upsert_pool,
};
pub use self::keys::{AdminPatchKeyReq, delete_api_key, patch_api_key};
pub use self::models::{UpsertModelReq, upsert_model};
pub use self::ops::{
    FlushReq, ReconQuery, RepairReq, cache_flush, reconciliation, repair_reconciliation,
};
pub use self::plans::{CreateRedemptionsReq, UpsertPlanReq, create_redemptions, upsert_plan};
pub(super) use self::pricing::publish_pricing_inner;
pub use self::pricing::{
    ImportNewApiReq, PublishPricingQuery, UpsertRuleReq, delete_pricing_rule,
    import_newapi_pricing, list_pricing_rules, publish_pricing, upsert_pricing_rule,
};
pub use self::roles::{AssignRoleReq, CreateRoleReq, assign_role, create_role, list_roles};
pub use self::settings::{SetSettingReq, SmtpTestReq, get_setting, set_setting, smtp_test};
pub use self::users::{
    LeaderboardQuery, UserUsageQuery, leaderboard, list_users, user_overview, user_usage,
};

/// 权限点守卫（IMPLEMENTATION §6.2）：403 时 param 携带缺失的权限点，便于排障。
pub(super) async fn guard(
    state: &AppState,
    headers: &HeaderMap,
    permission: &str,
) -> Result<Arc<AuthedKey>, AppError> {
    let key = authenticate(state, headers).await?;
    if !key.has_permission(permission) {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param(permission.to_owned()),
        );
    }
    super::audit_fallback::note_actor(key.actor_user_id(), permission);
    Ok(key)
}

/// `/admin/` 下全是要登录的接口：先鉴权再交给处理器。请求体 / 查询串提取器跑在处理器体之前，
/// 不先鉴权的话，匿名请求带坏请求体拿到的是 400 而不是 401。权限点仍由处理器里的 [`guard`] 查。
pub(super) async fn authenticate_first(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if request.uri().path().starts_with("/admin/")
        && let Err(error) = authenticate(&state, request.headers()).await
    {
        return error.into_response();
    }
    next.run(request).await
}

/// 带 own/all 范围的守卫（#6267）：Denied → 403；返回范围供 handler 做属主过滤。
pub(super) async fn guard_scoped(
    state: &AppState,
    headers: &HeaderMap,
    base: &str,
) -> Result<(Arc<AuthedKey>, PermScope), AppError> {
    let key = authenticate(state, headers).await?;
    match key.permission_scope(base) {
        PermScope::Denied => Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param(base.to_owned()),
        ),
        scope => {
            super::audit_fallback::note_actor(key.actor_user_id(), base);
            Ok((key, scope))
        }
    }
}

/// own 范围下的渠道属主校验。
pub(super) async fn ensure_channel_owner(
    state: &AppState,
    channel_id: i64,
    actor: &AuthedKey,
    scope: PermScope,
) -> Result<(), AppError> {
    if scope == PermScope::All {
        return Ok(());
    }
    let owner = okapi_store::admin::channel_owner(&state.pg, channel_id)
        .await?
        .ok_or_else(|| AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND))?;
    if owner != Some(actor.actor_user_id()) {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED).with_param("owner"),
        );
    }
    Ok(())
}

/// 角色管理强制 super_admin：防止"默认全权 admin"给自己发角色形成提权链。
pub(super) async fn guard_super_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<AuthedKey>, AppError> {
    let key = guard(state, headers, permissions::ROLE_MANAGE).await?;
    if key.role < 100 {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("super_admin_required"),
        );
    }
    Ok(key)
}

pub(super) async fn audit(
    state: &AppState,
    actor: &AuthedKey,
    action: &str,
    target: &str,
    detail: Value,
) {
    super::audit_fallback::note_audited();
    if let Err(err) = okapi_store::admin::record_audit(
        &state.pg,
        &format!("admin:{}", actor.actor_user_id()),
        action,
        target,
        detail,
    )
    .await
    {
        tracing::error!(error = %err, action, "审计写入失败");
    }
}
