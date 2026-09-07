//! 管理面只读列表查询（IMPLEMENTATION §11.6 接口面清单）。
//!
//! 与 `admin` 模块分工：`admin` 放写操作与既有单点查询；本模块集中放**分页列表**，
//! 这些查询共性强（limit/offset + 总数 + 关键词 + 占用计数），集中便于统一护栏。
//!
//! 护栏（PG 只做点查与账本，看板聚合走 CH——见 00-project 红线）：
//! - 切片一律经 `Slice` 钳制上限，防管理端误传超大 limit 拖垮 PG；大表（令牌 / 兑换码）
//!   走 `Slice::capped_limit`，不存在"回全量"；
//! - 数据与 COUNT 两条查询 `try_join!` 并行；配置类列表全量取（`Slice::is_all`）时
//!   干脆不发 COUNT，`total = data.len()`（见 `count_unless_all`）；
//! - 只回配置与账本事实，不做跨表聚合统计（统计一律走 `ch` 模块 + 物化视图）；
//! - 列表附带"占用计数"，供删除前的引用检查（见 `mutate` 的 Conflict 语义）。

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

/// 单页上限。
pub const MAX_PAGE: i64 = 200;

/// 列表切片（limit ∈ [1, MAX_PAGE]，offset ≥ 0）。
///
/// 配置类列表：`limit = None` 回全量——下拉选项、全量校验这类调用方不分页，
/// 列表页才传 limit。大表（令牌 / 兑换码 / 用户）取 [`Slice::capped_limit`]：
/// 不传 limit 也封顶到 `MAX_PAGE`。无论切不切，`Page::total` 都是过滤后的真实总数，
/// 前端翻页器据此画页码。
#[derive(Debug, Clone, Copy, Default)]
pub struct Slice {
    pub limit: Option<i64>,
    pub offset: i64,
}

impl Slice {
    /// 全量（不分页）。
    pub const ALL: Self = Self {
        limit: None,
        offset: 0,
    };

    #[must_use]
    pub fn new(limit: Option<i64>, offset: i64) -> Self {
        Self {
            limit: limit.map(|l| l.clamp(1, MAX_PAGE)),
            offset: offset.max(0),
        }
    }

    /// 大表专用的 limit：调用方没传也封顶到 `MAX_PAGE`，令牌 / 兑换码这类表不存在"回全量"。
    #[must_use]
    pub fn capped_limit(self) -> i64 {
        self.limit.unwrap_or(MAX_PAGE)
    }

    /// 全量且从头取：此时 `data.len()` 就是 total，COUNT 那一遍可以省掉。
    #[must_use]
    pub fn is_all(self) -> bool {
        self.limit.is_none() && self.offset == 0
    }
}

/// 计数查询按需执行：全量切片回 `None`（调用方用 `data.len()` 补 total），
/// 否则跑 `count`。与数据查询放进同一个 `try_join!` 即可并行——两条各占一个连接，
/// 管理面列表的规模下换掉一次串行 RTT 是划算的。
pub async fn count_unless_all<F, T>(slice: Slice, count: F) -> Result<Option<T>, sqlx::Error>
where
    F: Future<Output = Result<T, sqlx::Error>>,
{
    if slice.is_all() {
        Ok(None)
    } else {
        count.await.map(Some)
    }
}

