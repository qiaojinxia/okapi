//! 自助充值支付闭环（IMPLEMENTATION §11.2 / §13-M4）：
//! epay 聚合（MD5 签名跳转 + 异步回调）与 Stripe Checkout（session 外呼 + webhook 验签）。
//! 回调幂等 = recharge_orders 状态机单向（0→1 行级原子恰一次）→ credit
//! （event_type=recharge，actor=system:payment）。
//!
//! settings：
//!   payment_epay   = {gateway_url, pid, key, usd_to_cny_milli?=7000}
//!   payment_stripe = {secret_key, webhook_secret, api_base?=https://api.stripe.com}

use crate::gateway::auth::authenticate;
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use md5::{Digest as Md5Digest, Md5};
use okapi_providers::custom_pass::{PassRequest, PassResponse};
use okapi_store::payments::Acceptance;
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Connection;
use std::collections::BTreeMap;

mod validation;

const MIN_TOPUP_MICRO: i64 = 1_000_000; // $1 起充

// ---- 配置 ----

#[derive(Deserialize)]
struct EpayCfg {
    gateway_url: String,
    pid: String,
    key: String,
    #[serde(default = "default_rate")]
    usd_to_cny_milli: i64,
}

fn default_rate() -> i64 {
    7000
}

#[derive(Deserialize)]
struct StripeCfg {
    secret_key: String,
    webhook_secret: String,
    #[serde(default)]
    api_base: Option<String>,
}

async fn load_cfg<T: serde::de::DeserializeOwned>(
    state: &AppState,
    key: &str,
) -> Result<T, AppError> {
    let raw = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = $1"#, key)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, "payment_not_configured"))?;
    serde_json::from_value(raw).map_err(|_| AppError::internal())
}

// ---- 金额换算（纯整数，禁浮点） ----

/// Convert micro-USD and a milli exchange rate directly to cents. The wider
/// integer intermediate avoids saturation; round once, upwards, at the cent.
fn quote_cents(micro: i64, rate_milli: i64) -> Result<i128, AppError> {
    if rate_milli <= 0 {
        return Err(AppError::bad_request().with_param("usd_to_cny_milli"));
    }
    i128::from(micro)
        .checked_mul(i128::from(rate_milli))
        .and_then(|amount| amount.checked_add(9_999_999))
        .map(|amount| amount / 10_000_000)
        // recharge_orders.pay_amount is NUMERIC(12,2). Reject outside its
        // range before creating an order; never truncate the amount to fit.
        .filter(|cents| (1..=999_999_999_999).contains(cents))
        .ok_or_else(|| AppError::bad_request().with_param("amount_micro"))
}
fn decimal_cents(cents: i128) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

// ---- epay 签名（协议既定 MD5：ASCII 升序拼 k=v& + key） ----

fn epay_sign<K: AsRef<str> + Ord>(params: &BTreeMap<K, String>, key: &str) -> String {
    let mut buf = String::new();
    for (k, v) in params {
        let k = k.as_ref();
        if v.is_empty() || k == "sign" || k == "sign_type" {
            continue;
        }
        if !buf.is_empty() {
            buf.push('&');
        }
        buf.push_str(k);
        buf.push('=');
        buf.push_str(v);
    }
    buf.push_str(key);
    hex::encode(Md5::digest(buf.as_bytes()))
}

// ---- 下单 ----

#[derive(Deserialize)]
pub struct TopupReq {
    pub amount_micro: i64,
    /// epay | stripe
    pub gateway: String,
}

/// POST /api/me/topup：创建钱包充值订单并返回支付跳转信息。
pub async fn topup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<TopupReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    if req.amount_micro < MIN_TOPUP_MICRO {
        return Err(AppError::bad_request().with_param("amount_micro"));
    }
    let order = place_order(
        &state,
        key.user_id,
        req.amount_micro,
        &req.gateway,
        None,
        "okapi_topup",
    )
    .await?;
    Ok(Json(order))
}

