//! 用户/渠道开通与种子：单用户模式引导 + 开发/集成测试用的最小写路径。
//! 正式管理面 CRUD 属 console（M2）。

use crate::error::StoreError;
use sqlx::PgPool;

/// 单用户模式引导（IMPLEMENTATION §6.5）：确保 root 用户与 root key 存在。
/// 返回 (user_id, key_id, 是否新建了 key)。
/// Setup 向导：users 表为空时排他地创建首个超管 + key。
/// 表级排它锁保证并发首启只成功一次；已初始化返回 None。
pub async fn setup_first_admin(
    pool: &PgPool,
    username: &str,
    key_hash: &str,
    key_prefix: &str,
) -> Result<Option<(i64, i64)>, StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query!("LOCK TABLE users IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let existing = sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM users"#)
        .fetch_one(&mut *tx)
        .await?;
    if existing > 0 {
        return Ok(None);
    }
    let user_id = sqlx::query_scalar!(
        r#"INSERT INTO users (username, role) VALUES ($1, 100) RETURNING id"#,
        username
    )
    .fetch_one(&mut *tx)
    .await?;
    let key_id = sqlx::query_scalar!(
        r#"
        INSERT INTO api_keys (user_id, key_hash, key_prefix, name)
        VALUES ($1, $2, $3, 'setup-admin')
        RETURNING id
        "#,
        user_id,
        key_hash,
        key_prefix
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((user_id, key_id)))
}

pub async fn ensure_root(
    pool: &PgPool,
    key_hash: &str,
    key_prefix: &str,
) -> Result<(i64, i64, bool), StoreError> {
    let user_id = match sqlx::query_scalar!(
        r#"SELECT id FROM users WHERE username = 'root' AND deleted_at IS NULL"#
    )
    .fetch_optional(pool)
    .await?
    {
        Some(id) => id,
        None => {
            sqlx::query_scalar!(
                r#"INSERT INTO users (username, role) VALUES ('root', 100) RETURNING id"#
            )
            .fetch_one(pool)
            .await?
        }
    };

    if let Some(key_id) = sqlx::query_scalar!(
        r#"SELECT id FROM api_keys WHERE user_id = $1 AND name = 'root' AND deleted_at IS NULL"#,
        user_id
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok((user_id, key_id, false));
    }

    let key_id = sqlx::query_scalar!(
        r#"
        INSERT INTO api_keys (user_id, name, key_hash, key_prefix)
        VALUES ($1, 'root', $2, $3)
        RETURNING id
        "#,
        user_id,
        key_hash,
        key_prefix
    )
    .fetch_one(pool)
    .await?;

    Ok((user_id, key_id, true))
}

/// 创建用户（种子/测试）。
pub async fn create_user(pool: &PgPool, username: &str) -> Result<i64, StoreError> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO users (username) VALUES ($1) RETURNING id"#,
        username
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 创建 API key（种子/测试；key_hash = SHA-256 hex）。
pub async fn create_api_key(
    pool: &PgPool,
    user_id: i64,
    key_hash: &str,
    key_prefix: &str,
) -> Result<i64, StoreError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO api_keys (user_id, name, key_hash, key_prefix)
        VALUES ($1, 'seed', $2, $3)
        RETURNING id
        "#,
        user_id,
        key_hash,
        key_prefix
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 创建模型 + 倍率定价（种子/测试）；倍率以十进制字符串精确入库。
pub async fn create_model_ratio(
    pool: &PgPool,
    model_name: &str,
    model_ratio: &str,
    completion_ratio: &str,
    cache_ratio: &str,
) -> Result<i64, StoreError> {
    let model_id = sqlx::query_scalar!(
        r#"INSERT INTO models (model_name) VALUES ($1) RETURNING id"#,
        model_name
    )
    .fetch_one(pool)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO model_pricing (model_id, pricing_mode, model_ratio, completion_ratio, cache_ratio)
        VALUES ($1, 'ratio', ($2::text)::numeric, ($3::text)::numeric, ($4::text)::numeric)
        "#,
        model_id,
        model_ratio,
        completion_ratio,
        cache_ratio
    )
    .execute(pool)
    .await?;
    Ok(model_id)
}

/// 创建渠道 + 一把 key（种子/测试）。
// 种子/测试用的直插助手：参数即建表列，聚成结构体反而在调用点更啰嗦
#[allow(clippy::too_many_arguments)]
pub async fn create_channel(
    pool: &PgPool,
    name: &str,
    provider: &str,
    api_base: &str,
    credential: &str,
    models: &[&str],
    trust_upstream_usage: bool,
    master_key: Option<&str>,
) -> Result<(i64, i64), StoreError> {
    let models_json = serde_json::json!(models);
    let channel_id = sqlx::query_scalar!(
        r#"
        INSERT INTO channels (name, provider, api_base, models, trust_upstream_usage)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        "#,
        name,
        provider,
        api_base,
        models_json,
        trust_upstream_usage
    )
    .fetch_one(pool)
    .await?;

    let key_id = sqlx::query_scalar!(
        r#"
        INSERT INTO channel_keys (channel_id, credential_ciphertext)
        VALUES ($1, $2)
        RETURNING id
        "#,
        channel_id,
        crate::credential::seal_or_plain(master_key, credential)?
    )
    .fetch_one(pool)
    .await?;

    // 新渠道缺省进内置 default 池：渠道只服务它所在的池，不入池即对谁都不可达。
    // 调用方要专属可见性时再用 set_channel_pools 覆盖成员关系。
    sqlx::query!(
        r#"INSERT INTO pool_channels (pool_code, channel_id) VALUES ($1, $2)
           ON CONFLICT DO NOTHING"#,
        crate::channels::DEFAULT_POOL,
        channel_id
    )
    .execute(pool)
    .await?;

    Ok((channel_id, key_id))
}
