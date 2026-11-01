//! 定价规则、价簿发布与 new-api 倍率导入。
use super::super::query::{PageQuery, Query};
use super::{audit, guard, ratio_fits};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_kernel::extract::Path;
use serde::Deserialize;
use serde_json::{Value, json};

// ---- 定价规则栈（DESIGN §3.4）----

#[derive(Deserialize)]
pub struct UpsertRuleReq {
    pub rule_code: String,
    /// volume | time_based | discount | surge。
    pub rule_type: String,
    /// 命中时施加的乘数（十进制字符串，禁浮点入库）。
    pub multiplier: String,
    /// volume：本月累计 token 阈值（与消费额阈值二者至少一项；同配 = AND）。
    #[serde(default)]
    pub min_monthly_tokens: Option<u64>,
    /// volume：本月累计消费阈值 micro-USD（贵模型大客户用量少但付费多，§11.5）。
    #[serde(default)]
    pub min_monthly_spend_micro: Option<u64>,
    /// time_based 必填：[start, end) 本地分钟窗，允许跨零点回绕。
    #[serde(default)]
    pub start_minute: Option<u16>,
    #[serde(default)]
    pub end_minute: Option<u16>,
    /// time_based 可选：星期列表 0=周日…6=周六（缺省每天；与分钟窗同为机器本地钟源）。
    #[serde(default)]
    pub weekdays: Option<Vec<u8>>,
    /// 多命中叠加语义：stackable（缺省，连乘）/ exclusive（桶内 priority 最高独占）/
    /// best_for_user（桶内取对用户最优一条）。
    #[serde(default)]
    pub stacking_mode: Option<String>,
    /// 作用域选择器 {"groups":[],"models":[],"users":[]}；缺省不限。
    #[serde(default)]
    pub scope: Option<Value>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub valid_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub valid_to: Option<chrono::DateTime<chrono::Utc>>,
}

pub(super) const fn default_true() -> bool {
    true
}

/// 按 rule_type 组装并校验 params——非法组合一律 400，绝不让"配得上却永不命中"
/// 的规则进库（本端点补齐前 pricing_rules 只能手写 SQL，正是该类事故的来源）。
pub(super) fn rule_params(req: &UpsertRuleReq) -> Result<Value, AppError> {
    if req.multiplier.parse::<okapi_pricing::RatioFp>().is_err() {
        return Err(AppError::bad_request().with_param("multiplier"));
    }
    let mut params = serde_json::Map::new();
    params.insert(
        "multiplier".to_owned(),
        Value::String(req.multiplier.clone()),
    );
    match req.rule_type.as_str() {
        "volume" => {
            // 双阈值轴至少一项 > 0；两轴全空的 volume 就是无条件规则冒充，拒绝
            let tokens = req.min_monthly_tokens.unwrap_or(0);
            let spend = req.min_monthly_spend_micro.unwrap_or(0);
            if tokens == 0 && spend == 0 {
                return Err(AppError::bad_request().with_param("volume_threshold"));
            }
            if tokens > 0 {
                params.insert("min_monthly_tokens".to_owned(), json!(tokens));
            }
            if spend > 0 {
                params.insert("min_monthly_spend_micro".to_owned(), json!(spend));
            }
        }
        "time_based" => {
            let (start, end) = req
                .start_minute
                .zip(req.end_minute)
                .ok_or_else(|| AppError::bad_request().with_param("minute_window"))?;
            if start >= 1440 || end >= 1440 {
                return Err(AppError::bad_request().with_param("minute_window"));
            }
            // start == end 是空窗（永不命中），对齐 new-api rc.27 #6934 的反面语义：
            // 与其静默退化成全天生效，不如在配置期就拒绝
            if start == end {
                return Err(AppError::bad_request().with_param("minute_window_empty"));
            }
            params.insert("start_minute".to_owned(), json!(start));
            params.insert("end_minute".to_owned(), json!(end));
            if let Some(days) = &req.weekdays {
                // 空列表/非法值与"缺省每天"是三回事：空掩码永不命中，属配置错误
                let mask = okapi_pricing::WeekdayMask::from_days(days)
                    .ok_or_else(|| AppError::bad_request().with_param("weekdays"))?;
                params.insert("weekdays".to_owned(), json!(mask.days()));
            }
        }
        "discount" | "surge" => {}
        _ => return Err(AppError::bad_request().with_param("rule_type")),
    }
    if let Some(raw) = req.stacking_mode.as_deref() {
        let mode = okapi_pricing::Stacking::parse(raw)
            .ok_or_else(|| AppError::bad_request().with_param("stacking_mode"))?;
        // 缺省值不落库：params 里只存显式偏离，与 weekdays 同一策略
        if mode != okapi_pricing::Stacking::Stackable {
            params.insert("stacking_mode".to_owned(), json!(mode.tag()));
        }
    }
    Ok(Value::Object(params))
}

