use crate::error::StoreError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// 模型 + 倍率定价 upsert（倍率以十进制字符串精确入库）。返回 model_id。
/// 倍率制的全部 token 侧轴（十进制字符串，精确入库；禁浮点）。
///
/// 用结构体而非位置参数：轴已达七条，位置参数极易错位——把 audio 填进 image
/// 不会报错，只会静默错价。
#[derive(Debug, Clone, Copy)]
pub struct RatioAxes<'a> {
    pub model: &'a str,
    pub completion: &'a str,
    pub cache: &'a str,
    pub cache_write: &'a str,
    pub audio: &'a str,
    pub audio_completion: &'a str,
    pub image: &'a str,
    /// None preserves existing independent rates; {} clears them.
    pub modality_ratios: Option<&'a serde_json::Value>,
}

impl<'a> RatioAxes<'a> {
    /// 只给三层基础倍率，其余轴取 1.0（缺省 = 按文本计，语义见 DESIGN §3.2）。
    #[must_use]
    pub const fn basic(model: &'a str, completion: &'a str, cache: &'a str) -> Self {
        Self {
            model,
            completion,
            cache,
            cache_write: "1",
            audio: "1",
            audio_completion: "1",
            image: "1",
            modality_ratios: None,
        }
    }
}

pub async fn upsert_model_ratio(
    pool: &PgPool,
    model_name: &str,
    axes: RatioAxes<'_>,
) -> Result<i64, StoreError> {
    let mut tx = pool.begin().await?;
    let id = upsert_model_ratio_in(&mut tx, model_name, axes).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn upsert_model_ratio_in(
    connection: &mut sqlx::PgConnection,
    model_name: &str,
    axes: RatioAxes<'_>,
) -> Result<i64, StoreError> {
    // vendor 按模型名前缀自动归类（仅在为空时填，管理员显式值永不被覆盖）——
    // 省掉建模型时的一次手填，列表页也就能按供应商分组筛选
    let vendor = crate::vendor::classify(model_name);
    let model_id = sqlx::query_scalar!(
        r#"
        INSERT INTO models (model_name, vendor) VALUES ($1, $2)
        ON CONFLICT (model_name) DO UPDATE SET
            vendor = COALESCE(models.vendor, EXCLUDED.vendor),
            updated_at = now()
        RETURNING id
        "#,
        model_name,
        vendor
    )
    .fetch_one(&mut *connection)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO model_pricing
            (model_id, pricing_mode, model_ratio, completion_ratio, cache_ratio,
             cache_write_ratio, audio_ratio, audio_completion_ratio, image_ratio, modality_ratios)
        VALUES ($1, 'ratio', ($2::text)::numeric, ($3::text)::numeric, ($4::text)::numeric,
                ($5::text)::numeric, ($6::text)::numeric, ($7::text)::numeric,
                ($8::text)::numeric, COALESCE($9, '{}'::jsonb))
        ON CONFLICT (model_id) DO UPDATE SET
            pricing_mode = 'ratio',
            model_ratio = EXCLUDED.model_ratio,
            completion_ratio = EXCLUDED.completion_ratio,
            cache_ratio = EXCLUDED.cache_ratio,
            cache_write_ratio = EXCLUDED.cache_write_ratio,
            audio_ratio = EXCLUDED.audio_ratio,
            audio_completion_ratio = EXCLUDED.audio_completion_ratio,
            image_ratio = EXCLUDED.image_ratio,
            modality_ratios = COALESCE($9, model_pricing.modality_ratios),
            -- 改回 ratio 必须清掉阶梯表：留着它，下次误切 tiered 会拿到一张早已过期的旧表；
            -- 按次单价同理——对外的定价清单 / MCP 不看模式就把它列出来
            tier_expr = NULL,
            per_call_price_micro = NULL,
            updated_at = now()
        "#,
        model_id,
        axes.model,
        axes.completion,
        axes.cache,
        axes.cache_write,
        axes.audio,
        axes.audio_completion,
        axes.image,
        axes.modality_ratios
    )
    .execute(&mut *connection)
    .await?;
    Ok(model_id)
}

