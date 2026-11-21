use crate::error::StoreError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelRow {
    pub id: i64,
    pub name: String,
    pub provider: String,
    pub api_base: Option<String>,
    pub status: i16,
    pub priority: i32,
    pub models: serde_json::Value,
    pub trust_upstream_usage: bool,
    pub owner_id: Option<i64>,
    /// 渠道行为开关（thinking_to_content / bill_by_response_model / strip_request_fields）。
    /// 管理端要按当前值渲染开关，否则编辑表单只能默认全关——保存即静默清掉已生效的配置。
    pub settings: serde_json::Value,
    /// 所属渠道池代码。同理由：不回显当前值，编辑面一保存就把池成员关系清空。
    pub pools: Vec<String>,
    /// 池成员关系明细（含成员级调度参数覆盖）。
    pub pool_members: Vec<PoolMember>,
    /// 相对成本系数（千分比；1000 = 官方标价）。既是调度加权除数，也是毛利核算里
    /// "官方价 × 系数 = 上游成本"的系数（§11.18）。
    pub cost_milli: i64,
    /// 出口绑定三列（§11.41；`egress::Binding::from_columns` 还原）。
    pub egress_mode: Option<String>,
    pub egress_proxy_id: Option<i64>,
    pub egress_group_code: Option<String>,
}

/// 渠道在某个池里的成员关系：覆盖为 None 时继承渠道 / key 自身的调度参数。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolMember {
    pub pool_code: String,
    #[serde(default)]
    pub priority_override: Option<i32>,
    #[serde(default)]
    pub weight_override: Option<i32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChannelKeyRow {
    pub id: i64,
    pub channel_id: i64,
    pub status: i16,
    pub failed_count: i32,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// 加权随机的权重（多 key 渠道的核心调度参数，管理端需可见可调）。
    pub weight: i32,
    pub max_concurrency: Option<i32>,
    /// 0 = static_key，1 = oauth_refresh（§11.38；列表据此决定是否解出到期时间）。
    pub credential_kind: i16,
    /// 固定分配组分给这把 key 的代理（§11.41）；None = 未分配 / 非固定分配。
    pub egress_proxy_id: Option<i64>,
}

/// 渠道列表切片 + 过滤集内的启用数（列表页头"共 N 条 · M 启用"不必再拉全量数）。
#[derive(Debug)]
pub struct ChannelList {
    pub page: crate::listing::Page<ChannelRow>,
    pub enabled: i64,
}

/// 渠道列表过滤条件；`Default` = 不过滤。
/// 四个字段里两个同为 `Option<&str>`，用结构体而非位置参数，换位不会静默编译通过。
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelFilter<'a> {
    /// own 范围：只看该属主的渠道。
    pub owner: Option<i64>,
    /// 名称 / 地址关键词（ILIKE，通配符由 `like_pattern` 转义）。
    pub query: Option<&'a str>,
    /// 协议精确匹配。
    pub provider: Option<&'a str>,
    /// 状态精确匹配。
    pub status: Option<i16>,
}

