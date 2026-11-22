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
    /// 请求模型 → 上游模型映射（回显以防编辑表单静默清空）。
    pub model_mapping: serde_json::Value,
    /// 渠道能力声明（tools / vision 等；回显以防编辑表单静默清空）。
    pub capabilities: serde_json::Value,
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
    /// 管理端列表置顶时间；非空即置顶，按置顶时间倒序排在最前，不影响路由调度优先级。
    pub pinned_at: Option<DateTime<Utc>>,
}

fn default_member_role() -> String {
    "primary".to_owned()
}

fn default_true() -> bool {
    true
}

/// 渠道在某个池里的成员关系：覆盖为 None 时继承渠道 / key 自身的调度参数。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolMember {
    pub pool_code: String,
    #[serde(default)]
    pub priority_override: Option<i32>,
    #[serde(default)]
    pub weight_override: Option<i32>,
    /// 成员角色：`primary`（默认主力）| `backup`（兜底）| `canary`（灰度）。
    #[serde(default = "default_member_role")]
    pub role: String,
    /// 在该池内是否参与调度（false = 仅在本池摘流，不影响渠道在其他池的状态）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 成员标签（供池护栏 `required_tags / excluded_tags` 匹配）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 成员在该池内的模型通配过滤（None = 继承渠道全部模型）。
    #[serde(default)]
    pub model_filter: Option<Vec<String>>,
}