/// 定价规则 upsert（改动后需 publish 才进价簿；与模型/分组同一发布闸）。
pub async fn upsert_pricing_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpsertRuleReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    // 规则码是主键（varchar(64)）：与分组码等其它机器标识同一道校验，空串 / 超长 / 怪字符都 400
    let rule_code = super::super::identifiers::ensure_code("rule_code", &req.rule_code, 64)?;
    let params = rule_params(&req)?;
    let scope = req.scope.clone().unwrap_or_else(|| json!({}));
    if !scope.is_object() {
        return Err(AppError::bad_request().with_param("scope"));
    }
    if req
        .valid_from
        .zip(req.valid_to)
        .is_some_and(|(f, t)| f >= t)
    {
        return Err(AppError::bad_request().with_param("valid_window"));
    }
    okapi_store::admin::upsert_pricing_rule(
        &state.pg,
        okapi_store::admin::PricingRuleInput {
            rule_code,
            rule_type: &req.rule_type,
            scope: &scope,
            params: &params,
            priority: req.priority,
            enabled: req.enabled,
            valid_from: req.valid_from,
            valid_to: req.valid_to,
        },
    )
    .await?;
    audit(
        &state,
        &actor,
        "pricing.upsert_rule",
        rule_code,
        json!({ "rule_type": req.rule_type, "params": params, "enabled": req.enabled }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// 规则列表（按生效叠加序返回，管理端据此核对多规则连乘结果）；`limit/offset` 可选切片。
pub async fn list_pricing_rules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let page = okapi_store::admin::list_pricing_rules(&state.pg, q.slice()).await?;
    let data: Vec<Value> = page
        .data
        .into_iter()
        .map(|r| {
            json!({
                "rule_code": r.rule_code,
                "rule_type": r.rule_type,
                "scope": r.scope,
                "params": r.params,
                "priority": r.priority,
                "enabled": r.enabled,
                "valid_from": r.valid_from,
                "valid_to": r.valid_to,
            })
        })
        .collect();
    Ok(Json(json!({ "data": data, "total": page.total })))
}

pub async fn delete_pricing_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(rule_code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    if !okapi_store::admin::delete_pricing_rule(&state.pg, &rule_code).await? {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    }
    audit(&state, &actor, "pricing.delete_rule", &rule_code, json!({})).await;
    Ok(Json(json!({ "ok": true, "requires_publish": true })))
}

/// 发布定价 epoch：**发布前全量编译校验（fail-closed，DESIGN §3.3）**——
/// 配置编译不过即拒绝发布；snapshot 存发布时刻配置全量（历史/回滚/diff）。
#[derive(Deserialize, Default)]
pub struct PublishPricingQuery {
    pub expected_base_per_1m_micro: Option<i64>,
}

pub async fn publish_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PublishPricingQuery>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_PUBLISH).await?;

    let rows = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let base = okapi_kernel::pricing_loader::draft_base_price(&state.pg).await?;
    if query
        .expected_base_per_1m_micro
        .is_some_and(|expected| expected != base)
    {
        return Err(AppError::bad_request().with_param("pricing_base_changed"));
    }
    if let Err(err) = okapi_kernel::pricing_loader::compile_rows(&rows, base) {
        tracing::warn!(error = %err, "定价配置编译失败，拒绝发布");
        return Err(AppError::bad_request().with_param(format!("compile: {err}")));
    }
    let mut snapshot = serde_json::to_value(&rows).map_err(|_| AppError::internal())?;
    snapshot["base_price_per_1m_micro"] = json!(base);

    let epoch =
        okapi_store::admin::publish_epoch(&state.pg, actor.actor_user_id(), &snapshot).await?;
    // 广播（best-effort；丢失由 gateway 30s 轮询兜底）
    if let Some(nats) = &state.nats {
        let _ = nats
            .publish("pricing.epoch", epoch.to_string().into())
            .await;
    }
    audit(
        &state,
        &actor,
        "pricing.publish",
        &epoch.to_string(),
        json!({}),
    )
    .await;
    Ok(Json(json!({ "epoch": epoch })))
}

