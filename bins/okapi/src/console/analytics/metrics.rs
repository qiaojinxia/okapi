//! 立方体度量的累加与打包：核心度量、原价、缓存度量（趋势 / 拆分 / 流向共用）。
use super::super::stats::{ch_i64, rate_bp};
use super::AGG;
use serde_json::{Value, json};

pub(super) fn aggregate_metrics() -> String {
    // This is a hint, not an authoritative test flag. Keep all matching records
    // in every total and require the fixture's exact random-suffix format.
    const FIXTURE_MODEL: &str = "^(cube-[ab]-[0-9a-f]{10}|log-[0-9a-f]{12}|modal-[0-9a-f]{32})$";
    format!(
        "{AGG}, sumIf(requests, match(model, '{FIXTURE_MODEL}')) AS fixture_reqs, \
        sumIf(prompt_tokens + completion_tokens, match(model, '{FIXTURE_MODEL}')) AS fixture_tokens, {}, {}, {}",
        super::super::usage_sources::sum_sql(),
        super::super::token_details::sum_sql(),
        super::super::output_rate::sum_sql()
    )
}

/// 一行聚合 → 展示字段（比率全部基点/整数，避免前端拿浮点二次换算）。
/// Bucket counters are additive; ratios and averages are packed only after summing.
/// This avoids querying the entire current source again just for the trend total.
pub(super) const CORE_ADDITIVE_FIELDS: &[&str] = &[
    "reqs",
    "financial_records",
    "prompt",
    "cached",
    "completion",
    "reasoning",
    "spend",
    "saved",
    "cost",
    "errs",
    "lat_sum",
    "lat_n",
    "lat_observed",
    "lat_output",
    "ttft_s",
    "ttft_n",
    "ttft_observed",
    "write_sum",
    "write_n",
    "read_n",
    "cost_n",
    "covered_spend",
    "covered_cost_sum",
    "fixture_reqs",
    "fixture_tokens",
];
pub(super) fn accumulate_core_metrics(target: &mut Value, row: &Value) {
    for &field in CORE_ADDITIVE_FIELDS {
        target[field] = json!(ch_i64(target, field).saturating_add(ch_i64(row, field)));
    }
}

pub(super) fn sum_metric_rows(rows: &[Value]) -> Value {
    let mut total = json!({});
    for row in rows {
        accumulate_original(&mut total, row);
        accumulate_core_metrics(&mut total, row);
        super::super::usage_sources::accumulate(&mut total, row);
        super::super::token_details::accumulate(&mut total, row);
        super::super::output_rate::accumulate(&mut total, row);
    }
    total
}

pub(super) fn pack_metrics(r: &Value) -> serde_json::Map<String, Value> {
    let reqs = ch_i64(r, "reqs");
    let prompt = ch_i64(r, "prompt");
    let cached = ch_i64(r, "cached");
    let completion = ch_i64(r, "completion");
    let errs = ch_i64(r, "errs");
    let ttft_n = ch_i64(r, "ttft_n");
    let mut m = serde_json::Map::new();
    let known = ch_i64(r, "cost_n");
    let known_amount = ch_i64(r, "covered_spend");
    let known_cost = ch_i64(r, "covered_cost_sum");
    let records = ch_i64(r, "financial_records");
    m.insert("financial_records".into(), json!(records));
    m.insert("cost_known_records".into(), json!(known));
    m.insert("cost_known_requests".into(), json!(known));
    m.insert(
        "cost_coverage_bp".into(),
        if records > 0 {
            json!(rate_bp(known, records))
        } else {
            Value::Null
        },
    );
    m.insert("known_amount_micro".into(), json!(known_amount));
    m.insert("known_cost_micro".into(), json!(known_cost));
    m.insert(
        "known_margin_micro".into(),
        if known > 0 {
            json!(known_amount - known_cost)
        } else {
            Value::Null
        },
    );
    m.insert(
        "margin_micro".into(),
        if known == records && records > 0 {
            json!(known_amount - known_cost)
        } else {
            Value::Null
        },
    );
    m.extend(cache_metrics(r, reqs));
    m.insert(
        "suspected_test_requests".into(),
        json!(ch_i64(r, "fixture_reqs")),
    );
    m.insert(
        "suspected_test_tokens".into(),
        json!(ch_i64(r, "fixture_tokens")),
    );
    m.insert("test_detection_basis".into(), json!("fixture_model_name"));

    m.insert("requests".into(), json!(reqs));
    m.insert("errors".into(), json!(errs));
    m.insert("error_rate_bp".into(), json!(rate_bp(errs, reqs)));
    m.insert("prompt_tokens".into(), json!(prompt));
    m.insert("cached_tokens".into(), json!(cached));
    m.insert("completion_tokens".into(), json!(completion));
    m.insert("reasoning_tokens".into(), json!(ch_i64(r, "reasoning")));
    m.insert("tokens".into(), json!(prompt.saturating_add(completion)));
    // 口径与门户 breakdown 一致：命中 token / 输入 token
    m.insert("amount_micro".into(), json!(ch_i64(r, "spend")));
    m.insert("original_amount_micro".into(), original_amount(r));
    m.insert("discount_micro".into(), json!(ch_i64(r, "saved")));
    m.insert("upstream_cost_micro".into(), json!(ch_i64(r, "cost")));
    m.extend(super::super::latency::metrics(
        ch_i64(r, "lat_sum"),
        ch_i64(r, "lat_n"),
        ch_i64(r, "lat_output"),
        reqs,
        ch_i64(r, "lat_observed"),
    ));
    m.extend(super::super::ttft_average::metrics(
        ch_i64(r, "ttft_s"),
        ttft_n,
        reqs,
        ch_i64(r, "ttft_observed"),
    ));
    m.extend(super::super::usage_sources::metrics(
        r,
        reqs,
        [Some(prompt), Some(completion)],
        Some(cached),
        ch_i64(r, "read_n"),
    ));
    m.extend(super::super::token_details::metrics(r, reqs));
    m.extend(super::super::output_rate::metrics(r, reqs));
    m
}

