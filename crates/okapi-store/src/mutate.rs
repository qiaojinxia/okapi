//! 管理面变更操作：更新 / 删除 / 批量（补齐 CRUD 的 U 与 D，IMPLEMENTATION §11.6）。
//!
//! 删除策略分两类，取舍依据是"账本可解释性"：
//! - **软删**（channels / api_keys / users）：billing_records 引用这些 id，硬删会让历史
//!   账单失去可解释性，故走 `deleted_at` 并同步停用从属资源；
//! - **配置类硬删**（models / groups / plans / roles / rules）：纯配置无账本引用，但删除前
//!   做占用检查，被引用时返回 `Conflict` 让管理端先解绑——不静默级联，避免用户
//!   悄悄掉回默认组导致计费口径突变。
//!
//! 定价类变更后须发布新 epoch 才生效（PriceBook 是编译期快照），由 console 层提示。

use crate::error::StoreError;
use sqlx::PgPool;

/// 渠道可编辑字段（None = 不改动）。
#[derive(Debug, Default)]
pub struct ChannelPatch<'a> {
    pub name: Option<&'a str>,
    pub api_base: Option<&'a str>,
    pub models: Option<&'a serde_json::Value>,
    pub model_mapping: Option<&'a serde_json::Value>,
    pub capabilities: Option<&'a serde_json::Value>,
    pub settings: Option<&'a serde_json::Value>,
    pub retry_policy: Option<&'a serde_json::Value>,
    pub upstream_unit_cost: Option<&'a serde_json::Value>,
    pub priority: Option<i32>,
    pub weight: Option<i32>,
    pub trust_upstream_usage: Option<bool>,
}

