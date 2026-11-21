use crate::error::StoreError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// 追加用户分组（套餐核销用；已在组则幂等）。
pub async fn add_user_group(
    pool: &PgPool,
    user_id: i64,
    group_code: &str,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"
        INSERT INTO user_groups (user_id, group_code, priority)
        VALUES ($1, $2, 0)
        ON CONFLICT (user_id, group_code) DO NOTHING
        "#,
        user_id,
        group_code
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 用户个人计价系数（`users.price_multiplier`）。十进制字符串入库，计费链路不碰浮点。
/// 返回是否命中（false = 用户不存在/已删除）。
///
/// 此前这一列全仓只读：鉴权链路读它、`/admin/users` 返回它、前端用户列表还专门有一列
/// `×{price_multiplier}` 在展示，却没有任何写入路径——它永远只能是建表默认的 1.0。
pub async fn set_user_multiplier(
    pool: &PgPool,
    user_id: i64,
    multiplier: &str,
) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"UPDATE users SET price_multiplier = ($2::text)::numeric, updated_at = now()
           WHERE id = $1 AND deleted_at IS NULL"#,
        user_id,
        multiplier
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 用户能给自己的 key 选的分组（IMPLEMENTATION §11.14 R4），`g` = price_groups、`$1` = user_id：
/// 分配给他的组 ∪ 标为 `self_select` 的公开档位 ∪ 默认组。默认组只在管理员没给他分配分组时才给：
/// 分配了分组，就是把他限定在这些组（外加公开档位）里。套餐带来的组（`granted_group`）随套餐收回，
/// 不算管理员分配，买了套餐照样能选默认组。宏而不是常量：要在编译期 `concat!` 进查询文本。
macro_rules! selectable_group {
    () => {
        r"(
    g.self_select
    OR EXISTS (SELECT 1 FROM user_groups ug WHERE ug.user_id = $1 AND ug.group_code = g.group_code)
    OR (g.is_default AND NOT EXISTS (
        SELECT 1 FROM user_groups ug
        WHERE ug.user_id = $1
          AND NOT EXISTS (SELECT 1 FROM user_subscriptions s
                          WHERE s.user_id = ug.user_id AND s.status = 1 AND s.granted_group
                            AND s.group_code_snapshot = ug.group_code)))
)"
    };
}

/// 可选分组的一行（门户建 key / 改 key 的档位下拉）。
#[derive(Debug, sqlx::FromRow)]
pub struct SelectableGroupRow {
    pub group_code: String,
    pub ratio: String,
    pub description: Option<String>,
    pub self_select: bool,
    /// 在 `user_groups` 里（管理员分配或套餐带来）。
    pub assigned: bool,
}

