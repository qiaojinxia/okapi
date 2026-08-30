//! 管理面写路径（console 角色用）：模型/定价 upsert、渠道管理、epoch 发布、审计。

use crate::error::StoreError;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// 模型 + 倍率定价 upsert（倍率以十进制字符串精确入库）。返回 model_id。
pub async fn upsert_model_ratio(
    pool: &PgPool,
    model_name: &str,
    model_ratio: &str,
    completion_ratio: &str,
    cache_ratio: &str,
    cache_write_ratio: &str,
) -> Result<i64, StoreError> {
    let mut tx = pool.begin().await?;
    let model_id = sqlx::query_scalar!(
        r#"
        INSERT INTO models (model_name) VALUES ($1)
        ON CONFLICT (model_name) DO UPDATE SET updated_at = now()
        RETURNING id
        "#,
        model_name
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        r#"
        INSERT INTO model_pricing
            (model_id, pricing_mode, model_ratio, completion_ratio, cache_ratio, cache_write_ratio)
        VALUES ($1, 'ratio', ($2::text)::numeric, ($3::text)::numeric, ($4::text)::numeric,
                ($5::text)::numeric)
        ON CONFLICT (model_id) DO UPDATE SET
            pricing_mode = 'ratio',
            model_ratio = EXCLUDED.model_ratio,
            completion_ratio = EXCLUDED.completion_ratio,
            cache_ratio = EXCLUDED.cache_ratio,
            cache_write_ratio = EXCLUDED.cache_write_ratio,
            updated_at = now()
        "#,
        model_id,
        model_ratio,
        completion_ratio,
        cache_ratio,
        cache_write_ratio
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
    let model_id = sqlx::query_scalar!(
        r#"
        INSERT INTO models (model_name) VALUES ($1)
        ON CONFLICT (model_name) DO UPDATE SET updated_at = now()
        RETURNING id
        "#,
        model_name
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
        SELECT id, channel_id, status, failed_count, cooldown_until, last_error, max_concurrency
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
