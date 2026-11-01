//! 模型与模型定价的写入（草稿校验、价格模式选择）。
use super::{audit, guard, ratio_fits};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct UpsertModelReq {
    pub model_name: String,
    #[serde(default)]
    pub metadata: Option<okapi_store::model_config::ModelMetadata>,
    #[serde(default)]
    pub pricing_mode: Option<String>,
    #[serde(default)]
    pub per_call_price_micro: Option<i64>,
    /// 倍率一律十进制字符串（精确入库，禁浮点）。
    pub model_ratio: String,
    #[serde(default = "default_one")]
    pub completion_ratio: String,
    #[serde(default = "default_one")]
    pub cache_ratio: String,
    /// 缓存写入倍率（Anthropic cache_creation；官方 1.25×@5m / 2.0×@1h，缺省 1=按常规输入计）。
    /// 兼容 new-api 键名 `create_cache_ratio`。
    #[serde(default = "default_one", alias = "create_cache_ratio")]
    pub cache_write_ratio: String,
    /// 音频输入倍率（相对文本；gpt-4o-audio 官方 16）。
    #[serde(default = "default_one")]
    pub audio_ratio: String,
    /// 音频输出倍率（叠乘在 audio_ratio 之上；官方 2）。缺省 1 且 audio_ratio 也为 1 时，
    /// 音频输出回落到 completion_ratio（详见 DESIGN §3.2）。
    #[serde(default = "default_one")]
    pub audio_completion_ratio: String,
    /// 图片输入倍率（相对文本）。
    #[serde(default = "default_one")]
    pub image_ratio: String,
    /// Independent cache/modal prices as decimal strings. Omit to preserve; {} clears.
    #[serde(default)]
    pub modality_ratios: Option<Value>,
    /// Independent native tool prices. Omit to preserve; {} clears the draft.
    #[serde(default)]
    pub server_tool_prices: Option<Value>,
    /// service_tier 档位倍率（如 {"flex":"0.5","priority":"2.0"}；
    /// None=不改动，空对象=清除，DESIGN §3-4.5）。
    #[serde(default)]
    pub tier_ratios: Option<serde_json::Map<String, Value>>,
    /// 阶梯计价表 `"0:2.5,128000:5"`（from_tokens:USD_per_1M，首档须从 0 起、严格升序）。
    /// 给非空串 = 切 tiered 模式（`model_ratio` 由阶梯查表替代，其余倍率轴照旧）；
    /// 给空串 = 切回 ratio；None = 不改动模式。
    #[serde(default)]
    pub tier_expr: Option<String>,
    /// 模型级降级链（DESIGN §3.4.1）：零可用候选时按序改投。
    /// None=不改动，空数组=清除。条目须为已存在的模型名——写入时校验，
    /// 拼错的降级模型只会在最脆弱的时刻（主模型已全挂）暴露，必须前置拦截。
    #[serde(default)]
    pub fallback_models: Option<Vec<String>>,
}

pub(super) fn default_one() -> String {
    "1".to_owned()
}

pub(super) fn validate_model_draft(req: &UpsertModelReq) -> Result<(), AppError> {
    // 模型名合法地带 `:`、`/`、`@`（Bedrock `…-v1:0`、Vertex `…@20240620`），不套机器标识字符集
    super::super::identifiers::ensure_display_name("model_name", &req.model_name, 128)?;
    if let Some(meta) = &req.metadata {
        meta.validate()
            .map_err(|param| AppError::bad_request().with_param(param))?;
    }
    // 倍率字面量校验（复用定价域解析器，非法值 400）
    for literal in [
        &req.model_ratio,
        &req.completion_ratio,
        &req.cache_ratio,
        &req.cache_write_ratio,
        &req.audio_ratio,
        &req.audio_completion_ratio,
        &req.image_ratio,
    ] {
        if !ratio_fits(literal) {
            return Err(AppError::bad_request().with_param("ratio"));
        }
    }
    if let Some(value) = &req.modality_ratios {
        okapi_pricing::ModalityRatios::parse(value)
            .map_err(|param| AppError::bad_request().with_param(param))?;
    }
    if let Some(value) = &req.server_tool_prices {
        okapi_pricing::ServerToolPrices::parse(value)
            .map_err(|param| AppError::bad_request().with_param(param))?;
    }
    // Validate all supplied price axes before changing the model row.
    if let Some(tiers) = &req.tier_ratios
        && (tiers.len() > 32
            || tiers.iter().any(|(key, v)| {
                key.is_empty()
                    || key.trim() != key
                    || key.len() > 64
                    || v.as_str()
                        .is_none_or(|s| s.parse::<okapi_pricing::RatioFp>().is_err())
            }))
    {
        return Err(AppError::bad_request().with_param("tier_ratios"));
    }
    Ok(())
}

