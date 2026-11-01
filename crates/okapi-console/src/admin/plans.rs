//! 订阅套餐与兑换码批次。
use super::{audit, ensure_group_usable, ensure_redemption_amount, guard};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct UpsertPlanReq {
    pub plan_code: String,
    pub display_name: String,
    /// 0 充值模板（缺省）/ 1 订阅（IMPLEMENTATION §11.28）。
    #[serde(default)]
    pub kind: i16,
    /// kind 0：入账金额；kind 1：每窗额度。
    pub grant_micro: i64,
    /// 兑换后追加分组 / 订阅有效期内附加分组（须为已存在的 price_groups.group_code）。
    #[serde(default)]
    pub group_code: Option<String>,
    /// kind 0：兑换后设置余额有效期（天）。
    #[serde(default)]
    pub balance_valid_days: Option<i32>,
    /// kind 1：售价（0 = 不可自助购买）。
    #[serde(default)]
    pub price_micro: i64,
    /// kind 1 必填：1 日 2 周 3 月。
    #[serde(default)]
    pub period: Option<i16>,
    /// kind 1 必填：有效期（天）。
    #[serde(default)]
    pub duration_days: Option<i32>,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub description: Option<String>,
}

/// 建/改套餐（#1790-5；plan_code 幂等 upsert；订阅形态字段校验见 §11.28）。
pub async fn upsert_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpsertPlanReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    if req.grant_micro <= 0 {
        return Err(AppError::bad_request().with_param("plan"));
    }
    let plan_code = super::super::identifiers::ensure_code("plan_code", &req.plan_code, 64)?;
    let display_name =
        super::super::identifiers::ensure_optional_text("display_name", &req.display_name, 128)?;
    if req.balance_valid_days.is_some_and(|d| d <= 0) {
        return Err(AppError::bad_request().with_param("balance_valid_days"));
    }
    if req.grant_micro > okapi_ledger::holds::MAXIMUM_MICROS {
        return Err(AppError::bad_request().with_param("grant_micro"));
    }
    if !(0..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&req.price_micro) {
        return Err(AppError::bad_request().with_param("price_micro"));
    }
    let (period, duration_days, balance_valid_days) = match req.kind {
        0 => (None, None, req.balance_valid_days),
        1 => {
            let Some(period) = req.period.filter(|p| (1..=3).contains(p)) else {
                return Err(AppError::bad_request().with_param("period"));
            };
            let Some(days) = req.duration_days.filter(|d| *d > 0) else {
                return Err(AppError::bad_request().with_param("duration_days"));
            };
            okapi_store::subscriptions::expiry_after(chrono::Utc::now(), days)
                .map_err(|_| AppError::bad_request().with_param("duration_days"))?;
            (Some(period), Some(days), None)
        }
        _ => return Err(AppError::bad_request().with_param("kind")),
    };
    // 与另外两处设组入口同一道闸：分组要存在**且已发布进价簿**。只校验存在的话，套餐绑了还没发布的组，
    // 买了它的用户每笔请求都在计价处撞 `UnknownGroup`（fail-closed 回 500），直到有人发布
    if let Some(group) = req.group_code.as_deref() {
        ensure_group_usable(&state, group, "group_code").await?;
    }
    let id = okapi_store::admin::create_plan(
        &state.pg,
        &okapi_store::admin::PlanSpec {
            plan_code,
            display_name,
            kind: req.kind,
            grant_micro: req.grant_micro,
            group_code: req.group_code.as_deref(),
            balance_valid_days,
            price_micro: req.price_micro,
            period,
            duration_days,
            sort_order: req.sort_order,
            description: req
                .description
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        },
    )
    .await?;
    audit(
        &state,
        &actor,
        "plan.upsert",
        plan_code,
        json!({
            "kind": req.kind,
            "grant_micro": req.grant_micro,
            "group_code": req.group_code,
            "balance_valid_days": balance_valid_days,
            "price_micro": req.price_micro,
            "period": period,
            "duration_days": duration_days,
        }),
    )
    .await;
    Ok(Json(json!({ "plan_id": id })))
}

#[derive(Deserialize)]
pub struct CreateRedemptionsReq {
    pub count: u32,
    pub amount_micro: i64,
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 绑套餐（核销金额取套餐 grant 覆盖面值，可附带加组/余额有效期，#1790-5）。
    #[serde(default)]
    pub plan_code: Option<String>,
    /// 限定核销用户（他人核销与不存在同响应）。
    #[serde(default)]
    pub bind_user_id: Option<i64>,
    /// 同批次单 IP 核销上限（依赖 CDN 头取 IP，直连无头不限）。
    #[serde(default)]
    pub max_per_ip: Option<i32>,
}

/// 批量生成兑换码（明文仅本次返回；核销走门户 /api/me/redeem）。
pub async fn create_redemptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<CreateRedemptionsReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::USER_BALANCE_ADJUST).await?;
    if req.count == 0 || req.count > 1000 {
        return Err(AppError::bad_request().with_param("count_or_amount"));
    }
    ensure_redemption_amount(req.amount_micro)?;
    // 这两项原来靠库约束兜底（CHECK max_per_ip > 0、bind_user_id 外键），违反时回 500
    if req.max_per_ip.is_some_and(|cap| cap <= 0) {
        return Err(AppError::bad_request().with_param("max_per_ip"));
    }
    if let Some(bound) = req.bind_user_id {
        let live = sqlx::query_scalar!(
            r#"SELECT role FROM users WHERE id = $1 AND deleted_at IS NULL"#,
            bound
        )
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        if live.is_none() {
            return Err(AppError::bad_request().with_param("bind_user_id"));
        }
    }
    let codes: Vec<String> = (0..req.count)
        .map(|_| {
            use rand::RngExt;
            use rand::distr::Alphanumeric;
            let body: String = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(24)
                .map(char::from)
                .collect();
            format!("okapi-{body}")
        })
        .collect();
    let batch_id = okapi_store::admin::create_redemption_codes(
        &state.pg,
        actor.actor_user_id(),
        req.amount_micro,
        &codes,
        req.expires_at,
        okapi_store::admin::RedemptionOptions {
            plan_code: req.plan_code.as_deref(),
            bind_user_id: req.bind_user_id,
            max_per_ip: req.max_per_ip,
        },
    )
    .await?
    .ok_or_else(|| AppError::bad_request().with_param("plan_code"))?;
    audit(
        &state,
        &actor,
        "redemption.create",
        &batch_id.to_string(),
        json!({
            "count": req.count, "amount_micro": req.amount_micro,
            "plan_code": req.plan_code, "bind_user_id": req.bind_user_id,
            "max_per_ip": req.max_per_ip,
        }),
    )
    .await;
    Ok(Json(json!({
        "batch_id": batch_id,
        // 明文仅生成时返回一次
        "codes": codes,
    })))
}