/// 用户可选的分组，见 `selectable_group!`。
pub async fn selectable_groups(
    pool: &PgPool,
    user_id: i64,
) -> Result<Vec<SelectableGroupRow>, StoreError> {
    Ok(sqlx::query_as::<_, SelectableGroupRow>(concat!(
        "SELECT g.group_code, g.group_ratio::text AS ratio, g.description, g.self_select,
                EXISTS (SELECT 1 FROM user_groups ug
                         WHERE ug.user_id = $1 AND ug.group_code = g.group_code) AS assigned
         FROM price_groups g
         WHERE ",
        selectable_group!(),
        " ORDER BY g.sort_order, g.group_code"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// 覆盖式设置用户分组（定价取最高 priority，可见性取并集，§6.3）。
///
/// 同一事务里把他名下绑在可选范围之外的 key 改回「跟随用户分组」，返回改了几把：分配分组是把人
/// 限定在这些组里，留着绑在默认组或别的组上的 key 就等于没限定；从私有组里移出时同理收回。
pub async fn set_user_groups(
    pool: &PgPool,
    user_id: i64,
    groups: &[(String, i32)],
) -> Result<u64, StoreError> {
    let codes: Vec<String> = groups.iter().map(|(c, _)| c.clone()).collect();
    let priorities: Vec<i32> = groups.iter().map(|(_, p)| *p).collect();
    let mut tx = pool.begin().await?;
    // Serialize replacement even when the old membership set is empty.
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query!(r#"DELETE FROM user_groups WHERE user_id = $1"#, user_id)
        .execute(&mut *tx)
        .await?;
    if !groups.is_empty() {
        sqlx::query!(
            r#"
            INSERT INTO user_groups (user_id, group_code, priority)
            SELECT $1, code, prio FROM unnest($2::varchar[], $3::int[]) AS t(code, prio)
            "#,
            user_id,
            &codes,
            &priorities
        )
        .execute(&mut *tx)
        .await?;
    }
    let reset = sqlx::query(concat!(
        "UPDATE api_keys k SET group_override = NULL
         WHERE k.user_id = $1 AND k.deleted_at IS NULL AND k.group_override IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM price_groups g
                           WHERE g.group_code = k.group_override AND ",
        selectable_group!(),
        ")"
    ))
    .bind(user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(reset)
}

/// 创建自定义管理子角色（权限点集合）。
pub async fn create_admin_role(
    pool: &PgPool,
    role_code: &str,
    display_name: &str,
    permissions: &serde_json::Value,
) -> Result<i64, StoreError> {
    // 按 role_code upsert：权限点集合随功能增长要改（新增只读端点时给"只读运营"补权限），
    // 此前纯 INSERT 让角色一经创建就不可修改，只能删了重建——而删除会被"仍有用户绑定"拒绝
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO admin_roles (role_code, display_name, permissions)
        VALUES ($1, $2, $3)
        ON CONFLICT (role_code) DO UPDATE
            SET display_name = EXCLUDED.display_name,
                permissions = EXCLUDED.permissions
        RETURNING id
        "#,
        role_code,
        display_name,
        permissions
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 调整用户平台角色与自定义子角色绑定。返回是否命中行。
pub async fn assign_user_role(
    pool: &PgPool,
    user_id: i64,
    role: Option<i16>,
    admin_role_id: Option<i64>,
) -> Result<bool, StoreError> {
    let result = sqlx::query!(
        r#"
        UPDATE users
        SET role = COALESCE($2, role),
            admin_role_id = $3,
            updated_at = now()
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        user_id,
        role,
        admin_role_id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

// ---- API key 生命周期（用户自助 + 管理管控）----

/// api_keys 可写字段补丁。
///
/// 三态语义：`None` = 不改；`Some(None)` = 置空（解除限制）；`Some(Some(v))` = 设为 v。
/// 分层由调用方决定：自助面支持账号所有者管理额度和访问限制；
/// 分组须在管理员划定的可选集合内，速率限制仍仅管理面填写。
#[derive(Debug, Default, Clone)]
pub struct ApiKeyPatch {
    /// Lifetime spending cap. None = unchanged; Some(None) = unlimited.
    pub quota_micro: Option<Option<i64>>,
    pub name: Option<String>,
    /// 1=启用 2=停用（3=expired 由过期时间派生，不接受直接写）。
    pub status: Option<i16>,
    pub expires_at: Option<Option<DateTime<Utc>>>,
    pub model_allowlist: Option<Option<serde_json::Value>>,
    pub group_override: Option<Option<String>>,
    /// IP 白名单（地址 / CIDR 字符串数组；Some(None) = 解除限制）。
    pub ip_allowlist: Option<Option<serde_json::Value>>,
    pub rpm_limit: Option<Option<i32>>,
    pub tpm_limit: Option<Option<i32>>,
    pub rpd_limit: Option<Option<i32>>,
    pub daily_token_limit: Option<Option<i64>>,
    pub max_concurrency: Option<Option<i32>>,
}

/// 被改动的 key 标识：`key_hash` 供调用方精确失效 `auth:key:*` 缓存，
/// `user_id` 供审计记录归属主体。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TouchedApiKey {
    pub key_hash: String,
    pub user_id: i64,
}

/// 部分更新 api_keys。`owner` = Some(uid) 时限定属主（自助面越权防线，
/// 与 404 同响应不泄漏他人 key 是否存在）。返回 None = 不存在/非属主。
pub async fn patch_api_key(
    pool: &PgPool,
    key_id: i64,
    owner: Option<i64>,
    patch: &ApiKeyPatch,
) -> Result<Option<TouchedApiKey>, StoreError> {
    if patch
        .quota_micro
        .flatten()
        .is_some_and(|v| !(1..=9_007_199_254_740_991).contains(&v))
    {
        return Err(StoreError::InvalidData("quota_micro"));
    }
    let row = sqlx::query_as::<_, TouchedApiKey>(
        r"
        UPDATE api_keys SET
            name              = COALESCE($3, name),
            status            = COALESCE($4::smallint, status),
            expires_at        = CASE WHEN $5::bool THEN $6::timestamptz ELSE expires_at END,
            model_allowlist   = CASE WHEN $7::bool THEN $8::jsonb ELSE model_allowlist END,
            group_override    = CASE WHEN $9::bool THEN $10::varchar ELSE group_override END,
            rpm_limit         = CASE WHEN $11::bool THEN $12::int ELSE rpm_limit END,
            tpm_limit         = CASE WHEN $13::bool THEN $14::int ELSE tpm_limit END,
            rpd_limit         = CASE WHEN $15::bool THEN $16::int ELSE rpd_limit END,
            daily_token_limit = CASE WHEN $17::bool THEN $18::bigint ELSE daily_token_limit END,
            max_concurrency   = CASE WHEN $19::bool THEN $20::int ELSE max_concurrency END,
            ip_allowlist      = CASE WHEN $21::bool THEN $22::jsonb ELSE ip_allowlist END,
            quota_mode        = CASE WHEN $23::bool THEN CASE WHEN $24::bigint IS NULL THEN 0 ELSE 1 END ELSE quota_mode END,
            quota_micro       = CASE WHEN $23::bool THEN $24::bigint ELSE quota_micro END
        WHERE id = $1 AND deleted_at IS NULL
          AND ($2::bigint IS NULL OR user_id = $2)
        RETURNING key_hash, user_id
        ",
    )
    .bind(key_id)
    .bind(owner)
    .bind(patch.name.as_deref())
    .bind(patch.status)
    .bind(patch.expires_at.is_some())
    .bind(patch.expires_at.flatten())
    .bind(patch.model_allowlist.is_some())
    .bind(patch.model_allowlist.as_ref().and_then(Option::as_ref))
    .bind(patch.group_override.is_some())
    .bind(patch.group_override.as_ref().and_then(Option::as_deref))
    .bind(patch.rpm_limit.is_some())
    .bind(patch.rpm_limit.flatten())
    .bind(patch.tpm_limit.is_some())
    .bind(patch.tpm_limit.flatten())
    .bind(patch.rpd_limit.is_some())
    .bind(patch.rpd_limit.flatten())
    .bind(patch.daily_token_limit.is_some())
    .bind(patch.daily_token_limit.flatten())
    .bind(patch.max_concurrency.is_some())
    .bind(patch.max_concurrency.flatten())
    .bind(patch.ip_allowlist.is_some())
    .bind(patch.ip_allowlist.as_ref().and_then(Option::as_ref))
    .bind(patch.quota_micro.is_some())
    .bind(patch.quota_micro.flatten())
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| TouchedApiKey {
        key_hash: r.key_hash,
        user_id: r.user_id,
    }))
}

/// 软删除 api_key：保留行以占住 `key_hash` 唯一约束，杜绝同明文 key 复活。
/// 鉴权回源已按 `deleted_at IS NULL` 过滤，缓存失效后立即失效。
pub async fn soft_delete_api_key(
    pool: &PgPool,
    key_id: i64,
    owner: Option<i64>,
) -> Result<Option<TouchedApiKey>, StoreError> {
    let row = sqlx::query!(
        r#"
        UPDATE api_keys SET deleted_at = now()
        WHERE id = $1 AND deleted_at IS NULL
          AND ($2::bigint IS NULL OR user_id = $2)
        RETURNING key_hash, user_id
        "#,
        key_id,
        owner
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| TouchedApiKey {
        key_hash: r.key_hash,
        user_id: r.user_id,
    }))
}

/// 软删一条 web 会话换来的全部登录 key（退出登录用）；返回被删 key 的哈希，供调用方清鉴权缓存。
pub async fn soft_delete_session_keys(
    pool: &PgPool,
    session_hash: &str,
) -> Result<Vec<String>, StoreError> {
    Ok(sqlx::query_scalar(
        "UPDATE api_keys SET deleted_at = now() \
         WHERE session_hash = $1 AND deleted_at IS NULL RETURNING key_hash",
    )
    .bind(session_hash)
    .fetch_all(pool)
    .await?)
}