/// 下单公共体（钱包充值与订阅购买共用，§11.28）：建 `recharge_orders` 行 + 网关跳转信息。
/// `plan_id` 非空 = 订阅购买单（`amount_micro` 为售价快照，回调激活订阅而不入钱包）。
/// `item_name` 是支付页上的商品名，按表单协议编码。
// 双网关下单线性分支，拆分割裂订单时序
#[allow(clippy::too_many_lines)]
pub async fn place_order(
    state: &AppState,
    user_id: i64,
    amount_micro: i64,
    gateway: &str,
    plan: Option<&okapi_store::subscriptions::SubPlan>,
    item_name: &str,
) -> Result<Value, AppError> {
    if !(1..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&amount_micro) {
        return Err(AppError::bad_request().with_param("amount_micro"));
    }
    let order_no = format!(
        "okp{}{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        rand::rng()
            .sample_iter(&Alphanumeric)
            .take(8)
            .map(char::from)
            .collect::<String>()
    );

    match gateway {
        "epay" => {
            let cfg: EpayCfg = load_cfg(state, "payment_epay").await?;
            let money = decimal_cents(quote_cents(amount_micro, cfg.usd_to_cny_milli)?);
            okapi_store::admin::create_recharge_order(
                &state.pg,
                okapi_store::admin::NewOrder {
                    order_no: &order_no,
                    user_id,
                    amount_micro,
                    gateway: "epay",
                    pay_amount: &money,
                    currency: "CNY",
                    merchant_id: Some(&cfg.pid),
                    plan_id: plan.map(|p| p.id),
                    subscription_snapshot: plan
                        .map(serde_json::to_value)
                        .transpose()
                        .map_err(|_| AppError::internal())?,
                },
            )
            .await?;
            let mut params: BTreeMap<&str, String> = BTreeMap::new();
            params.insert("pid", cfg.pid.clone());
            params.insert("type", "alipay".to_owned());
            params.insert("out_trade_no", order_no.clone());
            params.insert("name", item_name.to_owned());
            params.insert("money", money.clone());
            let sign = epay_sign(&params, &cfg.key);
            Ok(json!({
                "order_no": order_no,
                "gateway": "epay",
                "pay_url": cfg.gateway_url,
                // 前端以表单/查询串提交给 epay 网关
                "params": {
                    "pid": cfg.pid, "type": "alipay", "out_trade_no": order_no,
                    "name": item_name, "money": money,
                    "sign": sign, "sign_type": "MD5",
                },
            }))
        }
        "stripe" => {
            let cfg: StripeCfg = load_cfg(state, "payment_stripe").await?;
            let cents = quote_cents(amount_micro, 1000)?;
            let usd = decimal_cents(cents);
            okapi_store::admin::create_recharge_order(
                &state.pg,
                okapi_store::admin::NewOrder {
                    order_no: &order_no,
                    user_id,
                    amount_micro,
                    gateway: "stripe",
                    pay_amount: &usd,
                    currency: "USD",
                    merchant_id: None,
                    plan_id: plan.map(|p| p.id),
                    subscription_snapshot: plan
                        .map(serde_json::to_value)
                        .transpose()
                        .map_err(|_| AppError::internal())?,
                },
            )
            .await?;
            // 分整数（Stripe unit_amount 为最小货币单位）
            let mut form = reqwest::Url::parse("https://checkout.invalid")
                .map_err(|_| AppError::internal())?;
            form.query_pairs_mut().extend_pairs([
                ("mode", "payment"),
                ("success_url", "https://example.invalid/ok"),
                ("cancel_url", "https://example.invalid/cancel"),
                ("metadata[order_no]", &order_no),
                ("line_items[0][quantity]", "1"),
                ("line_items[0][price_data][currency]", "usd"),
                ("line_items[0][price_data][unit_amount]", &cents.to_string()),
                ("line_items[0][price_data][product_data][name]", item_name),
            ]);
            let body = form.query().ok_or_else(AppError::internal)?.to_owned();
            let api = cfg
                .api_base
                .as_deref()
                .unwrap_or("https://api.stripe.com")
                .trim_end_matches('/')
                .to_owned();
            super::ssrf::validate_api_base(state, &api).await?;
            let resp = state
                .pass
                .probe(PassRequest {
                    method: axum::http::Method::POST,
                    url: format!("{api}/v1/checkout/sessions"),
                    auth_header: "authorization".to_owned(),
                    auth_value: format!("Bearer {}", cfg.secret_key),
                    content_type: Some("application/x-www-form-urlencoded".to_owned()),
                    body: bytes::Bytes::from(body),
                    proxy_url: None,
                    extra_headers: vec![("idempotency-key".to_owned(), order_no.clone())],
                })
                .await;
            let session = match resp {
                Ok(PassResponse::Ok { mut stream, .. }) => {
                    use futures::StreamExt as _;
                    let mut buf = Vec::new();
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk.map_err(|_| validation::gateway_error())?;
                        if buf.len().saturating_add(chunk.len()) > 1_048_576 {
                            return Err(validation::gateway_error());
                        }
                        buf.extend_from_slice(&chunk);
                    }
                    serde_json::from_slice::<Value>(&buf)
                        .map_err(|_| validation::gateway_error())?
                }
                _ => {
                    return Err(AppError::new(
                        StatusCode::BAD_GATEWAY,
                        "payment_gateway_error",
                    ));
                }
            };
            let (session_id, pay_url) = validation::checkout_response(&session)?;
            if !okapi_store::payments::bind_checkout(&state.pg, &order_no, session_id).await? {
                return Err(validation::gateway_error());
            }
            Ok(json!({
                "order_no": order_no,
                "gateway": "stripe",
                "pay_url": pay_url,
                "session_id": session_id,
            }))
        }
        _ => Err(AppError::bad_request().with_param("gateway")),
    }
}