pub(super) fn model_price_selection(
    req: &UpsertModelReq,
) -> Result<(Option<&str>, Option<&str>), AppError> {
    // Legacy tier_expr selects a mode only when supplied; omission truly preserves it.
    let mode = req.pricing_mode.as_deref().or_else(|| {
        req.tier_expr.as_deref().map(|e| {
            if e.trim().is_empty() {
                "ratio"
            } else {
                "tiered"
            }
        })
    });
    let tier_expr = req
        .tier_expr
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    match mode {
        Some("tiered") => {
            let expr = tier_expr.ok_or_else(|| AppError::bad_request().with_param("tier_expr"))?;
            okapi_pricing::TierTable::check_expr(expr).map_err(|reason| {
                AppError::bad_request().with_param(format!("tier_expr:{reason}"))
            })?;
        }
        Some("per_call")
            if req
                .per_call_price_micro
                .is_none_or(|v| !(0..=okapi_ledger::holds::MAXIMUM_MICROS).contains(&v)) =>
        {
            return Err(AppError::bad_request().with_param("per_call_price_micro"));
        }
        Some("ratio" | "per_call") | None => {}
        _ => return Err(AppError::bad_request().with_param("pricing_mode")),
    }
    Ok((mode, tier_expr))
}

pub async fn upsert_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpsertModelReq>,
) -> Result<Json<Value>, AppError> {
    let actor = guard(&state, &headers, permissions::PRICING_WRITE).await?;
    validate_model_draft(&req)?;
    let (mode, tier_expr) = model_price_selection(&req)?;
    let name = req.model_name.trim();
    let axes = okapi_store::admin::RatioAxes {
        model: &req.model_ratio,
        completion: &req.completion_ratio,
        cache: &req.cache_ratio,
        cache_write: &req.cache_write_ratio,
        audio: &req.audio_ratio,
        audio_completion: &req.audio_completion_ratio,
        image: &req.image_ratio,
        modality_ratios: req.modality_ratios.as_ref(),
    };
    let tiers = req.tier_ratios.as_ref().map(|v| Value::Object(v.clone()));
    let model_id = okapi_store::model_config::save(
        &state.pg,
        okapi_store::model_config::ModelDraft {
            name,
            axes,
            mode,
            tier_expr,
            per_call_price_micro: req.per_call_price_micro,
            tier_ratios: tiers.as_ref(),
            server_tool_prices: req.server_tool_prices.as_ref(),
            fallbacks: req.fallback_models.as_deref(),
            metadata: req.metadata.as_ref(),
        },
    )
    .await
    .map_err(|e| match e {
        okapi_store::StoreError::InvalidData(param) => AppError::bad_request().with_param(param),
        e => e.into(),
    })?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "pricing.upsert_model",
        &req.model_name,
        json!({
            "model_ratio": req.model_ratio,
            "completion_ratio": req.completion_ratio,
            "cache_ratio": req.cache_ratio,
            "cache_write_ratio": req.cache_write_ratio,
            "audio_ratio": req.audio_ratio,
            "audio_completion_ratio": req.audio_completion_ratio,
            "image_ratio": req.image_ratio,
            "modality_ratios": req.modality_ratios,
            "server_tool_prices": req.server_tool_prices,
            "metadata": req.metadata,
            "pricing_mode": mode,
            "per_call_price_micro": req.per_call_price_micro,
            "tier_ratios": req.tier_ratios,
            "tier_expr": req.tier_expr,
            "fallback_models": req.fallback_models,
        }),
    )
    .await;
    Ok(Json(
        json!({ "model_id": model_id, "requires_publish": true }),
    ))
}

