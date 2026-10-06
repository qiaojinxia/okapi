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
    create_channel_configured(
        pool,
        ChannelCreate {
            name,
            provider,
            api_base,
            credential,
            models,
            trust_upstream_usage,
            owner_id: None,
            settings: None,
            priority: 0,
            max_concurrency: None,
            cost_milli: None,
            pools: None,
            egress: None,
            egress_preassigned: None,
        },
        master_key,
    )
    .await
}

/// One transaction persists a channel, its sole initial credential and all routing options.
/// The record deliberately has no Debug implementation because it contains a credential.
pub struct ChannelCreate<'a> {
    pub name: &'a str,
    pub provider: &'a str,
    pub api_base: &'a str,
    pub credential: &'a str,
    pub models: &'a [&'a str],
    pub trust_upstream_usage: bool,
    pub owner_id: Option<i64>,
    pub settings: Option<&'a serde_json::Value>,
    pub priority: i32,
    pub max_concurrency: Option<i32>,
    pub cost_milli: Option<i64>,
    /// None joins default; an explicit empty slice creates an unreachable channel.
    pub pools: Option<&'a [crate::admin::PoolMember]>,
    /// 出口绑定（§11.41）；None = 继承全局默认。
    pub egress: Option<&'a crate::egress::Binding>,
    /// 固定分配组下换码前已选定的代理（OAuth 登录）：新 key 直接落这个分配，不再另分。
    pub egress_preassigned: Option<i64>,
}

pub async fn create_channel_configured(
    pool: &PgPool,
    input: ChannelCreate<'_>,
    master_key: Option<&str>,
) -> Result<(i64, i64), StoreError> {
    let sealed = crate::credential::seal_or_plain(master_key, input.credential)?;
    let models = serde_json::json!(input.models);
    let settings = input
        .settings
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let costs = input
        .cost_milli
        .map(|cost| serde_json::json!({"relative_cost_milli":cost}));
    let mut tx = pool.begin().await?;
    let channel_id: i64 = sqlx::query_scalar(
        "INSERT INTO channels (name,provider,api_base,models,trust_upstream_usage,owner_id,settings,priority,upstream_unit_cost) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
    ).bind(input.name).bind(input.provider).bind(input.api_base).bind(models)
        .bind(input.trust_upstream_usage).bind(input.owner_id).bind(settings)
        .bind(input.priority).bind(costs).fetch_one(&mut *tx).await?;
    let key_id: i64 = sqlx::query_scalar(
        "INSERT INTO channel_keys (channel_id,credential_ciphertext,credential_kind,max_concurrency) VALUES ($1,$2,$3,$4) RETURNING id",
    ).bind(channel_id).bind(sealed)
        .bind(i16::from(crate::credential::OAuthCredential::parse(input.credential).is_some()))
        .bind(input.max_concurrency).fetch_one(&mut *tx).await?;
    let defaults = [crate::admin::PoolMember {
        pool_code: crate::channels::DEFAULT_POOL.into(),
        priority_override: None,
        weight_override: None,
    }];
    for member in input.pools.unwrap_or(&defaults) {
        sqlx::query("INSERT INTO pool_channels (pool_code,channel_id,priority_override,weight_override) VALUES ($1,$2,$3,$4)")
            .bind(&member.pool_code).bind(channel_id).bind(member.priority_override).bind(member.weight_override)
            .execute(&mut *tx).await?;
    }
    // 继承也要对账：全局默认若是固定分配组，新 key 当场分到代理
    crate::egress::bind_new_channel(
        &mut tx,
        channel_id,
        key_id,
        input.egress.unwrap_or(&crate::egress::Binding::Inherit),
        input.egress_preassigned,
    )
    .await?;
    tx.commit().await?;
    Ok((channel_id, key_id))
}