/// 渠道列表；`filter` 的四个维度全部落在 SQL 里，`slice` 不分页时回全量。
/// `ChannelList::enabled` 与 `Page::total` 都是过滤集内的计数。
pub async fn list_channels(
    pool: &PgPool,
    filter: ChannelFilter<'_>,
    slice: crate::listing::Slice,
) -> Result<ChannelList, StoreError> {
    let ChannelFilter {
        owner,
        query,
        provider,
        status,
    } = filter;
    let pattern = crate::listing::like_pattern(query);
    let (rows, counts) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT c.id, c.name, c.provider, c.api_base, c.status, c.priority, c.models,
               c.trust_upstream_usage, c.owner_id,
               COALESCE(c.settings, '{}'::jsonb) AS "settings!",
               GREATEST(COALESCE((c.upstream_unit_cost ->> 'relative_cost_milli')::bigint, 1000), 0) AS "cost_milli!",
               c.egress_mode, c.egress_proxy_id, c.egress_group_code,
               COALESCE(
                   (SELECT array_agg(pc.pool_code ORDER BY pc.pool_code)
                      FROM pool_channels pc WHERE pc.channel_id = c.id),
                   ARRAY[]::varchar[]
               ) AS "pools!",
               COALESCE(
                   (SELECT jsonb_agg(jsonb_build_object(
                               'pool_code', pc.pool_code,
                               'priority_override', pc.priority_override,
                               'weight_override', pc.weight_override
                           ) ORDER BY pc.pool_code)
                      FROM pool_channels pc WHERE pc.channel_id = c.id),
                   '[]'::jsonb
               ) AS "pool_members!"
        FROM channels c
        WHERE c.deleted_at IS NULL AND ($1::bigint IS NULL OR c.owner_id = $1)
          AND ($2::text IS NULL OR c.name ILIKE $2 OR c.api_base ILIKE $2)
          AND ($3::text IS NULL OR c.provider = $3)
          AND ($4::smallint IS NULL OR c.status = $4)
        ORDER BY c.priority DESC, c.id
        LIMIT $5 OFFSET $6
        "#,
            owner,
            pattern.as_deref(),
            provider,
            status,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        crate::listing::count_unless_all(
            slice,
            sqlx::query!(
                r#"SELECT COUNT(*) AS "total!", COUNT(*) FILTER (WHERE c.status = 1) AS "enabled!"
           FROM channels c
           WHERE c.deleted_at IS NULL AND ($1::bigint IS NULL OR c.owner_id = $1)
             AND ($2::text IS NULL OR c.name ILIKE $2 OR c.api_base ILIKE $2)
             AND ($3::text IS NULL OR c.provider = $3)
             AND ($4::smallint IS NULL OR c.status = $4)"#,
                owner,
                pattern.as_deref(),
                provider,
                status
            )
            .fetch_one(pool)
        ),
    )?;
    let (total, enabled) = counts.map_or_else(
        || {
            (
                crate::listing::len_as_total(rows.len()),
                crate::listing::len_as_total(rows.iter().filter(|r| r.status == 1).count()),
            )
        },
        |c| (c.total, c.enabled),
    );
    let data = rows
        .into_iter()
        .map(|r| ChannelRow {
            id: r.id,
            name: r.name,
            provider: r.provider,
            api_base: r.api_base,
            status: r.status,
            priority: r.priority,
            models: r.models,
            trust_upstream_usage: r.trust_upstream_usage,
            owner_id: r.owner_id,
            settings: r.settings,
            pools: r.pools,
            pool_members: serde_json::from_value(r.pool_members).unwrap_or_default(),
            cost_milli: r.cost_milli,
            egress_mode: r.egress_mode,
            egress_proxy_id: r.egress_proxy_id,
            egress_group_code: r.egress_group_code,
        })
        .collect();
    Ok(ChannelList {
        page: crate::listing::Page { data, total },
        enabled,
    })
}

