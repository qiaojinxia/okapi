//! 管理面写路径（console 角色用）：模型/定价 upsert、渠道管理、epoch 发布、审计。

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
        }
    }
}

pub async fn upsert_model_ratio(
    pool: &PgPool,
    model_name: &str,
    axes: RatioAxes<'_>,
) -> Result<i64, StoreError> {
    let mut tx = pool.begin().await?;
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
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO model_pricing
            (model_id, pricing_mode, model_ratio, completion_ratio, cache_ratio,
             cache_write_ratio, audio_ratio, audio_completion_ratio, image_ratio)
        VALUES ($1, 'ratio', ($2::text)::numeric, ($3::text)::numeric, ($4::text)::numeric,
                ($5::text)::numeric, ($6::text)::numeric, ($7::text)::numeric,
                ($8::text)::numeric)
        ON CONFLICT (model_id) DO UPDATE SET
            pricing_mode = 'ratio',
            model_ratio = EXCLUDED.model_ratio,
            completion_ratio = EXCLUDED.completion_ratio,
            cache_ratio = EXCLUDED.cache_ratio,
            cache_write_ratio = EXCLUDED.cache_write_ratio,
            audio_ratio = EXCLUDED.audio_ratio,
            audio_completion_ratio = EXCLUDED.audio_completion_ratio,
            image_ratio = EXCLUDED.image_ratio,
            updated_at = now()
        "#,
        model_id,
        axes.model,
        axes.completion,
        axes.cache,
        axes.cache_write,
        axes.audio,
        axes.audio_completion,
        axes.image
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(model_id)
}

fn code_hash(code: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(code.trim().as_bytes()))
}

/// 兑换码批量生成选项（套餐/限定用户/单 IP 上限，#1790-5）。
#[derive(Debug, Default, Clone, Copy)]
pub struct RedemptionOptions<'a> {
    pub plan_code: Option<&'a str>,
    pub bind_user_id: Option<i64>,
    pub max_per_ip: Option<i32>,
}

/// 批量生成兑换码（明文仅生成时返回一次；落库只存 SHA-256，docs §1.6）。
/// 返回 None = 指定的 plan_code 不存在/停用。
pub async fn create_redemption_codes(
    pool: &PgPool,
    created_by: i64,
    amount_micro: i64,
    codes: &[String],
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    opts: RedemptionOptions<'_>,
) -> Result<Option<uuid::Uuid>, StoreError> {
    let plan_id = if let Some(plan_code) = opts.plan_code {
        let id = sqlx::query_scalar!(
            r#"SELECT id FROM plans WHERE plan_code = $1 AND status = 1"#,
            plan_code
        )
        .fetch_optional(pool)
        .await?;
        let Some(id) = id else {
            return Ok(None);
        };
        Some(id)
    } else {
        None
    };
    let batch_id = uuid::Uuid::new_v4();
    let mut tx = pool.begin().await?;
    for code in codes {
        sqlx::query!(
            r#"
            INSERT INTO redemption_codes
                (code_hash, amount_micro, batch_id, created_by, expires_at,
                 plan_id, bind_user_id, max_per_ip)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
            code_hash(code),
            amount_micro,
            batch_id,
            created_by,
            expires_at,
            plan_id,
            opts.bind_user_id,
            opts.max_per_ip
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some(batch_id))
}

/// 原子核销：status 1→2 行级翻转（并发/重放天然拒绝），返回面值与码 id。
/// 核销结果（含套餐附带语义，#1790-5）。
#[derive(Debug)]
pub struct ClaimedRedemption {
    pub code_id: i64,
    /// 实际入账金额：绑套餐时 = plans.grant_micro（覆盖面值），否则 = 面值。
    pub amount_micro: i64,
    pub plan_code: Option<String>,
    /// 套餐附带：兑换后追加的分组。
    pub grant_group: Option<String>,
    /// 套餐附带：兑换后设置余额有效期（天）。
    pub balance_valid_days: Option<i32>,
}

/// 核销预查（IP 限制闸需要在翻转前拿到批次与限额；只读，不改状态）。
pub struct RedemptionPrecheck {
    pub batch_id: uuid::Uuid,
    pub max_per_ip: Option<i32>,
    pub bind_user_id: Option<i64>,
}

pub async fn redemption_precheck(
    pool: &PgPool,
    code: &str,
) -> Result<Option<RedemptionPrecheck>, StoreError> {
    let row = sqlx::query!(
        r#"SELECT batch_id, max_per_ip, bind_user_id
           FROM redemption_codes WHERE code_hash = $1 AND status = 1"#,
        code_hash(code)
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| RedemptionPrecheck {
        batch_id: r.batch_id,
        max_per_ip: r.max_per_ip,
        bind_user_id: r.bind_user_id,
    }))
}

