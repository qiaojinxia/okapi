//! 管理面只读列表查询（IMPLEMENTATION §11.6 接口面清单）。
//!
//! 与 `admin` 模块分工：`admin` 放写操作与既有单点查询；本模块集中放**分页列表**，
//! 这些查询共性强（limit/offset + 总数 + 关键词 + 占用计数），集中便于统一护栏。
//!
//! 护栏（PG 只做点查与账本，看板聚合走 CH——见 00-project 红线）：
//! - 一律分页并由 `clamp_page` 钳制上限，防管理端误传超大 limit 拖垮 PG；
//! - 只回配置与账本事实，不做跨表聚合统计（统计一律走 `ch` 模块 + 物化视图）；
//! - 列表附带"占用计数"，供删除前的引用检查（见 `mutate` 的 Conflict 语义）。

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

/// 单页上限。
pub const MAX_PAGE: i64 = 200;

/// 钳制分页参数（limit ∈ [1, MAX_PAGE]，offset ≥ 0）。
#[must_use]
pub fn clamp_page(limit: i64, offset: i64) -> (i64, i64) {
    (limit.clamp(1, MAX_PAGE), offset.max(0))
}

/// 关键词 → ILIKE 模式；转义 `%` `_` 防调用方注入通配符。
fn like_pattern(query: Option<&str>) -> Option<String> {
    query
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", q.replace('%', "\\%").replace('_', "\\_")))
}

/// 分页信封（`total` 供前端翻页器）。
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    pub total: i64,
}

#[derive(Debug, Serialize)]
pub struct ModelListRow {
    pub model_name: String,
    pub display_name: Option<String>,
    pub vendor: Option<String>,
    pub status: i16,
    pub sort_order: i32,
    pub capabilities: serde_json::Value,
    pub context_window: Option<i32>,
    /// 无定价行时为 None——模型已建但未定价属配置错误，管理端应高亮。
    pub pricing_mode: Option<String>,
    pub model_ratio: Option<String>,
    pub completion_ratio: Option<String>,
    pub cache_ratio: Option<String>,
    pub cache_write_ratio: Option<String>,
    pub per_call_price_micro: Option<i64>,
    pub tier_expr: Option<String>,
    pub tier_ratios: Option<serde_json::Value>,
}