/// 行数 → `total`。列表长度不可能超过 i64，转换失败只在不变量被破坏时出现，饱和处理即可。
#[must_use]
pub fn len_as_total(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

/// 关键词 → ILIKE 模式；转义 `%` `_` 防调用方注入通配符。
/// 所有走 `ILIKE $n` 的列表（含 console 层直写 SQL 的用户列表）都应经它，避免搜索词里的
/// 下划线被当成单字符通配。
#[must_use]
pub fn like_pattern(query: Option<&str>) -> Option<String> {
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
    pub audio_ratio: Option<String>,
    pub audio_completion_ratio: Option<String>,
    pub image_ratio: Option<String>,
    pub per_call_price_micro: Option<i64>,
    pub tier_expr: Option<String>,
    pub tier_ratios: Option<serde_json::Value>,
    /// 模型级降级链（零候选时按序改投，DESIGN §3.4.1）。
    pub fallback_models: Vec<String>,
}

/// 模型列表切片 + 过滤集内的未定价数（模型页"只看未定价 (N)"按钮不必再拉全量数）。
#[derive(Debug)]
pub struct ModelList {
    pub page: Page<ModelListRow>,
    pub unpriced: i64,
}

/// 模型配置列表（含定价四轴）。倍率以 `::text` 出库保精度，展示层不做浮点运算。
/// LEFT JOIN 以暴露"未定价模型"；`unpriced` 只看这些配了一半的；`query` 匹配模型名 / 展示名。
pub async fn list_models(
    pool: &PgPool,
    query: Option<&str>,
    unpriced: bool,
    slice: Slice,
) -> Result<ModelList, StoreError> {
    let pattern = like_pattern(query);
    // SQL 文本保持原缩进：sqlx 离线快照按字面哈希，动一个空格就要重新 prepare
    let (rows, counts) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT m.model_name, m.display_name, m.vendor, m.status, m.sort_order,
               m.capabilities, m.context_window, m.fallback_models,
               p.pricing_mode            AS "pricing_mode?",
               p.model_ratio::text       AS model_ratio,
               p.completion_ratio::text  AS completion_ratio,
               p.cache_ratio::text       AS cache_ratio,
               p.cache_write_ratio::text AS cache_write_ratio,
               p.audio_ratio::text            AS audio_ratio,
               p.audio_completion_ratio::text AS audio_completion_ratio,
               p.image_ratio::text            AS image_ratio,
               p.per_call_price_micro, p.tier_expr, p.tier_ratios
        FROM models m
        LEFT JOIN model_pricing p ON p.model_id = m.id
        WHERE ($1::text IS NULL OR m.model_name ILIKE $1 OR m.display_name ILIKE $1)
          AND (NOT $2::boolean OR p.pricing_mode IS NULL)
        ORDER BY m.sort_order, m.model_name
        LIMIT $3 OFFSET $4
        "#,
            pattern.as_deref(),
            unpriced,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        count_unless_all(
            slice,
            sqlx::query!(
                r#"SELECT COUNT(*) AS "total!",
                  COUNT(*) FILTER (WHERE p.pricing_mode IS NULL) AS "unpriced!"
           FROM models m LEFT JOIN model_pricing p ON p.model_id = m.id
           WHERE ($1::text IS NULL OR m.model_name ILIKE $1 OR m.display_name ILIKE $1)
             AND (NOT $2::boolean OR p.pricing_mode IS NULL)"#,
                pattern.as_deref(),
                unpriced
            )
            .fetch_one(pool)
        ),
    )?;
    // 全量时两个计数直接数手里的行
    let (total, unpriced) = counts.map_or_else(
        || {
            (
                len_as_total(rows.len()),
                len_as_total(rows.iter().filter(|r| r.pricing_mode.is_none()).count()),
            )
        },
        |c| (c.total, c.unpriced),
    );
    let data = rows
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
            audio_ratio: r.audio_ratio,
            audio_completion_ratio: r.audio_completion_ratio,
            image_ratio: r.image_ratio,
            per_call_price_micro: r.per_call_price_micro,
            tier_expr: r.tier_expr,
            tier_ratios: r.tier_ratios,
            fallback_models: serde_json::from_value(r.fallback_models).unwrap_or_default(),
        })
        .collect();
    Ok(ModelList {
        page: Page { data, total },
        unpriced,
    })
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
    /// 该分组的池（分组必有池，缺省 default）。
    pub pool_code: String,
    /// 该分组的池内渠道数。
    pub channel_count: i64,
    /// 用户可否在门户为自己的 key 自选此分组。
    pub self_select: bool,
    /// 分组内每用户分钟 / 小时请求上限（§11.32）；None = 不限。
    pub rpm_limit: Option<i32>,
    pub rph_limit: Option<i32>,
}

/// 渠道池列表行。
pub struct PoolListRow {
    pub pool_code: String,
    pub description: Option<String>,
    pub routing_strategy: String,
    /// 本池无候选时退到的池（单跳）。
    pub fallback_pool_code: Option<String>,
    /// 池内渠道数。
    pub channel_count: i64,
    /// 引用该池的分组数 + 令牌数 + 把它当降级目标的池数（>0 时删除会被拒）。
    pub group_count: i64,
    pub key_count: i64,
    pub fallback_ref_count: i64,
}