/// 阶梯计价写入（`pricing_mode='tiered'` + `tier_expr`）。
///
/// 与 ratio 模式共用除 `model_ratio` 外的全部倍率轴——阶梯只替换「每 token 基准单价」
/// 这一段，补全/缓存/多模态轴仍照常叠乘。
///
/// 此前引擎（`PricingMode::Tiered`）、价簿加载器（`pricing_loader` 读 `tier_expr`）、
/// 解析器（`TierTable::parse`）与 schema 用例都在，唯独没有任何写入路径——三种计价模式，
/// 控制面只配得出 ratio，per_call 得绕 new-api 导入，tiered 只能手改库。
pub async fn upsert_model_tiered(
    pool: &PgPool,
    model_name: &str,
    axes: RatioAxes<'_>,
    tier_expr: &str,
) -> Result<i64, StoreError> {
    let mut tx = pool.begin().await?;
    let vendor = crate::vendor::classify(model_name);
    let model_id = sqlx::query_scalar!(
        r#"
        INSERT INTO models (model_name, vendor) VALUES ($1, $2)
        ON CONFLICT (model_name) DO UPDATE SET
            vendor = COALESCE(models.vendor, EXCLUDED.vendor),
            updated_at = now()
        RETURNING id
        "#,
        model_name,
        vendor
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO model_pricing
            (model_id, pricing_mode, model_ratio, completion_ratio, cache_ratio,
             cache_write_ratio, audio_ratio, audio_completion_ratio, image_ratio, tier_expr, modality_ratios)
        VALUES ($1, 'tiered', ($2::text)::numeric, ($3::text)::numeric, ($4::text)::numeric,
                ($5::text)::numeric, ($6::text)::numeric, ($7::text)::numeric,
                ($8::text)::numeric, $9, COALESCE($10, '{}'::jsonb))
        ON CONFLICT (model_id) DO UPDATE SET
            pricing_mode = 'tiered',
            completion_ratio = EXCLUDED.completion_ratio,
            cache_ratio = EXCLUDED.cache_ratio,
            cache_write_ratio = EXCLUDED.cache_write_ratio,
            audio_ratio = EXCLUDED.audio_ratio,
            audio_completion_ratio = EXCLUDED.audio_completion_ratio,
            image_ratio = EXCLUDED.image_ratio,
            tier_expr = EXCLUDED.tier_expr,
            modality_ratios = COALESCE($10, model_pricing.modality_ratios),
            -- 切到阶梯要清掉按次单价：留着它，不看模式的对外定价清单 / MCP 会把它列出来（同 upsert_model_ratio_in）
            per_call_price_micro = NULL,
            updated_at = now()
        "#,
        model_id,
        axes.model,
        axes.completion,
        axes.cache,
        axes.cache_write,
        axes.audio,
        axes.audio_completion,
        axes.image,
        tier_expr,
        axes.modality_ratios
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(model_id)
}

/// 按次计费模型 upsert（new-api model_price 导入等）。
pub async fn upsert_model_per_call(
    pool: &PgPool,
    model_name: &str,
    per_call_price_micro: i64,
) -> Result<i64, StoreError> {
    let mut tx = pool.begin().await?;
    let id = upsert_model_per_call_in(&mut tx, model_name, per_call_price_micro).await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn upsert_model_per_call_in(
    connection: &mut sqlx::PgConnection,
    model_name: &str,
    per_call_price_micro: i64,
) -> Result<i64, StoreError> {
    // vendor 按模型名前缀自动归类（仅在为空时填，管理员显式值永不被覆盖）——
    // 省掉建模型时的一次手填，列表页也就能按供应商分组筛选
    let vendor = crate::vendor::classify(model_name);
    let model_id = sqlx::query_scalar!(
        r#"
        INSERT INTO models (model_name, vendor) VALUES ($1, $2)
        ON CONFLICT (model_name) DO UPDATE SET
            vendor = COALESCE(models.vendor, EXCLUDED.vendor),
            updated_at = now()
        RETURNING id
        "#,
        model_name,
        vendor
    )
    .fetch_one(&mut *connection)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO model_pricing (model_id, pricing_mode, per_call_price_micro)
        VALUES ($1, 'per_call', $2)
        ON CONFLICT (model_id) DO UPDATE SET
            pricing_mode = 'per_call',
            per_call_price_micro = EXCLUDED.per_call_price_micro,
            -- 切到按次要清掉阶梯表：留着它，下次误切 tiered 会拿到一张早已过期的旧表，
            -- 不看模式的对外定价清单 / MCP 也会把它列出来（同 upsert_model_ratio_in）
            tier_expr = NULL,
            updated_at = now()
        "#,
        model_id,
        per_call_price_micro
    )
    .execute(&mut *connection)
    .await?;
    Ok(model_id)
}

/// 定价分组 upsert 参数。
#[derive(Debug, Clone, Copy)]
pub struct PriceGroupInput<'a> {
    pub group_code: &'a str,
    /// 倍率十进制字符串，精确入库。
    pub group_ratio: &'a str,
    pub description: &'a str,
    /// 该分组的用户打哪个池；None = 内置 default 池。
    pub pool_code: Option<&'a str>,
    /// 用户可否在门户为自己的 key 选择此分组。
    pub self_select: bool,
    /// 分组内每用户分钟 / 小时请求上限（§11.32）；None = 不限。
    pub rpm_limit: Option<i32>,
    pub rph_limit: Option<i32>,
}