pub async fn claim_redemption(
    pool: &PgPool,
    code: &str,
    user_id: i64,
) -> Result<Option<ClaimedRedemption>, StoreError> {
    // 原子翻转（bind_user 条件内联：他人核销与不存在同响应，防探测）
    let row = sqlx::query!(
        r#"
        UPDATE redemption_codes
        SET status = 2, redeemed_by = $2, redeemed_at = now()
        WHERE code_hash = $1 AND status = 1
          AND (expires_at IS NULL OR expires_at > now())
          AND (bind_user_id IS NULL OR bind_user_id = $2)
        RETURNING id, amount_micro, plan_id
        "#,
        code_hash(code),
        user_id
    )
    .fetch_optional(pool)
    .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    // 套餐为静态配置：两步读无竞态
    let plan = if let Some(plan_id) = r.plan_id {
        sqlx::query!(
            r#"SELECT plan_code, grant_micro, group_code, balance_valid_days
               FROM plans WHERE id = $1 AND status = 1"#,
            plan_id
        )
        .fetch_optional(pool)
        .await?
    } else {
        None
    };
    Ok(Some(match plan {
        Some(p) => ClaimedRedemption {
            code_id: r.id,
            amount_micro: p.grant_micro,
            plan_code: Some(p.plan_code),
            grant_group: p.group_code,
            balance_valid_days: p.balance_valid_days,
        },
        None => ClaimedRedemption {
            code_id: r.id,
            amount_micro: r.amount_micro,
            plan_code: None,
            grant_group: None,
            balance_valid_days: None,
        },
    }))
}

/// 建套餐（#1790-5）。
pub async fn create_plan(
    pool: &PgPool,
    plan_code: &str,
    display_name: &str,
    grant_micro: i64,
    group_code: Option<&str>,
    balance_valid_days: Option<i32>,
) -> Result<i64, StoreError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO plans (plan_code, display_name, grant_micro, group_code, balance_valid_days)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (plan_code) DO UPDATE
            SET display_name = EXCLUDED.display_name,
                grant_micro = EXCLUDED.grant_micro,
                group_code = EXCLUDED.group_code,
                balance_valid_days = EXCLUDED.balance_valid_days,
                status = 1
        RETURNING id
        "#,
        plan_code,
        display_name,
        grant_micro,
        group_code,
        balance_valid_days
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

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

/// 创建充值订单（status=0）。
pub async fn create_recharge_order(
    pool: &PgPool,
    order_no: &str,
    user_id: i64,
    amount_micro: i64,
    gateway: &str,
    pay_amount: &str,
    currency: &str,
) -> Result<i64, StoreError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO recharge_orders (order_no, user_id, amount_micro, gateway, pay_amount, currency)
        VALUES ($1, $2, $3, $4, ($5::text)::numeric, $6)
        RETURNING id
        "#,
        order_no,
        user_id,
        amount_micro,
        gateway,
        pay_amount,
        currency
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 支付回调核销：status 0→1 行级原子翻转（重放/并发恰一次），返回 user 与额度。
pub async fn mark_recharge_paid(
    pool: &PgPool,
    order_no: &str,
    gateway_trade_no: &str,
) -> Result<Option<(i64, i64)>, StoreError> {
    let row = sqlx::query!(
        r#"
        UPDATE recharge_orders
        SET status = 1, gateway_trade_no = $2, paid_at = now()
        WHERE order_no = $1 AND status = 0
        RETURNING user_id, amount_micro
        "#,
        order_no,
        gateway_trade_no
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| (r.user_id, r.amount_micro)))
}