/// 池详情的一个成员渠道。
#[derive(Debug, Serialize)]
pub struct PoolMemberRow {
    pub channel_id: i64,
    pub name: String,
    pub provider: String,
    pub status: i16,
    pub priority: i32,
    pub priority_override: Option<i32>,
    pub weight_override: Option<i32>,
    pub models: Vec<String>,
    /// 该渠道当前可用 key 数（status=1 且不在冷却）。
    pub active_keys: i64,
}

/// 池详情：成员、能服务的模型并集、引用它的分组。
/// 分组抽屉与池抽屉都用它把"分组 → 池 → 渠道 → 模型"三跳在一处看全。
#[derive(Debug, Serialize)]
pub struct PoolDetail {
    pub pool_code: String,
    pub description: Option<String>,
    pub routing_strategy: String,
    pub fallback_pool_code: Option<String>,
    pub members: Vec<PoolMemberRow>,
    pub models: Vec<String>,
    pub groups: Vec<String>,
}

pub async fn pool_detail(pool: &PgPool, pool_code: &str) -> Result<Option<PoolDetail>, StoreError> {
    let head = sqlx::query!(
        r#"SELECT pool_code, description, routing_strategy, fallback_pool_code
           FROM channel_pools WHERE pool_code = $1"#,
        pool_code
    )
    .fetch_optional(pool)
    .await?;
    let Some(head) = head else {
        return Ok(None);
    };
    let members = sqlx::query!(
        r#"
        SELECT c.id AS channel_id, c.name, c.provider, c.status, c.priority, c.models,
               pc.priority_override, pc.weight_override,
               (SELECT COUNT(*) FROM channel_keys k
                 WHERE k.channel_id = c.id AND k.status = 1
                   AND (k.cooldown_until IS NULL OR k.cooldown_until < now())) AS "active_keys!"
        FROM pool_channels pc
        JOIN channels c ON c.id = pc.channel_id
        WHERE pc.pool_code = $1 AND c.deleted_at IS NULL
        ORDER BY COALESCE(pc.priority_override, c.priority) DESC, c.id
        "#,
        pool_code
    )
    .fetch_all(pool)
    .await?;
    let groups = sqlx::query_scalar!(
        r#"SELECT group_code FROM price_groups WHERE pool_code = $1 ORDER BY sort_order, group_code"#,
        pool_code
    )
    .fetch_all(pool)
    .await?;

    let mut models: Vec<String> = Vec::new();
    let members: Vec<PoolMemberRow> = members
        .into_iter()
        .map(|r| {
            let served: Vec<String> = serde_json::from_value(r.models).unwrap_or_default();
            if r.status == 1 {
                for m in &served {
                    if !models.contains(m) {
                        models.push(m.clone());
                    }
                }
            }
            PoolMemberRow {
                channel_id: r.channel_id,
                name: r.name,
                provider: r.provider,
                status: r.status,
                priority: r.priority,
                priority_override: r.priority_override,
                weight_override: r.weight_override,
                models: served,
                active_keys: r.active_keys,
            }
        })
        .collect();
    models.sort();
    Ok(Some(PoolDetail {
        pool_code: head.pool_code,
        description: head.description,
        routing_strategy: head.routing_strategy,
        fallback_pool_code: head.fallback_pool_code,
        members,
        models,
        groups,
    }))
}

pub async fn list_pools(pool: &PgPool, slice: Slice) -> Result<Page<PoolListRow>, StoreError> {
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT p.pool_code, p.description, p.routing_strategy, p.fallback_pool_code,
               (SELECT COUNT(*) FROM pool_channels pc
                  JOIN channels c ON c.id = pc.channel_id
                 WHERE pc.pool_code = p.pool_code AND c.deleted_at IS NULL)
                   AS "channel_count!",
               (SELECT COUNT(*) FROM price_groups g WHERE g.pool_code = p.pool_code)
                   AS "group_count!",
               (SELECT COUNT(*) FROM api_keys k
                 WHERE k.pool_override = p.pool_code AND k.deleted_at IS NULL) AS "key_count!",
               (SELECT COUNT(*) FROM channel_pools f
                 WHERE f.fallback_pool_code = p.pool_code) AS "fallback_ref_count!"
        FROM channel_pools p
        ORDER BY (p.pool_code <> 'default'), p.pool_code
        LIMIT $1 OFFSET $2
        "#,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        count_unless_all(
            slice,
            sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM channel_pools"#)
                .fetch_one(pool)
        ),
    )?;
    let total = total.unwrap_or_else(|| len_as_total(rows.len()));
    let data = rows
        .into_iter()
        .map(|r| PoolListRow {
            pool_code: r.pool_code,
            description: r.description,
            routing_strategy: r.routing_strategy,
            fallback_pool_code: r.fallback_pool_code,
            channel_count: r.channel_count,
            group_count: r.group_count,
            key_count: r.key_count,
            fallback_ref_count: r.fallback_ref_count,
        })
        .collect();
    Ok(Page { data, total })
}

