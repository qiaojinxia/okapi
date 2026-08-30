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
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use hmac::{Hmac, KeyInit as _, Mac};
use md5::{Digest as Md5Digest, Md5};
use okapi_providers::custom_pass::{PassRequest, PassResponse};
use rand::RngExt;
use rand::distr::Alphanumeric;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::BTreeMap;

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

/// micro-USD → 分粒度字符串（向上取整到分：网关不少收）。
fn micro_to_decimal_string(micro: i64) -> String {
    let cents = micro.saturating_add(9_999) / 10_000;
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// micro-USD → CNY 分字符串（rate_milli 千分比整数汇率）。
fn micro_usd_to_cny_string(micro: i64, rate_milli: i64) -> String {
    let cny_micro = micro.saturating_mul(rate_milli) / 1000;
    micro_to_decimal_string(cny_micro)
}

// ---- epay 签名（协议既定 MD5：ASCII 升序拼 k=v& + key） ----

fn epay_sign(params: &BTreeMap<&str, String>, key: &str) -> String {
    let mut buf = String::new();
    for (k, v) in params {
        if v.is_empty() || *k == "sign" || *k == "sign_type" {
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

/// POST /api/me/topup：创建订单并返回支付跳转信息。
// 双网关下单线性分支，拆分割裂订单时序
#[allow(clippy::too_many_lines)]
pub async fn topup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TopupReq>,
) -> Result<Json<Value>, AppError> {
    let key = authenticate(&state, &headers).await?;
    if req.amount_micro < MIN_TOPUP_MICRO {
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

    match req.gateway.as_str() {
        "epay" => {
            let cfg: EpayCfg = load_cfg(&state, "payment_epay").await?;
            let money = micro_usd_to_cny_string(req.amount_micro, cfg.usd_to_cny_milli);
            okapi_store::admin::create_recharge_order(
                &state.pg,
                &order_no,
                key.user_id,
                req.amount_micro,
                "epay",
                &money,
                "CNY",
            )
            .await?;
            let mut params: BTreeMap<&str, String> = BTreeMap::new();
            params.insert("pid", cfg.pid.clone());
            params.insert("type", "alipay".to_owned());
            params.insert("out_trade_no", order_no.clone());
            params.insert("name", "okapi_topup".to_owned());
            params.insert("money", money.clone());
            let sign = epay_sign(&params, &cfg.key);
            Ok(Json(json!({
                "order_no": order_no,
                "gateway": "epay",
                "pay_url": cfg.gateway_url,
                // 前端以表单/查询串提交给 epay 网关
                "params": {
                    "pid": cfg.pid, "type": "alipay", "out_trade_no": order_no,
                    "name": "okapi_topup", "money": money,
                    "sign": sign, "sign_type": "MD5",
                },
            })))
        }
        "stripe" => {
            let cfg: StripeCfg = load_cfg(&state, "payment_stripe").await?;
            let usd = micro_to_decimal_string(req.amount_micro);
            okapi_store::admin::create_recharge_order(
                &state.pg,
                &order_no,
                key.user_id,
                req.amount_micro,
                "stripe",
                &usd,
                "USD",
            )
            .await?;
            // 分整数（Stripe unit_amount 为最小货币单位）
            let cents = req.amount_micro.saturating_add(9_999) / 10_000;
            let body = format!(
                "mode=payment&success_url={}&cancel_url={}&metadata[order_no]={}&line_items[0][quantity]=1&line_items[0][price_data][currency]=usd&line_items[0][price_data][unit_amount]={}&line_items[0][price_data][product_data][name]=okapi_topup",
                "https%3A%2F%2Fexample.invalid%2Fok",
                "https%3A%2F%2Fexample.invalid%2Fcancel",
                order_no,
                cents
            );
            let api = cfg
                .api_base
                .as_deref()
                .unwrap_or("https://api.stripe.com")
                .trim_end_matches('/')
                .to_owned();
            let resp = state
                .pass
                .forward(PassRequest {
                    method: axum::http::Method::POST,
                    url: format!("{api}/v1/checkout/sessions"),
                    auth_header: "authorization".to_owned(),
                    auth_value: format!("Bearer {}", cfg.secret_key),
                    content_type: Some("application/x-www-form-urlencoded".to_owned()),
                    body: bytes::Bytes::from(body),
                })
                .await;
            let session = match resp {
                Ok(PassResponse::Ok { mut stream, .. }) => {
                    use futures::StreamExt as _;
                    let mut buf = Vec::new();
                    while let Some(Ok(chunk)) = stream.next().await {
                        buf.extend_from_slice(&chunk);
                    }
                    serde_json::from_slice::<Value>(&buf).map_err(|_| AppError::internal())?
                }
                _ => {
                    return Err(AppError::new(
                        StatusCode::BAD_GATEWAY,
                        "payment_gateway_error",
                    ));
                }
            };
            Ok(Json(json!({
                "order_no": order_no,
                "gateway": "stripe",
                "pay_url": session.get("url").and_then(Value::as_str),
                "session_id": session.get("id").and_then(Value::as_str),
            })))
        }
        _ => Err(AppError::bad_request().with_param("gateway")),
    }
}

// ---- 核销共用 ----

async fn settle_paid_order(
    state: &AppState,
    order_no: &str,
    trade_no: &str,
) -> Result<bool, AppError> {
    let Some((user_id, amount_micro)) =
        okapi_store::admin::mark_recharge_paid(&state.pg, order_no, trade_no).await?
    else {
        // 已核销/不存在：幂等吞掉（回调方期望成功应答停止重试）
        return Ok(false);
    };
    let amount = okapi_domain::Money::from_micros(amount_micro);
    let balance_after = state.ledger.credit(user_id, amount).await?;
    okapi_ledger::pg::record_credit(
        &state.pg,
        user_id,
        amount,
        "recharge",
        "system:payment",
        json!({"tags": ["recharge"], "order_no": order_no, "trade_no": trade_no}),
    )
    .await?;
    aff_reward(state, user_id, amount_micro, order_no).await;
    tracing::info!(
        order_no,
        user_id,
        amount_micro,
        balance_after = balance_after.as_micros(),
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
    let query = query.unwrap_or_default();
    let mut params: BTreeMap<&str, String> = BTreeMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            params.insert(
                match k {
                    "pid" => "pid",
                    "trade_no" => "trade_no",
                    "out_trade_no" => "out_trade_no",
                    "type" => "type",
                    "name" => "name",
                    "money" => "money",
                    "trade_status" => "trade_status",
                    "sign" => "sign",
                    "sign_type" => "sign_type",
                    _ => continue,
                },
                v.to_owned(),
            );
        }
    }
    let given_sign = params.get("sign").cloned().unwrap_or_default();
    let expect = epay_sign(&params, &cfg.key);
    if given_sign != expect {
        return Err(AppError::bad_request().with_param("sign"));
    }
    if params.get("trade_status").map(String::as_str) != Some("TRADE_SUCCESS") {
        return Ok("success".to_owned()); // 非成功态：确认收到，不入账
    }
    let order_no = params
        .get("out_trade_no")
        .ok_or_else(|| AppError::bad_request().with_param("out_trade_no"))?;
    let trade_no = params.get("trade_no").cloned().unwrap_or_default();
    settle_paid_order(&state, order_no, &trade_no).await?;
    Ok("success".to_owned())
}

// ---- Stripe webhook（Stripe-Signature: t=..,v1=HMAC-SHA256(secret, "{t}.{payload}")） ----

pub async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Result<Json<Value>, AppError> {
    let cfg: StripeCfg = load_cfg(&state, "payment_stripe").await?;
    let sig_header = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::bad_request().with_param("stripe_signature"))?;
    let mut ts = "";
    let mut v1 = "";
    for part in sig_header.split(',') {
        if let Some(x) = part.trim().strip_prefix("t=") {
            ts = x;
        } else if let Some(x) = part.trim().strip_prefix("v1=") {
            v1 = x;
        }
    }
    let mut mac = <Hmac<Sha256>>::new_from_slice(cfg.webhook_secret.as_bytes())
        .map_err(|_| AppError::internal())?;
    mac.update(ts.as_bytes());
    mac.update(b".");
    mac.update(&body);
    let expect = hex::encode(mac.finalize().into_bytes());
    if expect != v1 {
        return Err(AppError::bad_request().with_param("stripe_signature"));
    }

    let event: Value = serde_json::from_slice(&body).map_err(|_| AppError::bad_request())?;
    if event.get("type").and_then(Value::as_str) == Some("checkout.session.completed") {
        let session = event.pointer("/data/object").cloned().unwrap_or_default();
        let order_no = session
            .pointer("/metadata/order_no")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::bad_request().with_param("order_no"))?;
        let trade_no = session.get("id").and_then(Value::as_str).unwrap_or("");
        settle_paid_order(&state, order_no, trade_no).await?;
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
    if let Err(err) = state.ledger.credit(inviter_id, money).await {
        tracing::error!(inviter_id, invitee, error = %err, "aff 返利入账失败");
        return;
    }
    if let Err(err) = okapi_ledger::pg::record_credit(
        &state.pg,
        inviter_id,
        money,
        "adjust",
        "system:aff",
        json!({"reason": "aff_reward", "invitee": invitee, "order_no": order_no, "bp": bp}),
    )
    .await
    {
        tracing::error!(inviter_id, error = %err, "aff 返利事件写入失败（对账修复）");
    }
}