/// 按次计费模型 upsert（new-api model_price 导入等）。
pub async fn upsert_model_per_call(
    pool: &PgPool,
    model_name: &str,
    per_call_price_micro: i64,
) -> Result<i64, StoreError> {
    let mut tx = pool.begin().await?;
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
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO model_pricing (model_id, pricing_mode, per_call_price_micro)
        VALUES ($1, 'per_call', $2)
        ON CONFLICT (model_id) DO UPDATE SET
            pricing_mode = 'per_call',
            per_call_price_micro = EXCLUDED.per_call_price_micro,
            updated_at = now()
        "#,
        model_id,
        per_call_price_micro
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(model_id)
}

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
}

/// 渠道列表；`owner` = Some(uid) 时只返回该属主的渠道（own 范围）。
pub async fn list_channels(
    pool: &PgPool,
    owner: Option<i64>,
) -> Result<Vec<ChannelRow>, StoreError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, name, provider, api_base, status, priority, models, trust_upstream_usage, owner_id
        FROM channels
        WHERE deleted_at IS NULL AND ($1::bigint IS NULL OR owner_id = $1)
        ORDER BY priority DESC, id
        "#,
        owner
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
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
        })
        .collect())
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

/// 覆盖式设置渠道的可见组绑定（空数组 = 清空绑定）。
pub async fn set_channel_groups(
    pool: &PgPool,
    channel_id: i64,
    groups: &[String],
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        r#"DELETE FROM group_channel_bindings WHERE channel_id = $1"#,
        channel_id
    )
    .execute(&mut *tx)
    .await?;
    if !groups.is_empty() {
        sqlx::query!(
            r#"
            INSERT INTO group_channel_bindings (group_code, channel_id)
            SELECT unnest($2::varchar[]), $1
            "#,
            channel_id,
            groups
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// 定价分组 upsert（倍率十进制字符串精确入库）。
pub async fn upsert_price_group(
    pool: &PgPool,
    group_code: &str,
    group_ratio: &str,
    description: &str,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"
        INSERT INTO price_groups (group_code, group_ratio, description)
        VALUES ($1, ($2::text)::numeric, $3)
        ON CONFLICT (group_code) DO UPDATE SET
            group_ratio = EXCLUDED.group_ratio,
            description = EXCLUDED.description
        "#,
        group_code,
        group_ratio,
        description
    )
    .execute(pool)
    .await?;
    Ok(())
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

/// 列出全部定价规则（含停用；按生效序返回便于管理端核对叠加顺序）。
pub async fn list_pricing_rules(pool: &PgPool) -> Result<Vec<PricingRuleRow>, StoreError> {
    let rows = sqlx::query!(
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
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
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
        .collect())
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

/// 覆盖式设置用户分组（定价取最高 priority，可见性取并集，§6.3）。
pub async fn set_user_groups(
    pool: &PgPool,
    user_id: i64,
    groups: &[(String, i32)],
) -> Result<(), StoreError> {
    let codes: Vec<String> = groups.iter().map(|(c, _)| c.clone()).collect();
    let priorities: Vec<i32> = groups.iter().map(|(_, p)| *p).collect();
    let mut tx = pool.begin().await?;
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
    tx.commit().await?;
    Ok(())
}

/// 全局设置写入（settings KV）。
pub async fn set_setting(
    pool: &PgPool,
    key: &str,
    value: &serde_json::Value,
    updated_by: i64,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"
        INSERT INTO settings (key, value, updated_by)
        VALUES ($1, $2, $3)
        ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
            updated_by = EXCLUDED.updated_by, updated_at = now()
        "#,
        key,
        value,
        updated_by
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_channel_keys(pool: &PgPool) -> Result<Vec<ChannelKeyRow>, StoreError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, channel_id, status, failed_count, cooldown_until, last_error,
               weight, max_concurrency
        FROM channel_keys ORDER BY channel_id, id
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ChannelKeyRow {
            id: r.id,
            channel_id: r.channel_id,
            status: r.status,
            failed_count: r.failed_count,
            cooldown_until: r.cooldown_until,
            last_error: r.last_error,
            weight: r.weight,
            max_concurrency: r.max_concurrency,
        })
        .collect())
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

/// 创建自定义管理子角色（权限点集合）。
pub async fn create_admin_role(
    pool: &PgPool,
    role_code: &str,
    display_name: &str,
    permissions: &serde_json::Value,
) -> Result<i64, StoreError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO admin_roles (role_code, display_name, permissions)
        VALUES ($1, $2, $3)
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
/// 分层由调用方决定：自助面只填前四项（只能把自己的 key 收窄），
/// 限额与 `group_override` 是管控项与计价锚点，仅管理面填写。
#[derive(Debug, Default, Clone)]
pub struct ApiKeyPatch {
    pub name: Option<String>,
    /// 1=启用 2=停用（3=expired 由过期时间派生，不接受直接写）。
    pub status: Option<i16>,
    pub expires_at: Option<Option<DateTime<Utc>>>,
    pub model_allowlist: Option<Option<serde_json::Value>>,
    pub group_override: Option<Option<String>>,
    pub rpm_limit: Option<Option<i32>>,
    pub tpm_limit: Option<Option<i32>>,
    pub rpd_limit: Option<Option<i32>>,
    pub daily_token_limit: Option<Option<i64>>,
    pub max_concurrency: Option<Option<i32>>,
}

/// 被改动的 key 标识：`key_hash` 供调用方精确失效 `auth:key:*` 缓存，
/// `user_id` 供审计记录归属主体。
#[derive(Debug, Clone)]
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
    let row = sqlx::query!(
        r#"
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
            max_concurrency   = CASE WHEN $19::bool THEN $20::int ELSE max_concurrency END
        WHERE id = $1 AND deleted_at IS NULL
          AND ($2::bigint IS NULL OR user_id = $2)
        RETURNING key_hash, user_id
        "#,
        key_id,
        owner,
        patch.name.as_deref(),
        patch.status,
        patch.expires_at.is_some(),
        patch.expires_at.flatten(),
        patch.model_allowlist.is_some(),
        patch.model_allowlist.as_ref().and_then(Option::as_ref),
        patch.group_override.is_some(),
        patch.group_override.as_ref().and_then(Option::as_deref),
        patch.rpm_limit.is_some(),
        patch.rpm_limit.flatten(),
        patch.tpm_limit.is_some(),
        patch.tpm_limit.flatten(),
        patch.rpd_limit.is_some(),
        patch.rpd_limit.flatten(),
        patch.daily_token_limit.is_some(),
        patch.daily_token_limit.flatten(),
        patch.max_concurrency.is_some(),
        patch.max_concurrency.flatten(),
    )
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
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 软删除渠道；返回是否命中。调度候选查询已按 `deleted_at IS NULL` 过滤，
/// 故不必级联改 channel_keys（历史账单仍能按 channel_id 回溯）。
pub async fn soft_delete_channel(pool: &PgPool, channel_id: i64) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"UPDATE channels SET deleted_at = now(), updated_at = now()
           WHERE id = $1 AND deleted_at IS NULL"#,
        channel_id
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

/// 渠道单把 key 的权重/并发上限调整；返回是否命中。
pub async fn patch_channel_key(
    pool: &PgPool,
    channel_id: i64,
    channel_key_id: i64,
    weight: Option<i32>,
    max_concurrency: Option<Option<i32>>,
) -> Result<bool, StoreError> {
    let affected = sqlx::query!(
        r#"
        UPDATE channel_keys SET
            weight          = COALESCE($3, weight),
            max_concurrency = CASE WHEN $4::bool THEN $5::int ELSE max_concurrency END,
            updated_at      = now()
        WHERE id = $2 AND channel_id = $1
        "#,
        channel_id,
        channel_key_id,
        weight,
        max_concurrency.is_some(),
        max_concurrency.flatten(),
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
    let hit = sqlx::query_scalar!(
        r#"
        UPDATE channel_keys SET
            credential_ciphertext = $3,
            status = 1, cooldown_until = NULL, failed_count = 0, last_error = NULL,
            updated_at = now()
        WHERE id = $2 AND channel_id = $1
        RETURNING id
        "#,
        channel_id,
        target,
        credential.as_bytes()
    )
    .fetch_optional(pool)
    .await?;
    Ok(hit.map_or(RotateOutcome::NotFound, RotateOutcome::Rotated))
}

/// 管理操作审计留痕。
pub async fn record_audit(
    pool: &PgPool,
    actor: &str,
    action: &str,
    target: &str,
    detail: serde_json::Value,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"INSERT INTO audit_logs (actor, action, target, detail) VALUES ($1, $2, $3, $4)"#,
        actor,
        action,
        target,
        detail
    )
    .execute(pool)
    .await?;
    Ok(())
}