pub async fn list_groups(pool: &PgPool, slice: Slice) -> Result<Page<GroupListRow>, StoreError> {
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT g.group_code, g.group_ratio::text AS group_ratio, g.description,
               g.is_default, g.sort_order, g.pool_code, g.self_select,
               g.rpm_limit, g.rph_limit,
               (SELECT COUNT(*) FROM user_groups ug WHERE ug.group_code = g.group_code)
                   AS "user_count!",
               (SELECT COUNT(*) FROM pool_channels pc
                  JOIN channels c ON c.id = pc.channel_id
                 WHERE pc.pool_code = g.pool_code AND c.deleted_at IS NULL) AS "channel_count!"
        FROM price_groups g
        ORDER BY g.sort_order, g.group_code
        LIMIT $1 OFFSET $2
        "#,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        count_unless_all(
            slice,
            sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM price_groups"#)
                .fetch_one(pool)
        ),
    )?;
    let total = total.unwrap_or_else(|| len_as_total(rows.len()));
    let data = rows
        .into_iter()
        .map(|r| GroupListRow {
            group_code: r.group_code,
            group_ratio: r.group_ratio,
            description: r.description,
            is_default: r.is_default,
            sort_order: r.sort_order,
            user_count: r.user_count,
            pool_code: r.pool_code,
            channel_count: r.channel_count,
            self_select: r.self_select,
            rpm_limit: r.rpm_limit,
            rph_limit: r.rph_limit,
        })
        .collect();
    Ok(Page { data, total })
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
    pub ip_allowlist: Option<serde_json::Value>,
    pub rpm_limit: Option<i32>,
    pub max_concurrency: Option<i32>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 令牌列表（管理员可跨用户；`user` = Some(uid) 限定单用户，供门户复用）。
/// 大表：`slice` 不带 limit 也封顶到 `MAX_PAGE`。
pub async fn list_api_keys(
    pool: &PgPool,
    user: Option<i64>,
    query: Option<&str>,
    slice: Slice,
) -> Result<Page<ApiKeyListRow>, StoreError> {
    let (limit, offset) = (slice.capped_limit(), slice.offset);
    let pattern = like_pattern(query);
    // 大表 limit 永远被封顶，data.len() 不等于 total，COUNT 不能省——只并行
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT k.id, k.user_id, u.username, k.team_id, k.name, k.key_prefix, k.status,
               k.quota_mode, k.quota_micro, k.used_micro, k.model_allowlist,
               k.group_override, k.ip_allowlist, k.rpm_limit, k.max_concurrency,
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
        .fetch_all(pool),
        sqlx::query_scalar!(
            r#"SELECT COUNT(*)::bigint AS "c!"
           FROM api_keys k JOIN users u ON u.id = k.user_id
           WHERE k.deleted_at IS NULL
             AND ($1::bigint IS NULL OR k.user_id = $1)
             AND ($2::text IS NULL OR k.name ILIKE $2 OR u.username ILIKE $2)"#,
            user,
            pattern.as_deref()
        )
        .fetch_one(pool),
    )?;
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
                ip_allowlist: r.ip_allowlist,
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
/// 大表：`slice` 不带 limit 也封顶到 `MAX_PAGE`。
pub async fn list_redemptions(
    pool: &PgPool,
    batch: Option<uuid::Uuid>,
    status: Option<i16>,
    slice: Slice,
) -> Result<Page<RedemptionListRow>, StoreError> {
    let (limit, offset) = (slice.capped_limit(), slice.offset);
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
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
        .fetch_all(pool),
        sqlx::query_scalar!(
            r#"SELECT COUNT(*)::bigint AS "c!" FROM redemption_codes
           WHERE ($1::uuid IS NULL OR batch_id = $1)
             AND ($2::smallint IS NULL OR status = $2)"#,
            batch,
            status
        )
        .fetch_one(pool),
    )?;
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
    /// 0 充值模板 / 1 订阅（IMPLEMENTATION §11.28）。
    pub kind: i16,
    pub grant_micro: i64,
    pub group_code: Option<String>,
    pub balance_valid_days: Option<i32>,
    pub price_micro: i64,
    pub period: Option<i16>,
    pub duration_days: Option<i32>,
    pub sort_order: i32,
    pub description: Option<String>,
    pub status: i16,
    pub created_at: DateTime<Utc>,
    /// 引用该套餐的兑换码数（删除前占用检查）。
    pub code_count: i64,
    /// 当前激活的订阅数（kind 1）。
    pub active_subscribers: i64,
}