// ---- 核销共用 ----

async fn settle_paid_order(
    state: &AppState,
    order_no: &str,
    proof: &okapi_store::payments::PaymentProof<'_>,
) -> Result<bool, AppError> {
    let Some(user_id) = sqlx::query_scalar!(
        "SELECT user_id FROM recharge_orders WHERE order_no=$1",
        order_no
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?
    else {
        return Err(AppError::new(StatusCode::NOT_FOUND, "not_found").with_param("order_no"));
    };
    let mut guard = okapi_ledger::holds::UserGuard::acquire(&state.pg, user_id).await?;
    let mut tx = guard
        .connection()?
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    let order = match okapi_store::payments::accept_in_tx(&mut tx, order_no, proof).await? {
        Acceptance::Applied(order) => order,
        Acceptance::Duplicate => return Ok(false),
        Acceptance::Missing => {
            return Err(AppError::new(StatusCode::NOT_FOUND, "not_found").with_param("order_no"));
        }
        Acceptance::Mismatch(field) => return Err(AppError::bad_request().with_param(field)),
        Acceptance::Conflict(field) => {
            return Err(AppError::new(StatusCode::CONFLICT, "bad_request").with_param(field));
        }
        Acceptance::AwaitingSession => {
            return Err(
                AppError::new(StatusCode::SERVICE_UNAVAILABLE, "payment_gateway_error")
                    .with_param("session_id"),
            );
        }
    };
    let trade_no = proof.trade_no;
    let user_id = order.user_id;
    let amount_micro = order.amount_micro;
    if order.plan_id.is_some() {
        let plan: okapi_store::subscriptions::SubPlan =
            serde_json::from_value(order.subscription_snapshot.ok_or_else(AppError::internal)?)
                .map_err(|_| AppError::internal())?;
        let id = okapi_ledger::subscriptions::enqueue(
            &mut tx,
            user_id,
            &plan,
            &format!("purchase:{order_no}"),
            "system:payment",
            true,
        )
        .await?;
        tx.commit().await.map_err(okapi_store::StoreError::from)?;
        okapi_ledger::subscriptions::finish(&mut guard, &state.ledger, user_id, id).await;
        drop(guard);
        state.sched.auth_flush().await;
        aff_reward(state, user_id, amount_micro, order_no).await;
        return Ok(true);
    }
    let amount = okapi_domain::Money::from_micros(amount_micro);
    let operation_id = okapi_ledger::transfers::credit_in_tx(
        &mut tx,
        user_id,
        amount,
        "recharge",
        "system:payment",
        json!({"tags": ["recharge"], "order_no": order_no, "trade_no": trade_no}),
    )
    .await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    let receipt = okapi_ledger::transfers::finish(
        &mut guard,
        &state.ledger,
        user_id,
        operation_id,
        okapi_ledger::Pool::Wallet,
    )
    .await;
    drop(guard);
    aff_reward(state, user_id, amount_micro, order_no).await;
    tracing::info!(
        order_no,
        user_id,
        amount_micro,
        balance_after = ?receipt.balance_after.map(okapi_domain::Money::as_micros),
        operation_id = %receipt.operation_id,
        "充值入账"
    );
    Ok(true)
}

// ---- epay 异步回调（GET 查询串，验 MD5，应答纯文本 "success"） ----

pub async fn epay_callback(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<String, AppError> {
    let cfg: EpayCfg = load_cfg(&state, "payment_epay").await?;
    let params = validation::epay_params(query.as_deref().unwrap_or_default())?;
    validation::epay_signature(&params, &cfg.key)?;
    if params.get("trade_status").map(String::as_str) != Some("TRADE_SUCCESS") {
        return Ok("success".to_owned());
    }
    let order_no = validation::required(&params, "out_trade_no", 64)?;
    let trade_no = validation::required(&params, "trade_no", 128)?;
    let merchant_id = validation::required(&params, "pid", 128)?;
    if merchant_id != cfg.pid {
        return Err(AppError::bad_request().with_param("pid"));
    }
    let amount_minor = validation::money_minor(validation::required(&params, "money", 32)?)?;
    settle_paid_order(
        &state,
        order_no,
        &okapi_store::payments::PaymentProof {
            gateway: "epay",
            merchant_id,
            trade_no,
            currency: "CNY",
            amount_minor,
        },
    )
    .await?;
    Ok("success".to_owned())
}

/// Stripe validates the raw body before decoding; delayed payment methods are
/// fulfilled on async success, never merely because Checkout was completed.
pub async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Result<Json<Value>, AppError> {
    let cfg: StripeCfg = load_cfg(&state, "payment_stripe").await?;
    let mut signature_headers = headers.get_all("stripe-signature").iter();
    let signature = signature_headers
        .next()
        .and_then(|v| v.to_str().ok())
        .filter(|_| signature_headers.next().is_none())
        .ok_or_else(|| AppError::bad_request().with_param("stripe_signature"))?;
    validation::stripe_signature(signature, &body, &cfg.webhook_secret)?;
    if let Some(session) = validation::paid_session(&body)? {
        settle_paid_order(
            &state,
            &session.metadata.order_no,
            &okapi_store::payments::PaymentProof {
                gateway: "stripe",
                merchant_id: "",
                trade_no: &session.id,
                currency: &session.currency,
                amount_minor: session.amount_total,
            },
        )
        .await?;
    }
    Ok(Json(json!({"received": true})))
}

/// 邀请返利（M4 aff）：充值核销成功后给邀请人按 settings.aff_percent_bp（基点）返利。
/// 缺省 0 = 关闭；仅充值触发（兑换码核销不返利，防套利）；失败不阻断充值主流程。
async fn aff_reward(state: &AppState, invitee: i64, amount_micro: i64, order_no: &str) {
    let bp = sqlx::query_scalar!(
        r#"SELECT (value #>> '{}')::bigint AS "v!" FROM settings WHERE key = 'aff_percent_bp'"#
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .unwrap_or(0);
    if bp <= 0 {
        return;
    }
    let inviter_id = sqlx::query_scalar!(
        r#"SELECT inviter_id FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        invitee
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .flatten();
    let Some(inviter_id) = inviter_id else {
        return;
    };
    let reward = amount_micro.saturating_mul(bp.min(10_000)) / 10_000;
    if reward <= 0 {
        return;
    }
    let money = okapi_domain::Money::from_micros(reward);
    if let Err(err) = okapi_ledger::operations::credit(
        &state.pg,
        &state.ledger,
        inviter_id,
        money,
        "adjust",
        "system:aff",
        json!({"reason": "aff_reward", "invitee": invitee, "order_no": order_no, "bp": bp}),
    )
    .await
    {
        tracing::error!(inviter_id, error = %err, "aff 返利入账失败（需核对账本）");
    }
}