/// 局部更新渠道；false = 渠道不存在。
/// COALESCE 语义：未传字段保持原值；api_base 传空串则清空（NULLIF）。
pub async fn patch_channel(
    pool: &PgPool,
    channel_id: i64,
    patch: &ChannelPatch<'_>,
) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"
        UPDATE channels SET
            name                 = COALESCE($2, name),
            api_base             = CASE WHEN $3::text IS NULL THEN api_base
                                        ELSE NULLIF($3, '') END,
            models               = COALESCE($4, models),
            model_mapping        = COALESCE($5, model_mapping),
            capabilities         = COALESCE($6, capabilities),
            settings             = COALESCE($7, settings),
            retry_policy         = COALESCE($8, retry_policy),
            upstream_unit_cost   = COALESCE($9, upstream_unit_cost),
            priority             = COALESCE($10, priority),
            weight               = COALESCE($11, weight),
            trust_upstream_usage = COALESCE($12, trust_upstream_usage),
            updated_at           = now()
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        channel_id,
        patch.name,
        patch.api_base,
        patch.models,
        patch.model_mapping,
        patch.capabilities,
        patch.settings,
        patch.retry_policy,
        patch.upstream_unit_cost,
        patch.priority,
        patch.weight,
        patch.trust_upstream_usage
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 软删渠道（同时停用其下 key，避免调度器取到孤儿 key）。
pub async fn delete_channel(pool: &PgPool, channel_id: i64) -> Result<bool, StoreError> {
    let mut tx = pool.begin().await?;
    let affected = sqlx::query!(
        r#"UPDATE channels SET deleted_at = now(), status = 2, updated_at = now()
           WHERE id = $1 AND deleted_at IS NULL"#,
        channel_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if affected > 0 {
        sqlx::query!(
            r#"UPDATE channel_keys SET status = 2 WHERE channel_id = $1"#,
            channel_id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(affected > 0)
}

/// 批量改渠道状态（1=启用 2=停用）；返回影响行数。
pub async fn batch_set_channel_status(
    pool: &PgPool,
    ids: &[i64],
    status: i16,
) -> Result<u64, StoreError> {
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query!(
        r#"UPDATE channels SET status = $2, updated_at = now()
           WHERE id = ANY($1) AND deleted_at IS NULL"#,
        ids,
        status
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// 批量软删渠道。
pub async fn batch_delete_channels(pool: &PgPool, ids: &[i64]) -> Result<u64, StoreError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut tx = pool.begin().await?;
    let affected = sqlx::query!(
        r#"UPDATE channels SET deleted_at = now(), status = 2, updated_at = now()
           WHERE id = ANY($1) AND deleted_at IS NULL"#,
        ids
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query!(
        r#"UPDATE channel_keys SET status = 2 WHERE channel_id = ANY($1)"#,
        ids
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(affected)
}

/// 复制渠道（对齐 new-api "复制渠道"）：连同 key 与可见组克隆。
/// 新渠道**默认停用**——避免半配置的复制体立刻进调度。返回新 id；None = 源不存在。
pub async fn duplicate_channel(
    pool: &PgPool,
    channel_id: i64,
    new_name: &str,
) -> Result<Option<i64>, StoreError> {
    let mut tx = pool.begin().await?;
    let new_id = sqlx::query_scalar!(
        r#"
        INSERT INTO channels
            (name, provider, api_base, status, priority, weight, models, model_mapping,
             capabilities, settings, retry_policy, upstream_unit_cost,
             trust_upstream_usage, owner_id)
        SELECT $2, provider, api_base, 2, priority, weight, models, model_mapping,
               capabilities, settings, retry_policy, upstream_unit_cost,
               trust_upstream_usage, owner_id
        FROM channels WHERE id = $1 AND deleted_at IS NULL
        RETURNING id
        "#,
        channel_id,
        new_name
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(new_id) = new_id else {
        tx.rollback().await?;
        return Ok(None);
    };
    sqlx::query!(
        r#"
        INSERT INTO channel_keys (channel_id, credential_ciphertext, weight, max_concurrency)
        SELECT $2, credential_ciphertext, weight, max_concurrency
        FROM channel_keys WHERE channel_id = $1
        "#,
        channel_id,
        new_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO pool_channels (pool_code, channel_id)
        SELECT pool_code, $2 FROM pool_channels WHERE channel_id = $1
        "#,
        channel_id,
        new_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(new_id))
}

/// 删除渠道池。被分组或令牌引用时回 `Conflict`——那些主体会因此失去可见渠道，
/// 静默解绑等于悄悄放开可见性。池内渠道成员关系随 FK CASCADE 清理，不算占用。
pub async fn delete_channel_pool(pool: &PgPool, pool_code: &str) -> Result<bool, StoreError> {
    // 内置 default 池是"分组必有池"这条规则的兜底，不可删
    if pool_code == crate::channels::DEFAULT_POOL {
        return Err(StoreError::Conflict("builtin_pool"));
    }
    let refs = sqlx::query!(
        r#"
        SELECT (SELECT COUNT(*) FROM price_groups WHERE pool_code = $1) AS "groups!",
               (SELECT COUNT(*) FROM api_keys
                 WHERE pool_override = $1 AND deleted_at IS NULL)       AS "keys!",
               (SELECT COUNT(*) FROM channel_pools
                 WHERE fallback_pool_code = $1)                          AS "fallbacks!"
        "#,
        pool_code
    )
    .fetch_one(pool)
    .await?;
    // 被别的池当降级目标同样算引用：静默删掉等于那些池悄悄失去兜底
    if refs.groups > 0 || refs.keys > 0 || refs.fallbacks > 0 {
        return Err(StoreError::Conflict("pool_in_use"));
    }
    let affected = sqlx::query!(
        r#"DELETE FROM channel_pools WHERE pool_code = $1"#,
        pool_code
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 删除模型及其定价（硬删；user_pricing 覆盖随 FK CASCADE 清理）。
/// 仍被其他模型的降级链引用时回 `Conflict`——静默删除会让那些模型的兜底
/// 悄悄变短，且只在主模型全挂的最脆弱时刻才暴露。
pub async fn delete_model(pool: &PgPool, model_name: &str) -> Result<bool, StoreError> {
    let referrers = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*)::bigint AS "c!"
        FROM models
        WHERE model_name <> $1 AND fallback_models @> to_jsonb($1::text)
        "#,
        model_name
    )
    .fetch_one(pool)
    .await?;
    if referrers > 0 {
        return Err(StoreError::Conflict("model_in_fallback_chain"));
    }
    let affected = sqlx::query!(r#"DELETE FROM models WHERE model_name = $1"#, model_name)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(affected > 0)
}

/// 删除定价分组。默认组与被占用（用户/令牌/套餐）时返回 `Conflict`。
/// 渠道可见性已移到池，分组只是引用池，删分组不影响池本身。
pub async fn delete_price_group(pool: &PgPool, group_code: &str) -> Result<bool, StoreError> {
    let Some(is_default) = sqlx::query_scalar!(
        r#"SELECT is_default FROM price_groups WHERE group_code = $1"#,
        group_code
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(false);
    };
    if is_default {
        return Err(StoreError::Conflict("group_is_default"));
    }
    let refs = sqlx::query!(
        r#"
        SELECT (SELECT COUNT(*) FROM user_groups WHERE group_code = $1)            AS "users!",
               (SELECT COUNT(*) FROM api_keys
                 WHERE group_override = $1 AND deleted_at IS NULL)                 AS "keys!",
               (SELECT COUNT(*) FROM plans WHERE group_code = $1)                  AS "plans!"
        "#,
        group_code
    )
    .fetch_one(pool)
    .await?;
    if refs.users > 0 || refs.keys > 0 || refs.plans > 0 {
        return Err(StoreError::Conflict("group_in_use"));
    }
    sqlx::query!(
        r#"DELETE FROM price_groups WHERE group_code = $1"#,
        group_code
    )
    .execute(pool)
    .await?;
    Ok(true)
}

/// 删除套餐；已被兑换码引用时返回 `Conflict`（历史兑换码需保留套餐语义）。
pub async fn delete_plan(pool: &PgPool, plan_code: &str) -> Result<bool, StoreError> {
    let Some(plan_id) =
        sqlx::query_scalar!(r#"SELECT id FROM plans WHERE plan_code = $1"#, plan_code)
            .fetch_optional(pool)
            .await?
    else {
        return Ok(false);
    };
    let refs = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM redemption_codes WHERE plan_id = $1"#,
        plan_id
    )
    .fetch_one(pool)
    .await?;
    if refs > 0 {
        return Err(StoreError::Conflict("plan_in_use"));
    }
    sqlx::query!(r#"DELETE FROM plans WHERE id = $1"#, plan_id)
        .execute(pool)
        .await?;
    Ok(true)
}

/// 删除自定义管理角色；仍有用户绑定时返回 `Conflict`（防批量掉权）。
pub async fn delete_role(pool: &PgPool, role_code: &str) -> Result<bool, StoreError> {
    let Some(role_id) = sqlx::query_scalar!(
        r#"SELECT id FROM admin_roles WHERE role_code = $1"#,
        role_code
    )
    .fetch_optional(pool)
    .await?
    else {
        return Ok(false);
    };
    let bound = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM users
           WHERE admin_role_id = $1 AND deleted_at IS NULL"#,
        role_id
    )
    .fetch_one(pool)
    .await?;
    if bound > 0 {
        return Err(StoreError::Conflict("role_in_use"));
    }
    sqlx::query!(r#"DELETE FROM admin_roles WHERE id = $1"#, role_id)
        .execute(pool)
        .await?;
    Ok(true)
}

/// 停用整批未核销兑换码（status 1 → 3）；已核销的不动。
pub async fn disable_redemption_batch(pool: &PgPool, batch: uuid::Uuid) -> Result<u64, StoreError> {
    Ok(sqlx::query!(
        r#"UPDATE redemption_codes SET status = 3 WHERE batch_id = $1 AND status = 1"#,
        batch
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// 用户管理动作（吸收 new-api `POST /api/user/manage` 的统一操作端点形状）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAction {
    Ban,
    Unban,
    /// 提为 admin（10）。
    Promote,
    /// 降为普通用户（1）。
    Demote,
    /// 软删（保留账本外键）。
    Delete,
}

impl UserAction {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ban" => Some(Self::Ban),
            "unban" => Some(Self::Unban),
            "promote" => Some(Self::Promote),
            "demote" => Some(Self::Demote),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

/// 执行用户管理动作；false = 用户不存在（或已软删）。
///
/// super_admin 保护与"不可作用于自己"由调用方叠加校验。此处保证封禁/软删
/// **同时吊销其令牌**——否则鉴权缓存 TTL 内已发出的 key 仍能打数据面。
pub async fn manage_user(
    pool: &PgPool,
    user_id: i64,
    action: UserAction,
) -> Result<bool, StoreError> {
    let mut tx = pool.begin().await?;
    let affected = match action {
        UserAction::Ban => sqlx::query!(
            r#"UPDATE users SET status = 2, updated_at = now()
                   WHERE id = $1 AND deleted_at IS NULL"#,
            user_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
        UserAction::Unban => sqlx::query!(
            r#"UPDATE users SET status = 1, updated_at = now()
                   WHERE id = $1 AND deleted_at IS NULL"#,
            user_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
        UserAction::Promote | UserAction::Demote => {
            let role: i16 = if action == UserAction::Promote { 10 } else { 1 };
            // role < 100 守卫：super_admin 不可被降权，即便调用方越过上层校验
            sqlx::query!(
                r#"UPDATE users SET role = $2, updated_at = now()
                   WHERE id = $1 AND deleted_at IS NULL AND role < 100"#,
                user_id,
                role
            )
            .execute(&mut *tx)
            .await?
            .rows_affected()
        }
        UserAction::Delete => sqlx::query!(
            r#"UPDATE users SET deleted_at = now(), status = 2, updated_at = now()
                   WHERE id = $1 AND deleted_at IS NULL"#,
            user_id
        )
        .execute(&mut *tx)
        .await?
        .rows_affected(),
    };
    if affected > 0 && matches!(action, UserAction::Ban | UserAction::Delete) {
        sqlx::query!(
            r#"UPDATE api_keys SET status = 2 WHERE user_id = $1 AND deleted_at IS NULL"#,
            user_id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(affected > 0)
}

/// 软删令牌。
pub async fn delete_api_key(pool: &PgPool, key_id: i64) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"UPDATE api_keys SET deleted_at = now(), status = 2
           WHERE id = $1 AND deleted_at IS NULL"#,
        key_id
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 改令牌状态（1=启用 2=停用）。
pub async fn set_api_key_status(
    pool: &PgPool,
    key_id: i64,
    status: i16,
) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"UPDATE api_keys SET status = $2 WHERE id = $1 AND deleted_at IS NULL"#,
        key_id,
        status
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 令牌属主（own 范围校验用）；None = 不存在。
pub async fn api_key_owner(pool: &PgPool, key_id: i64) -> Result<Option<i64>, StoreError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT user_id FROM api_keys WHERE id = $1 AND deleted_at IS NULL"#,
        key_id
    )
    .fetch_optional(pool)
    .await?)
}

/// 启停定价规则（不删配置，便于活动复用）。
pub async fn set_pricing_rule_enabled(
    pool: &PgPool,
    rule_code: &str,
    enabled: bool,
) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"UPDATE pricing_rules SET enabled = $2, updated_at = now() WHERE rule_code = $1"#,
        rule_code,
        enabled
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_action_parses_known_verbs_only() {
        assert_eq!(UserAction::parse("ban"), Some(UserAction::Ban));
        assert_eq!(UserAction::parse("promote"), Some(UserAction::Promote));
        assert_eq!(UserAction::parse("delete"), Some(UserAction::Delete));
        // 未知动作必须拒绝，不可退化为任意默认行为
        assert_eq!(UserAction::parse("DROP TABLE"), None);
        assert_eq!(UserAction::parse(""), None);
        assert_eq!(UserAction::parse("BAN"), None, "大小写敏感，避免误匹配");
    }
}