pub async fn list_plans(pool: &PgPool, slice: Slice) -> Result<Page<PlanListRow>, StoreError> {
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT p.id, p.plan_code, p.display_name, p.kind, p.grant_micro, p.group_code,
               p.balance_valid_days, p.price_micro, p.period, p.duration_days, p.sort_order,
               p.description, p.status, p.created_at,
               (SELECT COUNT(*) FROM redemption_codes r WHERE r.plan_id = p.id)
                   AS "code_count!",
               (SELECT COUNT(*) FROM user_subscriptions s WHERE s.plan_id = p.id AND s.status = 1)
                   AS "active_subscribers!"
        FROM plans p ORDER BY p.id DESC
        LIMIT $1 OFFSET $2
        "#,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        count_unless_all(
            slice,
            sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM plans"#).fetch_one(pool)
        ),
    )?;
    let total = total.unwrap_or_else(|| len_as_total(rows.len()));
    let data = rows
        .into_iter()
        .map(|r| PlanListRow {
            id: r.id,
            plan_code: r.plan_code,
            display_name: r.display_name,
            kind: r.kind,
            grant_micro: r.grant_micro,
            group_code: r.group_code,
            balance_valid_days: r.balance_valid_days,
            price_micro: r.price_micro,
            period: r.period,
            duration_days: r.duration_days,
            sort_order: r.sort_order,
            description: r.description,
            status: r.status,
            created_at: r.created_at,
            code_count: r.code_count,
            active_subscribers: r.active_subscribers,
        })
        .collect();
    Ok(Page { data, total })
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
    fn slice_is_unbounded_without_limit_and_clamped_with_it() {
        let all = Slice::new(None, -3);
        assert_eq!(
            (all.limit, all.offset),
            (None, 0),
            "不传 limit 回全量，负 offset 收敛到 0"
        );
        let page = Slice::new(Some(10_000), 40);
        assert_eq!(
            (page.limit, page.offset),
            (Some(MAX_PAGE), 40),
            "给了 limit 才钳上限"
        );
        assert_eq!(
            Slice::new(Some(0), 0).limit,
            Some(1),
            "非法值收敛到合法下界"
        );
        assert_eq!((Slice::ALL.limit, Slice::ALL.offset), (None, 0));
    }

    #[test]
    fn capped_limit_never_returns_the_whole_table() {
        assert_eq!(Slice::ALL.capped_limit(), MAX_PAGE, "大表不传 limit 也封顶");
        assert_eq!(Slice::new(Some(50), 10).capped_limit(), 50);
        assert_eq!(Slice::new(Some(10_000), 0).capped_limit(), MAX_PAGE);
    }

    #[test]
    fn only_a_full_unoffset_slice_skips_the_count() {
        assert!(Slice::ALL.is_all());
        assert!(
            Slice::new(None, -1).is_all(),
            "负 offset 收敛到 0 后仍是全量"
        );
        assert!(
            !Slice::new(None, 5).is_all(),
            "跳过了前几行，data.len() 不再等于 total"
        );
        assert!(
            !Slice::new(Some(MAX_PAGE), 0).is_all(),
            "给了 limit 就可能截断"
        );
    }

    #[tokio::test]
    async fn count_unless_all_only_awaits_the_count_when_paged() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let ran = AtomicBool::new(false);
        let count = || async {
            ran.store(true, Ordering::SeqCst);
            Ok::<i64, sqlx::Error>(7)
        };
        assert_eq!(count_unless_all(Slice::ALL, count()).await.unwrap(), None);
        assert!(!ran.load(Ordering::SeqCst), "全量切片不得发 COUNT");

        let paged = Slice::new(Some(10), 0);
        assert_eq!(count_unless_all(paged, count()).await.unwrap(), Some(7));
        assert!(ran.load(Ordering::SeqCst));
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