/// 模型配置列表（含定价四轴）。倍率以 `::text` 出库保精度，展示层不做浮点运算。
/// LEFT JOIN 以暴露"未定价模型"。
pub async fn list_models(pool: &PgPool) -> Result<Vec<ModelListRow>, StoreError> {
    let rows = sqlx::query!(
        r#"
        SELECT m.model_name, m.display_name, m.vendor, m.status, m.sort_order,
               m.capabilities, m.context_window,
               p.pricing_mode            AS "pricing_mode?",
               p.model_ratio::text       AS model_ratio,
               p.completion_ratio::text  AS completion_ratio,
               p.cache_ratio::text       AS cache_ratio,
               p.cache_write_ratio::text AS cache_write_ratio,
               p.per_call_price_micro, p.tier_expr, p.tier_ratios
        FROM models m
        LEFT JOIN model_pricing p ON p.model_id = m.id
        ORDER BY m.sort_order, m.model_name
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ModelListRow {
            model_name: r.model_name,
            display_name: r.display_name,
            vendor: r.vendor,
            status: r.status,
            sort_order: r.sort_order,
            capabilities: r.capabilities,
            context_window: r.context_window,
            pricing_mode: r.pricing_mode,
            model_ratio: r.model_ratio,
            completion_ratio: r.completion_ratio,
            cache_ratio: r.cache_ratio,
            cache_write_ratio: r.cache_write_ratio,
            per_call_price_micro: r.per_call_price_micro,
            tier_expr: r.tier_expr,
            tier_ratios: r.tier_ratios,
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct GroupListRow {
    pub group_code: String,
    pub group_ratio: Option<String>,
    pub description: Option<String>,
    pub is_default: bool,
    pub sort_order: i32,
    /// 绑定该分组的用户数（删除前占用检查）。
    pub user_count: i64,
    /// 绑定该分组可见的渠道数。
    pub channel_count: i64,
}

pub async fn list_groups(pool: &PgPool) -> Result<Vec<GroupListRow>, StoreError> {
    let rows = sqlx::query!(
        r#"
        SELECT g.group_code, g.group_ratio::text AS group_ratio, g.description,
               g.is_default, g.sort_order,
               (SELECT COUNT(*) FROM user_groups ug WHERE ug.group_code = g.group_code)
                   AS "user_count!",
               (SELECT COUNT(*) FROM group_channel_bindings gb
                 WHERE gb.group_code = g.group_code) AS "channel_count!"
        FROM price_groups g
        ORDER BY g.sort_order, g.group_code
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| GroupListRow {
            group_code: r.group_code,
            group_ratio: r.group_ratio,
            description: r.description,
            is_default: r.is_default,
            sort_order: r.sort_order,
            user_count: r.user_count,
            channel_count: r.channel_count,
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct ApiKeyListRow {
    pub id: i64,
    pub user_id: i64,
    pub username: String,
    pub team_id: Option<i64>,
    pub name: String,
    /// 只回前缀——密钥明文从不落库（仅存 SHA-256）。
    pub key_prefix: String,
    pub status: i16,
    pub quota_mode: i16,
    pub quota_micro: Option<i64>,
    pub used_micro: i64,
    pub model_allowlist: Option<serde_json::Value>,
    pub group_override: Option<String>,
    pub rpm_limit: Option<i32>,
    pub max_concurrency: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 令牌列表（管理员可跨用户；`user` = Some(uid) 限定单用户，供门户复用）。
pub async fn list_api_keys(
    pool: &PgPool,
    user: Option<i64>,
    query: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Page<ApiKeyListRow>, StoreError> {
    let (limit, offset) = clamp_page(limit, offset);
    let pattern = like_pattern(query);
    let rows = sqlx::query!(
        r#"
        SELECT k.id, k.user_id, u.username, k.team_id, k.name, k.key_prefix, k.status,
               k.quota_mode, k.quota_micro, k.used_micro, k.model_allowlist,
               k.group_override, k.rpm_limit, k.max_concurrency,
               k.expires_at, k.last_used_at, k.created_at
        FROM api_keys k JOIN users u ON u.id = k.user_id
        WHERE k.deleted_at IS NULL
          AND ($1::bigint IS NULL OR k.user_id = $1)
          AND ($2::text IS NULL OR k.name ILIKE $2 OR u.username ILIKE $2)
        ORDER BY k.id DESC
        LIMIT $3 OFFSET $4
        "#,
        user,
        pattern.as_deref(),
        limit,
        offset
    )
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!"
           FROM api_keys k JOIN users u ON u.id = k.user_id
           WHERE k.deleted_at IS NULL
             AND ($1::bigint IS NULL OR k.user_id = $1)
             AND ($2::text IS NULL OR k.name ILIKE $2 OR u.username ILIKE $2)"#,
        user,
        pattern.as_deref()
    )
    .fetch_one(pool)
    .await?;
    Ok(Page {
        data: rows
            .into_iter()
            .map(|r| ApiKeyListRow {
                id: r.id,
                user_id: r.user_id,
                username: r.username,
                team_id: r.team_id,
                name: r.name,
                key_prefix: r.key_prefix,
                status: r.status,
                quota_mode: r.quota_mode,
                quota_micro: r.quota_micro,
                used_micro: r.used_micro,
                model_allowlist: r.model_allowlist,
                group_override: r.group_override,
                rpm_limit: r.rpm_limit,
                max_concurrency: r.max_concurrency,
                expires_at: r.expires_at,
                last_used_at: r.last_used_at,
                created_at: r.created_at,
            })
            .collect(),
        total,
    })
}

#[derive(Debug, Serialize)]
pub struct RedemptionListRow {
    pub id: i64,
    pub batch_id: uuid::Uuid,
    pub amount_micro: i64,
    pub status: i16,
    pub plan_code: Option<String>,
    pub bind_user_id: Option<i64>,
    pub max_per_ip: Option<i32>,
    pub redeemed_by: Option<i64>,
    pub redeemed_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 兑换码列表（**不含码明文**：只存 SHA-256，生成时一次性返回）。
pub async fn list_redemptions(
    pool: &PgPool,
    batch: Option<uuid::Uuid>,
    status: Option<i16>,
    limit: i64,
    offset: i64,
) -> Result<Page<RedemptionListRow>, StoreError> {
    let (limit, offset) = clamp_page(limit, offset);
    let rows = sqlx::query!(
        r#"
        SELECT r.id, r.batch_id, r.amount_micro, r.status, p.plan_code AS "plan_code?",
               r.bind_user_id, r.max_per_ip, r.redeemed_by, r.redeemed_at,
               r.expires_at, r.created_at
        FROM redemption_codes r
        LEFT JOIN plans p ON p.id = r.plan_id
        WHERE ($1::uuid IS NULL OR r.batch_id = $1)
          AND ($2::smallint IS NULL OR r.status = $2)
        ORDER BY r.id DESC
        LIMIT $3 OFFSET $4
        "#,
        batch,
        status,
        limit,
        offset
    )
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM redemption_codes
           WHERE ($1::uuid IS NULL OR batch_id = $1)
             AND ($2::smallint IS NULL OR status = $2)"#,
        batch,
        status
    )
    .fetch_one(pool)
    .await?;
    Ok(Page {
        data: rows
            .into_iter()
            .map(|r| RedemptionListRow {
                id: r.id,
                batch_id: r.batch_id,
                amount_micro: r.amount_micro,
                status: r.status,
                plan_code: r.plan_code,
                bind_user_id: r.bind_user_id,
                max_per_ip: r.max_per_ip,
                redeemed_by: r.redeemed_by,
                redeemed_at: r.redeemed_at,
                expires_at: r.expires_at,
                created_at: r.created_at,
            })
            .collect(),
        total,
    })
}

#[derive(Debug, Serialize)]
pub struct PlanListRow {
    pub id: i64,
    pub plan_code: String,
    pub display_name: String,
    pub grant_micro: i64,
    pub group_code: Option<String>,
    pub balance_valid_days: Option<i32>,
    pub status: i16,
    pub created_at: DateTime<Utc>,
    /// 引用该套餐的兑换码数（删除前占用检查）。
    pub code_count: i64,
}

pub async fn list_plans(pool: &PgPool) -> Result<Vec<PlanListRow>, StoreError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.plan_code, p.display_name, p.grant_micro, p.group_code,
               p.balance_valid_days, p.status, p.created_at,
               (SELECT COUNT(*) FROM redemption_codes r WHERE r.plan_id = p.id)
                   AS "code_count!"
        FROM plans p ORDER BY p.id DESC
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| PlanListRow {
            id: r.id,
            plan_code: r.plan_code,
            display_name: r.display_name,
            grant_micro: r.grant_micro,
            group_code: r.group_code,
            balance_valid_days: r.balance_valid_days,
            status: r.status,
            created_at: r.created_at,
            code_count: r.code_count,
        })
        .collect())
}

#[derive(Debug, Serialize)]
pub struct SettingListRow {
    pub key: String,
    pub value: serde_json::Value,
    pub updated_by: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

/// 系统设置全量（前端设置页一次拉齐；敏感键脱敏在 console 层做）。
pub async fn list_settings(pool: &PgPool) -> Result<Vec<SettingListRow>, StoreError> {
    let rows =
        sqlx::query!(r#"SELECT key, value, updated_by, updated_at FROM settings ORDER BY key"#)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| SettingListRow {
            key: r.key,
            value: r.value,
            updated_by: r.updated_by,
            updated_at: r.updated_at,
        })
        .collect())
}

/// 角色占用计数（删除前检查；角色本体列表已在 console::admin::list_roles）。
pub async fn role_user_count(pool: &PgPool, role_code: &str) -> Result<Option<i64>, StoreError> {
    let row = sqlx::query!(
        r#"
        SELECT (SELECT COUNT(*) FROM users u
                 WHERE u.admin_role_id = r.id AND u.deleted_at IS NULL) AS "c!"
        FROM admin_roles r WHERE r.role_code = $1
        "#,
        role_code
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_params_are_clamped() {
        assert_eq!(clamp_page(50, 10), (50, 10));
        assert_eq!(clamp_page(0, -5), (1, 0), "非法值收敛到合法下界");
        assert_eq!(clamp_page(10_000, 0), (MAX_PAGE, 0), "上限封顶防拖垮 PG");
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern(Some("ab")).as_deref(), Some("%ab%"));
        // 调用方传入的通配符必须被转义，否则 "%" 会退化为全表扫描
        assert_eq!(like_pattern(Some("a%b")).as_deref(), Some("%a\\%b%"));
        assert_eq!(like_pattern(Some("a_b")).as_deref(), Some("%a\\_b%"));
        assert_eq!(like_pattern(Some("   ")), None, "空白视为不过滤");
        assert_eq!(like_pattern(None), None);
    }
}