/// 定价分组 upsert。分组必有池（缺省 default）——"无池"这个状态不再存在。
pub async fn upsert_price_group(
    pool: &PgPool,
    input: PriceGroupInput<'_>,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"
        INSERT INTO price_groups (group_code, group_ratio, description, pool_code, self_select,
                                  rpm_limit, rph_limit)
        VALUES ($1, ($2::text)::numeric, $3, $4, $5, $6, $7)
        ON CONFLICT (group_code) DO UPDATE SET
            group_ratio = EXCLUDED.group_ratio,
            description = EXCLUDED.description,
            pool_code   = EXCLUDED.pool_code,
            self_select = EXCLUDED.self_select,
            rpm_limit   = EXCLUDED.rpm_limit,
            rph_limit   = EXCLUDED.rph_limit
        "#,
        input.group_code,
        input.group_ratio,
        input.description,
        input.pool_code.unwrap_or(crate::channels::DEFAULT_POOL),
        input.self_select,
        input.rpm_limit,
        input.rph_limit
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 定价分组是否存在（`group_override` 写入前置校验：
/// 交给 FK 报错只能得到 500，前置查得到可渲染的 error_code）。
pub async fn price_group_exists(pool: &PgPool, group_code: &str) -> Result<bool, StoreError> {
    let hit = sqlx::query_scalar!(
        r#"SELECT 1 AS "hit!" FROM price_groups WHERE group_code = $1"#,
        group_code
    )
    .fetch_optional(pool)
    .await?;
    Ok(hit.is_some())
}

/// 定价规则写入参数（具名字段防相邻同类型参数错位）。
#[derive(Debug, Clone, Copy)]
pub struct PricingRuleInput<'a> {
    pub rule_code: &'a str,
    /// volume | time_based | discount | surge。
    pub rule_type: &'a str,
    pub scope: &'a serde_json::Value,
    pub params: &'a serde_json::Value,
    pub priority: i32,
    pub enabled: bool,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
}

/// 一条定价规则（管理端列表）。
#[derive(Debug, Clone)]
pub struct PricingRuleRow {
    pub rule_code: String,
    pub rule_type: String,
    pub scope: serde_json::Value,
    pub params: serde_json::Value,
    pub priority: i32,
    pub enabled: bool,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
}

/// 定价规则 upsert（rule_code 为幂等锚；改动后需 publish 才进价簿）。
/// 语义合法性（rule_type × params 组合）由 console 侧校验，本层只落库。
pub async fn upsert_pricing_rule(
    pool: &PgPool,
    input: PricingRuleInput<'_>,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"
        INSERT INTO pricing_rules
            (rule_code, rule_type, scope, params, priority, enabled, valid_from, valid_to)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (rule_code) DO UPDATE SET
            rule_type = EXCLUDED.rule_type,
            scope = EXCLUDED.scope,
            params = EXCLUDED.params,
            priority = EXCLUDED.priority,
            enabled = EXCLUDED.enabled,
            valid_from = EXCLUDED.valid_from,
            valid_to = EXCLUDED.valid_to,
            updated_at = now()
        "#,
        input.rule_code,
        input.rule_type,
        input.scope,
        input.params,
        input.priority,
        input.enabled,
        input.valid_from,
        input.valid_to
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 列出定价规则（含停用；按生效序返回便于管理端核对叠加顺序）；`slice` 不分页时回全量。
pub async fn list_pricing_rules(
    pool: &PgPool,
    slice: crate::listing::Slice,
) -> Result<crate::listing::Page<PricingRuleRow>, StoreError> {
    let (rows, total) = tokio::try_join!(
        sqlx::query!(
            r#"
        SELECT rule_code, rule_type, scope, params, priority, enabled, valid_from, valid_to
        FROM pricing_rules
        ORDER BY
            CASE rule_type
                WHEN 'volume' THEN 0
                WHEN 'time_based' THEN 1
                WHEN 'discount' THEN 2
                ELSE 3
            END,
            priority,
            rule_code
        LIMIT $1 OFFSET $2
        "#,
            slice.limit,
            slice.offset
        )
        .fetch_all(pool),
        crate::listing::count_unless_all(
            slice,
            sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM pricing_rules"#)
                .fetch_one(pool)
        ),
    )?;
    let total = total.unwrap_or_else(|| crate::listing::len_as_total(rows.len()));
    let data = rows
        .into_iter()
        .map(|r| PricingRuleRow {
            rule_code: r.rule_code,
            rule_type: r.rule_type,
            scope: r.scope,
            params: r.params,
            priority: r.priority,
            enabled: r.enabled,
            valid_from: r.valid_from,
            valid_to: r.valid_to,
        })
        .collect();
    Ok(crate::listing::Page { data, total })
}

/// 删除定价规则；返回是否命中（false = 不存在，调用方转 404）。
pub async fn delete_pricing_rule(pool: &PgPool, rule_code: &str) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"DELETE FROM pricing_rules WHERE rule_code = $1"#,
        rule_code
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 发布定价 epoch（gateway 经 NATS 广播/30s 轮询感知后整表重载）。
/// snapshot 存发布时刻的定价配置全量（发布历史/回滚/diff 数据源，DESIGN §3.3）。
pub async fn publish_epoch(
    pool: &PgPool,
    published_by: i64,
    snapshot: &serde_json::Value,
) -> Result<i64, StoreError> {
    let epoch = sqlx::query_scalar!(
        r#"
        INSERT INTO pricing_epochs (snapshot, published_by)
        VALUES ($2, $1)
        RETURNING epoch
        "#,
        published_by,
        snapshot
    )
    .fetch_one(pool)
    .await?;
    Ok(epoch)
}