#[derive(Deserialize)]
pub struct ImportNewApiReq {
    #[serde(default)]
    pub model_ratio: serde_json::Map<String, Value>,
    #[serde(default)]
    pub completion_ratio: serde_json::Map<String, Value>,
    #[serde(default)]
    pub cache_ratio: serde_json::Map<String, Value>,
    /// new-api 缓存**写入**倍率（其键名为 create_cache_ratio）。
    #[serde(default, alias = "cache_write_ratio")]
    pub create_cache_ratio: serde_json::Map<String, Value>,
    /// new-api 音频输入倍率（相对文本）。
    #[serde(default)]
    pub audio_ratio: serde_json::Map<String, Value>,
    /// new-api 音频输出倍率（叠乘在 audio_ratio 之上）。
    #[serde(default)]
    pub audio_completion_ratio: serde_json::Map<String, Value>,
    /// new-api 图片输入倍率。
    #[serde(default)]
    pub image_ratio: serde_json::Map<String, Value>,
    /// new-api 按次价（USD）。
    #[serde(default)]
    pub model_price: serde_json::Map<String, Value>,
}

/// JSON 值 → 十进制字面量（数字经 ryu 最短表示 roundtrip，字符串原样）。
pub(super) fn ratio_literal(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// new-api 官方 ratio JSON 一键导入（M3 验收项）：
/// model_ratio 为主表（completion/cache 缺省 1），model_price → per_call；
/// 非法值跳过并报告；导入后不自动发布 epoch（管理员 review 后手动 publish）。
pub async fn import_newapi_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<ImportNewApiReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    let mut imported: i64 = 0;
    let mut skipped: Vec<String> = Vec::new();

    for (model, v) in &req.model_ratio {
        let Some(model_ratio) = ratio_literal(v) else {
            skipped.push(model.clone());
            continue;
        };
        let completion = req
            .completion_ratio
            .get(model)
            .and_then(ratio_literal)
            .unwrap_or_else(|| "1".to_owned());
        let cache = req
            .cache_ratio
            .get(model)
            .and_then(ratio_literal)
            .unwrap_or_else(|| "1".to_owned());
        let cache_write = req
            .create_cache_ratio
            .get(model)
            .and_then(ratio_literal)
            .unwrap_or_else(|| "1".to_owned());
        let pick = |m: &serde_json::Map<String, Value>| {
            m.get(model)
                .and_then(ratio_literal)
                .unwrap_or_else(|| "1".to_owned())
        };
        let audio = pick(&req.audio_ratio);
        let audio_completion = pick(&req.audio_completion_ratio);
        let image = pick(&req.image_ratio);
        // 复用定价域解析器做入库前校验（纯整数定点，禁浮点），并按列宽卡上限：
        // 解析得过却放不进 numeric(12,6) 的值落库时才溢出，会让导入在中途 500、留下前半批
        if ![
            &model_ratio,
            &completion,
            &cache,
            &cache_write,
            &audio,
            &audio_completion,
            &image,
        ]
        .iter()
        .all(|literal| ratio_fits(literal))
        {
            skipped.push(model.clone());
            continue;
        }
        okapi_store::admin::upsert_model_ratio(
            &state.pg,
            model,
            okapi_store::admin::RatioAxes {
                model: &model_ratio,
                completion: &completion,
                cache: &cache,
                cache_write: &cache_write,
                audio: &audio,
                audio_completion: &audio_completion,
                image: &image,
                modality_ratios: None,
            },
        )
        .await?;
        imported += 1;
    }

    for (model, v) in &req.model_price {
        let Some(literal) = ratio_literal(v) else {
            skipped.push(model.clone());
            continue;
        };
        // USD 十进制 → micro（scale=1e6 定点解析，恰为 micro-USD），与单模型写入同一条上限
        let Some(price) = literal
            .parse::<okapi_pricing::RatioFp>()
            .ok()
            .map(okapi_pricing::RatioFp::as_scaled)
            .filter(|micro| *micro <= okapi_ledger::holds::MAXIMUM_MICROS)
        else {
            skipped.push(model.clone());
            continue;
        };
        okapi_store::admin::upsert_model_per_call(&state.pg, model, price).await?;
        imported += 1;
    }

    audit(
        &state,
        &actor,
        "pricing.import_newapi",
        "batch",
        json!({ "imported": imported, "skipped": skipped }),
    )
    .await;
    Ok(Json(
        json!({ "imported": imported, "skipped": skipped, "published": false }),
    ))
}