#[cfg(test)]
mod model_draft_tests {
    use super::super::ensure_redemption_amount;
    use super::*;

    /// 倍率列是 numeric(12,6)：能解析但放不进的值要在入口挡掉，不能等落库溢出成 500。
    #[test]
    fn ratio_literals_must_fit_the_column() {
        for fits in ["0", "1", "0.000001", "999999.999999", " 2.5 "] {
            assert!(ratio_fits(fits), "{fits}");
        }
        for too_big in ["1000000", "1234567", "999999.9999991", "-1", "", "abc"] {
            assert!(!ratio_fits(too_big), "{too_big}");
        }
    }

    #[test]
    fn redemption_amounts_stay_inside_the_ledger_range() {
        assert!(ensure_redemption_amount(1).is_ok());
        assert!(ensure_redemption_amount(okapi_ledger::holds::MAXIMUM_MICROS).is_ok());
        for bad in [0, -5, okapi_ledger::holds::MAXIMUM_MICROS + 1, i64::MAX] {
            assert!(ensure_redemption_amount(bad).is_err(), "{bad}");
        }
    }

    fn request(extra: Value) -> UpsertModelReq {
        let mut body = json!({"model_name":"test-model", "model_ratio":"1"});
        let Value::Object(extra) = extra else {
            panic!("model draft fixture must be an object")
        };
        body.as_object_mut().unwrap().extend(extra);
        serde_json::from_value(body).unwrap()
    }

    #[test]
    fn omission_preserves_mode_and_explicit_modes_are_validated() {
        assert_eq!(
            model_price_selection(&request(json!({}))).unwrap(),
            (None, None)
        );
        assert_eq!(
            model_price_selection(&request(json!({"tier_expr":""}))).unwrap(),
            (Some("ratio"), None)
        );
        assert!(model_price_selection(&request(json!({"pricing_mode":"per_call"}))).is_err());
        assert!(
            model_price_selection(&request(
                json!({"pricing_mode":"per_call", "per_call_price_micro":-1})
            ))
            .is_err()
        );
        assert!(
            model_price_selection(&request(
                json!({"pricing_mode":"per_call", "per_call_price_micro":0})
            ))
            .is_ok()
        );
        // 账本 / Lua 的定量上限：超出的单价每笔请求都在入账处失败
        assert!(
            model_price_selection(&request(
                json!({"pricing_mode":"per_call", "per_call_price_micro":okapi_ledger::holds::MAXIMUM_MICROS})
            ))
            .is_ok()
        );
        assert!(
            model_price_selection(&request(
                json!({"pricing_mode":"per_call", "per_call_price_micro":okapi_ledger::holds::MAXIMUM_MICROS + 1})
            ))
            .is_err()
        );
        assert!(
            model_price_selection(&request(
                json!({"pricing_mode":"tiered", "tier_expr":"0:2.5,128000:5"})
            ))
            .is_ok()
        );
        assert!(
            model_price_selection(&request(
                json!({"pricing_mode":"tiered", "tier_expr":"100:2.5"})
            ))
            .is_err()
        );
    }

    #[test]
    fn malformed_metadata_and_prices_are_rejected_before_writing() {
        for invalid in [
            json!({"metadata":{"kind":"unknown"}}),
            json!({"metadata":{"capabilities":{"vision":"yes"}}}),
            json!({"metadata":{"max_output":0}}),
            json!({"tier_ratios":{"":"1"}}),
            json!({"modality_ratios":{"cache_write_1h":2}}),
            json!({"model_ratio":"1000000"}),
        ] {
            assert!(validate_model_draft(&request(invalid)).is_err());
        }
        assert!(validate_model_draft(&request(json!({"modality_ratios":{"cache_write_5m":"0","cache_write_1h":"2"}, "tier_ratios":{}}))).is_ok());
    }
}