impl Default for PoolMember {
    fn default() -> Self {
        Self {
            pool_code: String::new(),
            priority_override: None,
            weight_override: None,
            role: default_member_role(),
            enabled: true,
            tags: Vec::new(),
            model_filter: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
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
    /// key 级模型子集（null = 继承渠道全部模型）。
    pub model_subset: Option<serde_json::Value>,
    /// key 级每分钟请求上限（null = 不限）。
    pub rpm_limit: Option<i32>,
    /// key 级每日消费上限 micro-USD（null = 不限）。
    pub daily_spend_cap_micro: Option<i64>,
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

#[derive(sqlx::FromRow)]
struct ChannelRowDb {
    id: i64,
    name: String,
    provider: String,
    api_base: Option<String>,
    status: i16,
    priority: i32,
    models: serde_json::Value,
    model_mapping: serde_json::Value,
    capabilities: serde_json::Value,
    trust_upstream_usage: bool,
    owner_id: Option<i64>,
    settings: serde_json::Value,
    cost_milli: i64,
    egress_mode: Option<String>,
    egress_proxy_id: Option<i64>,
    egress_group_code: Option<String>,
    pinned_at: Option<DateTime<Utc>>,
    pools: Vec<String>,
    pool_members: serde_json::Value,
}

impl From<ChannelRowDb> for ChannelRow {
    fn from(r: ChannelRowDb) -> Self {
        Self {
            id: r.id,
            name: r.name,
            provider: r.provider,
            api_base: r.api_base,
            status: r.status,
            priority: r.priority,
            models: r.models,
            model_mapping: r.model_mapping,
            capabilities: r.capabilities,
            trust_upstream_usage: r.trust_upstream_usage,
            owner_id: r.owner_id,
            settings: r.settings,
            pools: r.pools,
            pool_members: serde_json::from_value(r.pool_members).unwrap_or_default(),
            cost_milli: r.cost_milli,
            egress_mode: r.egress_mode,
            egress_proxy_id: r.egress_proxy_id,
            egress_group_code: r.egress_group_code,
            pinned_at: r.pinned_at,
        }
    }
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
        sqlx::query_as::<_, ChannelRowDb>(
            r"
        SELECT c.id, c.name, c.provider, c.api_base, c.status, c.priority, c.models,
               COALESCE(c.model_mapping, '{}'::jsonb) AS model_mapping,
               COALESCE(c.capabilities, '{}'::jsonb) AS capabilities,
               c.trust_upstream_usage, c.owner_id,
               COALESCE(c.settings, '{}'::jsonb) AS settings,
               GREATEST(COALESCE((c.upstream_unit_cost ->> 'relative_cost_milli')::bigint, 1000), 0) AS cost_milli,
               c.egress_mode, c.egress_proxy_id, c.egress_group_code, c.pinned_at,
               COALESCE(
                   (SELECT array_agg(pc.pool_code ORDER BY pc.pool_code)
                      FROM pool_channels pc WHERE pc.channel_id = c.id),
                   ARRAY[]::varchar[]
               ) AS pools,
               COALESCE(
                   (SELECT jsonb_agg(jsonb_build_object(
                               'pool_code', pc.pool_code,
                               'priority_override', pc.priority_override,
                               'weight_override', pc.weight_override,
                               'role', pc.role,
                               'enabled', pc.enabled,
                               'tags', pc.tags,
                               'model_filter', pc.model_filter
                           ) ORDER BY pc.pool_code)
                      FROM pool_channels pc WHERE pc.channel_id = c.id),
                   '[]'::jsonb
               ) AS pool_members
        FROM channels c
        WHERE c.deleted_at IS NULL AND ($1::bigint IS NULL OR c.owner_id = $1)
          AND ($2::text IS NULL OR c.name ILIKE $2 OR c.api_base ILIKE $2)
          AND ($3::text IS NULL OR c.provider = $3)
          AND ($4::smallint IS NULL OR c.status = $4)
        ORDER BY (c.pinned_at IS NOT NULL) DESC, c.pinned_at DESC NULLS LAST, c.priority DESC, c.id
        LIMIT $5 OFFSET $6
        ",
        )
        .bind(owner)
        .bind(pattern.as_deref())
        .bind(provider)
        .bind(status)
        .bind(slice.limit)
        .bind(slice.offset)
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
    let data = rows.into_iter().map(ChannelRow::from).collect();
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
        let role = if m.role.is_empty() {
            "primary"
        } else {
            m.role.as_str()
        };
        let tags_json = serde_json::to_value(&m.tags).unwrap_or_else(|_| serde_json::json!([]));
        let filter_json = m
            .model_filter
            .as_ref()
            .map(|f| serde_json::to_value(f).unwrap_or_else(|_| serde_json::json!([])));
        sqlx::query(
            r"
            INSERT INTO pool_channels
                (pool_code, channel_id, priority_override, weight_override, role, enabled, tags, model_filter)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (pool_code, channel_id) DO UPDATE SET
                priority_override = EXCLUDED.priority_override,
                weight_override   = EXCLUDED.weight_override,
                role              = EXCLUDED.role,
                enabled           = EXCLUDED.enabled,
                tags              = EXCLUDED.tags,
                model_filter      = EXCLUDED.model_filter
            ",
        )
        .bind(&m.pool_code)
        .bind(channel_id)
        .bind(m.priority_override)
        .bind(m.weight_override)
        .bind(role)
        .bind(m.enabled)
        .bind(tags_json)
        .bind(filter_json)
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
            ..PoolMember::default()
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
    let rows = sqlx::query_as::<_, ChannelKeyRow>(
        r"
        SELECT id, channel_id, status, failed_count, cooldown_until, last_error,
               weight, max_concurrency, model_subset, rpm_limit, daily_spend_cap_micro,
               credential_kind, egress_proxy_id
        FROM channel_keys ORDER BY channel_id, id
        ",
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
    let rows = sqlx::query_as::<_, ChannelKeyRow>(
        r"
        SELECT id, channel_id, status, failed_count, cooldown_until, last_error,
               weight, max_concurrency, model_subset, rpm_limit, daily_spend_cap_micro,
               credential_kind, egress_proxy_id
        FROM channel_keys WHERE channel_id = ANY($1) ORDER BY channel_id, id
        ",
    )
    .bind(channel_ids)
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

/// 渠道列表置顶开关：pinned = true 写入当前时间（最新置顶排最前），false 清空。
/// 外层 None = 渠道不存在或已删除；内层 Option = 写入后的 `pinned_at`。
pub async fn set_channel_pinned(
    pool: &PgPool,
    channel_id: i64,
    pinned: bool,
) -> Result<Option<Option<DateTime<Utc>>>, StoreError> {
    let row: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
        r"UPDATE channels
          SET pinned_at = CASE WHEN $2 THEN now() ELSE NULL END,
              updated_at = now()
          WHERE id = $1 AND deleted_at IS NULL
          RETURNING pinned_at",
    )
    .bind(channel_id)
    .bind(pinned)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(ts,)| ts))
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

/// 渠道单把 key 的三态可写补丁（None = 不改；Some(None) = 清空限额/子集）。
#[derive(Debug, Default, Clone)]
pub struct ChannelKeyPatch {
    pub weight: Option<i32>,
    pub max_concurrency: Option<Option<i32>>,
    pub status: Option<i16>,
    pub model_subset: Option<Option<serde_json::Value>>,
    pub rpm_limit: Option<Option<i32>>,
    pub daily_spend_cap_micro: Option<Option<i64>>,
}

/// 调单把 key 的调度参数、限流护栏与状态。
///
/// `status` 同时复位 `failed_count / cooldown_until / last_error`——手动改状态的语义就是
/// 「我处理过了，重新开始计数」。
pub async fn patch_channel_key(
    pool: &PgPool,
    channel_id: i64,
    channel_key_id: i64,
    patch: &ChannelKeyPatch,
) -> Result<bool, StoreError> {
    let affected = sqlx::query(
        r"
        UPDATE channel_keys SET
            weight                = COALESCE($3, weight),
            max_concurrency       = CASE WHEN $4::bool THEN $5::int ELSE max_concurrency END,
            status                = COALESCE($6::smallint, status),
            failed_count          = CASE WHEN $6::smallint IS NULL THEN failed_count ELSE 0 END,
            cooldown_until        = CASE WHEN $6::smallint IS NULL THEN cooldown_until ELSE NULL END,
            last_error            = CASE WHEN $6::smallint IS NULL THEN last_error ELSE NULL END,
            model_subset          = CASE WHEN $7::bool THEN $8::jsonb ELSE model_subset END,
            rpm_limit             = CASE WHEN $9::bool THEN $10::int ELSE rpm_limit END,
            daily_spend_cap_micro = CASE WHEN $11::bool THEN $12::bigint ELSE daily_spend_cap_micro END,
            updated_at            = now()
        WHERE id = $2 AND channel_id = $1
        ",
    )
    .bind(channel_id)
    .bind(channel_key_id)
    .bind(patch.weight)
    .bind(patch.max_concurrency.is_some())
    .bind(patch.max_concurrency.flatten())
    .bind(patch.status)
    .bind(patch.model_subset.is_some())
    .bind(patch.model_subset.as_ref().and_then(Option::as_ref))
    .bind(patch.rpm_limit.is_some())
    .bind(patch.rpm_limit.flatten())
    .bind(patch.daily_spend_cap_micro.is_some())
    .bind(patch.daily_spend_cap_micro.flatten())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 向已有渠道追加一把新 key 的输入参数。
pub struct NewChannelKey<'a> {
    pub credential: &'a str,
    pub weight: i32,
    pub max_concurrency: Option<i32>,
    pub model_subset: Option<&'a serde_json::Value>,
    pub rpm_limit: Option<i32>,
    pub daily_spend_cap_micro: Option<i64>,
}

/// 向已有渠道追加一把新 key（带出口组固定分配协调）；返回 None = 渠道不存在/已删。
pub async fn add_channel_key(
    pool: &PgPool,
    channel_id: i64,
    input: NewChannelKey<'_>,
    master_key: Option<&str>,
) -> Result<Option<i64>, StoreError> {
    let mut tx = pool.begin().await?;
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM channels WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(channel_id)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Ok(None);
    }
    let sealed = crate::credential::seal_or_plain(master_key, input.credential)?;
    let kind = i16::from(crate::credential::OAuthCredential::parse(input.credential).is_some());
    let key_id: i64 = sqlx::query_scalar(
        r"INSERT INTO channel_keys (
              channel_id, credential_ciphertext, credential_kind, weight,
              max_concurrency, model_subset, rpm_limit, daily_spend_cap_micro, status
          )
          VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1)
          RETURNING id",
    )
    .bind(channel_id)
    .bind(sealed)
    .bind(kind)
    .bind(input.weight)
    .bind(input.max_concurrency)
    .bind(input.model_subset)
    .bind(input.rpm_limit)
    .bind(input.daily_spend_cap_micro)
    .fetch_one(&mut *tx)
    .await?;
    crate::egress::reconcile(&mut tx).await?;
    tx.commit().await?;
    Ok(Some(key_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteChannelKeyOutcome {
    Deleted,
    NotFound,
    LastKey,
}

/// 从多 key 渠道移除指定 key（禁止删空最后一把，避免留下无凭证的僵尸渠道）。
pub async fn delete_channel_key(
    pool: &PgPool,
    channel_id: i64,
    channel_key_id: i64,
) -> Result<DeleteChannelKeyOutcome, StoreError> {
    let mut tx = pool.begin().await?;
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM channels WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(channel_id)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Ok(DeleteChannelKeyOutcome::NotFound);
    }
    let key_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM channel_keys WHERE channel_id = $1")
        .bind(channel_id)
        .fetch_all(&mut *tx)
        .await?;
    if !key_ids.contains(&channel_key_id) {
        return Ok(DeleteChannelKeyOutcome::NotFound);
    }
    if key_ids.len() <= 1 {
        return Ok(DeleteChannelKeyOutcome::LastKey);
    }
    sqlx::query("DELETE FROM channel_keys WHERE id = $1 AND channel_id = $2")
        .bind(channel_key_id)
        .bind(channel_id)
        .execute(&mut *tx)
        .await?;
    crate::egress::reconcile(&mut tx).await?;
    tx.commit().await?;
    Ok(DeleteChannelKeyOutcome::Deleted)
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
