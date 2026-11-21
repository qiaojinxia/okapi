use crate::error::StoreError;
use sqlx::PgPool;

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
    let plan = if let Some(plan_code) = opts.plan_code {
        let row = sqlx::query!(
            r#"SELECT id,subscription_plan_snapshot(plans) AS snapshot FROM plans WHERE plan_code = $1 AND status = 1"#,
            plan_code
        )
        .fetch_optional(pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Some((row.id, row.snapshot))
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
                 plan_id, bind_user_id, max_per_ip,subscription_snapshot)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8,$9)
            "#,
            code_hash(code),
            amount_micro,
            batch_id,
            created_by,
            expires_at,
            plan.as_ref().map(|p| p.0),
            opts.bind_user_id,
            opts.max_per_ip,
            plan.as_ref().and_then(|p| p.1.as_ref())
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
    /// 绑**订阅**套餐时无意义（核销即激活订阅，不入钱包；`subscription_plan_id` 非空）。
    pub amount_micro: i64,
    pub plan_code: Option<String>,
    /// 套餐附带：兑换后追加的分组。
    pub grant_group: Option<String>,
    /// 套餐附带：兑换后设置余额有效期（天）。
    pub balance_valid_days: Option<i32>,
    /// 绑的是订阅套餐（plans.kind = 1）：调用方走 `subscriptions::activate`（§11.28）。
    pub subscription_plan_id: Option<i64>,
    pub subscription_snapshot: Option<serde_json::Value>,
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

/// Caller owns the business/ledger transaction.
pub async fn claim_redemption_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
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
        RETURNING id, amount_micro, plan_id,subscription_snapshot
        "#,
        code_hash(code),
        user_id
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(r) = row else {
        return Ok(None);
    };
    if let Some(snapshot) = r.subscription_snapshot {
        let plan: crate::subscriptions::SubPlan = serde_json::from_value(snapshot.clone())
            .map_err(|_| StoreError::InvalidData("subscription_snapshot"))?;
        return Ok(Some(ClaimedRedemption {
            code_id: r.id,
            amount_micro: 0,
            plan_code: Some(plan.plan_code),
            grant_group: None,
            balance_valid_days: None,
            subscription_plan_id: Some(plan.id),
            subscription_snapshot: Some(snapshot),
        }));
    }
    // 在同一事务读取套餐；权益与核销、账本一起提交。
    let plan = if let Some(plan_id) = r.plan_id {
        sqlx::query!(
            r#"SELECT id, plan_code, kind, grant_micro, group_code, balance_valid_days
               FROM plans WHERE id = $1 AND status = 1"#,
            plan_id
        )
        .fetch_optional(&mut **tx)
        .await?
    } else {
        None
    };
    Ok(Some(match plan {
        // 订阅套餐：核销即激活，钱包不动；分组由 activate 按"是否新加"记录以便到期收回
        Some(p) if p.kind == 1 => ClaimedRedemption {
            code_id: r.id,
            amount_micro: 0,
            plan_code: Some(p.plan_code),
            grant_group: None,
            balance_valid_days: None,
            subscription_plan_id: Some(p.id),
            subscription_snapshot: None,
        },
        Some(p) => ClaimedRedemption {
            code_id: r.id,
            amount_micro: p.grant_micro,
            plan_code: Some(p.plan_code),
            grant_group: p.group_code,
            balance_valid_days: p.balance_valid_days,
            subscription_plan_id: None,
            subscription_snapshot: None,
        },
        None => ClaimedRedemption {
            code_id: r.id,
            amount_micro: r.amount_micro,
            plan_code: None,
            grant_group: None,
            balance_valid_days: None,
            subscription_plan_id: None,
            subscription_snapshot: None,
        },
    }))
}

/// 套餐 upsert 输入（#1790-5；订阅形态 IMPLEMENTATION §11.28）。
#[derive(Debug, Clone)]
pub struct PlanSpec<'a> {
    pub plan_code: &'a str,
    pub display_name: &'a str,
    /// 0 充值模板 / 1 订阅。
    pub kind: i16,
    /// kind 0：入账金额；kind 1：每窗额度。
    pub grant_micro: i64,
    pub group_code: Option<&'a str>,
    /// kind 0 专用。
    pub balance_valid_days: Option<i32>,
    /// kind 1：售价（0 = 不可自助购买）。
    pub price_micro: i64,
    /// kind 1 必填：1 日 2 周 3 月。
    pub period: Option<i16>,
    /// kind 1 必填：有效期（天）。
    pub duration_days: Option<i32>,
    pub sort_order: i32,
    pub description: Option<&'a str>,
}

/// 建/改套餐（plan_code 幂等 upsert；改动不影响存量订阅——它们存的是快照）。
pub async fn create_plan(pool: &PgPool, spec: &PlanSpec<'_>) -> Result<i64, StoreError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO plans (plan_code, display_name, kind, grant_micro, group_code, balance_valid_days,
                           price_micro, period, duration_days, sort_order, description)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        ON CONFLICT (plan_code) DO UPDATE
            SET display_name = EXCLUDED.display_name,
                kind = EXCLUDED.kind,
                grant_micro = EXCLUDED.grant_micro,
                group_code = EXCLUDED.group_code,
                balance_valid_days = EXCLUDED.balance_valid_days,
                price_micro = EXCLUDED.price_micro,
                period = EXCLUDED.period,
                duration_days = EXCLUDED.duration_days,
                sort_order = EXCLUDED.sort_order,
                description = EXCLUDED.description,
                status = 1
        RETURNING id
        "#,
        spec.plan_code,
        spec.display_name,
        spec.kind,
        spec.grant_micro,
        spec.group_code,
        spec.balance_valid_days,
        spec.price_micro,
        spec.period,
        spec.duration_days,
        spec.sort_order,
        spec.description
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 新订单输入。
#[derive(Debug, Clone)]
pub struct NewOrder<'a> {
    pub order_no: &'a str,
    pub user_id: i64,
    /// 钱包充值：入账额度；订阅购买：售价快照。
    pub amount_micro: i64,
    pub gateway: &'a str,
    /// 支付报价（原币种小数字符串，回调必须精确匹配）。
    pub pay_amount: &'a str,
    pub currency: &'a str,
    pub merchant_id: Option<&'a str>,
    /// 非空 = 订阅购买单（§11.28）：支付成功激活订阅而不入钱包。
    pub plan_id: Option<i64>,
    pub subscription_snapshot: Option<serde_json::Value>,
}

/// 创建充值 / 订阅购买订单（status=0）。
pub async fn create_recharge_order(pool: &PgPool, order: NewOrder<'_>) -> Result<i64, StoreError> {
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO recharge_orders (order_no, user_id, amount_micro, gateway, pay_amount, currency, plan_id,subscription_snapshot,merchant_id,payment_contract_version)
        VALUES ($1, $2, $3, $4, ($5::text)::numeric, $6, $7,$8,$9,1)
        RETURNING id
        "#,
        order.order_no,
        order.user_id,
        order.amount_micro,
        order.gateway,
        order.pay_amount,
        order.currency,
        order.plan_id,
        order.subscription_snapshot,
        order.merchant_id
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// 已核销订单：钱包充值或订阅购买。
#[derive(Debug, Clone)]
pub struct PaidOrder {
    pub user_id: i64,
    pub amount_micro: i64,
    /// 非空 = 订阅购买单，调用方激活订阅而非入钱包。
    pub plan_id: Option<i64>,
    pub subscription_snapshot: Option<serde_json::Value>,
}