/// 渠道相对成本系数（千分比；缺省 1000 = 官方标价）。None = 渠道不存在。
/// 结算路径经进程缓存调用（`AppState::channel_cost_milli`），不在热路径。
pub async fn channel_cost_milli(pool: &PgPool, channel_id: i64) -> Result<Option<i64>, StoreError> {
    let row = sqlx::query_scalar!(
        r#"SELECT GREATEST(COALESCE((upstream_unit_cost ->> 'relative_cost_milli')::bigint, 1000), 0) AS "cost_milli!"
           FROM channels WHERE id = $1"#,
        channel_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 渠道属主（外层 None = 渠道不存在）。
pub async fn channel_owner(
    pool: &PgPool,
    channel_id: i64,
) -> Result<Option<Option<i64>>, StoreError> {
    let row = sqlx::query!(
        r#"SELECT owner_id FROM channels WHERE id = $1 AND deleted_at IS NULL"#,
        channel_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.owner_id))
}

/// 属主传播（渠道创建人，#6267）。
pub async fn set_channel_owner(
    pool: &PgPool,
    channel_id: i64,
    owner_id: i64,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"UPDATE channels SET owner_id = $2 WHERE id = $1"#,
        channel_id,
        owner_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 覆盖式设置该渠道所属的池（空数组 = 从所有池移出 → 孤儿，对谁都不可达）。
///
/// 从渠道这一侧维护成员关系：管理员的动作通常是"这个渠道给哪些档位用"，
/// 而不是逐个池去挑渠道。成员级覆盖随成员关系一起写：同一渠道在 stable 池当主力、
/// 在 fast 池当备胎，靠的就是这两列。
pub async fn set_channel_pools(
    pool: &PgPool,
    channel_id: i64,
    members: &[PoolMember],
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    // Serialize replacement even when the old membership set is empty.
    sqlx::query("SELECT id FROM channels WHERE id=$1 FOR UPDATE")
        .bind(channel_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query!(
        r#"DELETE FROM pool_channels WHERE channel_id = $1"#,
        channel_id
    )
    .execute(&mut *tx)
    .await?;
    for m in members {
        sqlx::query!(
            r#"
            INSERT INTO pool_channels (pool_code, channel_id, priority_override, weight_override)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (pool_code, channel_id) DO UPDATE SET
                priority_override = EXCLUDED.priority_override,
                weight_override   = EXCLUDED.weight_override
            "#,
            m.pool_code,
            channel_id,
            m.priority_override,
            m.weight_override
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 便捷形式：只给池码、不带覆盖（测试与迁移器用）。
pub async fn set_channel_pool_codes(
    pool: &PgPool,
    channel_id: i64,
    pools: &[String],
) -> Result<(), StoreError> {
    let members: Vec<PoolMember> = pools
        .iter()
        .map(|p| PoolMember {
            pool_code: p.clone(),
            priority_override: None,
            weight_override: None,
        })
        .collect();
    set_channel_pools(pool, channel_id, &members).await
}

/// 渠道池 upsert。策略取值由库 CHECK 兜底，非法值回 400 而不是写坏数据。
/// `fallback_pool_code`：本池无候选时退到的池（单跳；自引用由 CHECK 拒绝）。
pub async fn upsert_channel_pool(
    pool: &PgPool,
    pool_code: &str,
    description: &str,
    routing_strategy: &str,
    fallback_pool_code: Option<&str>,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"
        INSERT INTO channel_pools (pool_code, description, routing_strategy, fallback_pool_code)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (pool_code) DO UPDATE SET
            description        = EXCLUDED.description,
            routing_strategy   = EXCLUDED.routing_strategy,
            fallback_pool_code = EXCLUDED.fallback_pool_code,
            updated_at         = now()
        "#,
        pool_code,
        description,
        routing_strategy,
        fallback_pool_code
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 全站渠道 key（健康汇总这类整表调用方用）。列表页翻页请走 [`list_channel_keys_for`]。
pub async fn list_channel_keys(pool: &PgPool) -> Result<Vec<ChannelKeyRow>, StoreError> {
    let rows = sqlx::query_as!(
        ChannelKeyRow,
        r#"
        SELECT id, channel_id, status, failed_count, cooldown_until, last_error,
               weight, max_concurrency, credential_kind, egress_proxy_id
        FROM channel_keys ORDER BY channel_id, id
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 指定渠道的 key：渠道列表已按页切片，key 也只取本页那几条，不再整表拉回内存过滤。
pub async fn list_channel_keys_for(
    pool: &PgPool,
    channel_ids: &[i64],
) -> Result<Vec<ChannelKeyRow>, StoreError> {
    if channel_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as!(
        ChannelKeyRow,
        r#"
        SELECT id, channel_id, status, failed_count, cooldown_until, last_error,
               weight, max_concurrency, credential_kind, egress_proxy_id
        FROM channel_keys WHERE channel_id = ANY($1) ORDER BY channel_id, id
        "#,
        channel_ids
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 渠道启停（1=启用 2=手动停用）。返回是否命中行。
pub async fn set_channel_status(
    pool: &PgPool,
    channel_id: i64,
    status: i16,
) -> Result<bool, StoreError> {
    let result = sqlx::query!(
        r#"UPDATE channels SET status = $2, updated_at = now() WHERE id = $1 AND deleted_at IS NULL"#,
        channel_id,
        status
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

// ---- 渠道编辑 ----

/// channels 可写字段补丁（`None` = 不改）。
///
/// 这些列都是 NOT NULL 或语义上不该被清空，故只需两态；
/// 与 `ApiKeyPatch` 的三态不同是因为那边"解除限制"必须能表达。
#[derive(Debug, Default, Clone, Copy)]
pub struct ChannelPatch<'a> {
    pub name: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub api_base: Option<&'a str>,
    pub models: Option<&'a serde_json::Value>,
    pub model_mapping: Option<&'a serde_json::Value>,
    pub settings: Option<&'a serde_json::Value>,
    pub capabilities: Option<&'a serde_json::Value>,
    pub priority: Option<i32>,
    pub trust_upstream_usage: Option<bool>,
    /// 相对成本系数（千分比）；写入 `upstream_unit_cost.relative_cost_milli`，其余键保留。
    pub cost_milli: Option<i64>,
}

/// 部分更新渠道配置；返回是否命中（false = 不存在/已删除，调用方转 404）。
pub async fn patch_channel(
    pool: &PgPool,
    channel_id: i64,
    patch: ChannelPatch<'_>,
) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"
        UPDATE channels SET
            name                 = COALESCE($2, name),
            provider             = COALESCE($3, provider),
            api_base             = COALESCE($4, api_base),
            models               = COALESCE($5::jsonb, models),
            model_mapping        = COALESCE($6::jsonb, model_mapping),
            settings             = COALESCE($7::jsonb, settings),
            capabilities         = COALESCE($8::jsonb, capabilities),
            priority             = COALESCE($9, priority),
            trust_upstream_usage = COALESCE($10, trust_upstream_usage),
            upstream_unit_cost   = CASE WHEN $11::bigint IS NULL THEN upstream_unit_cost
                                        ELSE COALESCE(upstream_unit_cost, '{}'::jsonb)
                                             || jsonb_build_object('relative_cost_milli', $11::bigint) END,
            updated_at           = now()
        WHERE id = $1 AND deleted_at IS NULL
        "#,
        channel_id,
        patch.name,
        patch.provider,
        patch.api_base,
        patch.models,
        patch.model_mapping,
        patch.settings,
        patch.capabilities,
        patch.priority,
        patch.trust_upstream_usage,
        patch.cost_milli,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 渠道单把 key 的权重/并发上限调整；返回是否命中。
/// 调单把 key 的调度参数与状态。
///
/// `status` 同时复位 `failed_count / cooldown_until / last_error`——手动改状态的语义就是
/// 「我处理过了，重新开始计数」。此前没有这个参数：401/403 把 key 打成 `status=6`
/// （无冷却、不自愈）之后，冷却恢复 worker 只捞 `status IN (2,3,4)`，控制面又没有别的入口，
/// 唯一救法是重置凭证。
pub async fn patch_channel_key(
    pool: &PgPool,
    channel_id: i64,
    channel_key_id: i64,
    weight: Option<i32>,
    max_concurrency: Option<Option<i32>>,
    status: Option<i16>,
) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"
        UPDATE channel_keys SET
            weight          = COALESCE($3, weight),
            max_concurrency = CASE WHEN $4::bool THEN $5::int ELSE max_concurrency END,
            status          = COALESCE($6, status),
            failed_count    = CASE WHEN $6::smallint IS NULL THEN failed_count ELSE 0 END,
            cooldown_until  = CASE WHEN $6::smallint IS NULL THEN cooldown_until ELSE NULL END,
            last_error      = CASE WHEN $6::smallint IS NULL THEN last_error ELSE NULL END,
            updated_at      = now()
        WHERE id = $2 AND channel_id = $1
        "#,
        channel_id,
        channel_key_id,
        weight,
        max_concurrency.is_some(),
        max_concurrency.flatten(),
        status,
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 凭证轮换结果。多把 key 的渠道必须显式指定 `channel_key_id`——
/// 静默全量覆盖会把一次换 key 变成把整条渠道的凭证写成同一把。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotateOutcome {
    Rotated(i64),
    NotFound,
    Ambiguous,
}

/// 轮换渠道凭证，并复位 key 状态机（active / 清冷却 / 清失败计数）。
/// 复位是必需的：换凭证的场景多半是原凭证已被上游封停打进 invalid(6)，
/// 不复位则新凭证仍被状态机排除在候选之外。
pub async fn rotate_channel_credential(
    pool: &PgPool,
    channel_id: i64,
    channel_key_id: Option<i64>,
    credential: &str,
    master_key: Option<&str>,
) -> Result<RotateOutcome, StoreError> {
    // 未指定 key 时只在"恰好一把"的情况下自动选定；多把则要求显式指定，
    // 避免把凭证轮换到运维预期之外的那把 key 上
    let target = if let Some(id) = channel_key_id {
        id
    } else {
        let ids = sqlx::query_scalar!(
            r#"SELECT id FROM channel_keys WHERE channel_id = $1 ORDER BY id LIMIT 2"#,
            channel_id
        )
        .fetch_all(pool)
        .await?;
        match ids.as_slice() {
            [] => return Ok(RotateOutcome::NotFound),
            [only] => *only,
            _ => return Ok(RotateOutcome::Ambiguous),
        }
    };
    let hit = sqlx::query_scalar::<_, i64>(
        r"
        UPDATE channel_keys SET
            credential_ciphertext = $3, credential_kind = $4,
            status = 1, cooldown_until = NULL, failed_count = 0, last_error = NULL,
            updated_at = now()
        WHERE id = $2 AND channel_id = $1
        RETURNING id
        ",
    )
    .bind(channel_id)
    .bind(target)
    .bind(crate::credential::seal_or_plain(master_key, credential)?)
    .bind(i16::from(
        crate::credential::OAuthCredential::parse(credential).is_some(),
    ))
    .fetch_optional(pool)
    .await?;
    Ok(hit.map_or(RotateOutcome::NotFound, RotateOutcome::Rotated))
}

/// 读一把 key 的当前凭证明文（OAuth 刷新四步锁的"加锁后重读"，IMPLEMENTATION §11.38）。
/// None = key 不存在。
pub async fn read_key_credential(
    pool: &PgPool,
    channel_key_id: i64,
    master_key: Option<&str>,
) -> Result<Option<String>, StoreError> {
    let row = sqlx::query_scalar!(
        r#"SELECT credential_ciphertext FROM channel_keys WHERE id = $1"#,
        channel_key_id
    )
    .fetch_optional(pool)
    .await?;
    row.map(|c| crate::credential::open(master_key, &c))
        .transpose()
}

/// 静默回写刷新后的凭证：**不动**状态机（与 `rotate_channel_credential` 相反）——
/// 刷新是例行动作，不该把一把正在冷却的 key 顺手复位成 active。
pub async fn write_key_credential(
    pool: &PgPool,
    channel_key_id: i64,
    credential: &str,
    master_key: Option<&str>,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"UPDATE channel_keys SET credential_ciphertext = $2, updated_at = now() WHERE id = $1"#,
        channel_key_id,
        crate::credential::seal_or_plain(master_key, credential)?
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// `channel_keys.credential_kind`：1 = OAuth, including manually imported access tokens.
pub const CREDENTIAL_KIND_OAUTH: i16 = 1;