/// 精简源的行 → 展示字段：与 `pack_metrics` 同名同口径的核心部分，不含任何测量口径
/// （成本覆盖、缓存、延迟、TTFT、用量来源……），避免把"没查"伪装成 0。
pub(super) fn pack_core_metrics(r: &Value) -> serde_json::Map<String, Value> {
    let reqs = ch_i64(r, "reqs");
    let prompt = ch_i64(r, "prompt");
    let completion = ch_i64(r, "completion");
    let errs = ch_i64(r, "errs");
    let mut m = serde_json::Map::new();
    m.insert("requests".into(), json!(reqs));
    m.insert("errors".into(), json!(errs));
    m.insert("error_rate_bp".into(), json!(rate_bp(errs, reqs)));
    m.insert("prompt_tokens".into(), json!(prompt));
    m.insert("cached_tokens".into(), json!(ch_i64(r, "cached")));
    m.insert("completion_tokens".into(), json!(completion));
    m.insert("reasoning_tokens".into(), json!(ch_i64(r, "reasoning")));
    m.insert("tokens".into(), json!(prompt.saturating_add(completion)));
    m.insert("amount_micro".into(), json!(ch_i64(r, "spend")));
    m.insert("original_amount_micro".into(), original_amount(r));
    m.insert("discount_micro".into(), json!(ch_i64(r, "saved")));
    m.insert("upstream_cost_micro".into(), json!(ch_i64(r, "cost")));
    m
}

pub(super) fn original_amount(row: &Value) -> Value {
    use okapi_domain::Money;
    if let Some(checked) = row.get("checked_original_micro") {
        return money_value(checked).map_or(Value::Null, |amount| json!(amount.as_micros()));
    }
    let component = |field| row.get(field).map_or(Some(Money::ZERO), money_value);
    component("spend")
        .zip(component("saved"))
        .and_then(|(amount, discount)| amount.checked_add(discount))
        .map_or(Value::Null, |amount| json!(amount.as_micros()))
}

pub(super) fn money_value(value: &Value) -> Option<okapi_domain::Money> {
    value
        .as_str()
        .map_or_else(|| value.as_i64(), |text| text.parse::<i64>().ok())
        .map(okapi_domain::Money::from_micros)
}

pub(super) fn accumulate_original(total: &mut Value, row: &Value) {
    let previous = total
        .get("checked_original_micro")
        .map_or(Some(okapi_domain::Money::ZERO), money_value);
    total["checked_original_micro"] = previous
        .zip(money_value(&original_amount(row)))
        .and_then(|(sum, amount)| sum.checked_add(amount))
        .map_or(Value::Null, |amount| json!(amount.as_micros()));
}

pub(super) fn sum_core_rows(rows: &[Value]) -> Value {
    let mut total = json!({});
    for row in rows {
        accumulate_original(&mut total, row);
        for key in [
            "reqs",
            "financial_records",
            "prompt",
            "cached",
            "completion",
            "reasoning",
            "spend",
            "saved",
            "cost",
            "errs",
        ] {
            total[key] = json!(ch_i64(&total, key).saturating_add(ch_i64(row, key)));
        }
    }
    total
}

pub(super) fn cache_metrics(row: &Value, requests: i64) -> serde_json::Map<String, Value> {
    let known = ch_i64(row, "write_n");
    json!({
        "cache_write_known_requests": known,
        "cache_write_tokens": if known == requests { json!(ch_i64(row, "write_sum")) } else { Value::Null },
        // A subtotal, never a claim that unreported requests wrote zero tokens.
        "recorded_cache_write_tokens": if known > 0 || ch_i64(row, "write_sum") > 0 { json!(ch_i64(row, "write_sum")) } else { Value::Null },
        "cache_read_known_requests": ch_i64(row, "read_n"),
    }).as_object().cloned().unwrap_or_default()
}
