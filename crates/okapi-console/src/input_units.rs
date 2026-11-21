//! Independent input units; unknown history never becomes a measured character zero.
use super::stats::{ch_i64, scaled_ratio};
use okapi_kernel::error::AppError;
use serde_json::{Map, Value, json};

pub(super) const FIELDS: [&str; 6] = [
    "unit_observed",
    "unit_characters",
    "unit_character_n",
    "unit_token_n",
    "legacy_characters",
    "legacy_character_n",
];
pub(super) const VALID_CHAR: &str = "input_unit = 'characters' AND isNotNull(input_characters) AND prompt_tokens = 0 AND completion_tokens = 0 AND cached_tokens = 0 AND ifNull(cache_write_tokens, 0) = 0 AND reasoning_tokens = 0 AND ifNull(audio_prompt_tokens, 0) = 0 AND ifNull(audio_completion_tokens, 0) = 0 AND ifNull(image_prompt_tokens, 0) = 0 AND ifNull(image_completion_tokens, 0) = 0";

pub(super) fn raw_sql() -> String {
    format!(
        "count() AS unit_observed, sumIf(toUInt64(ifNull(input_characters, 0)), {VALID_CHAR}) AS unit_characters, countIf({VALID_CHAR}) AS unit_character_n, countIf(input_unit = 'tokens' AND isNull(input_characters)) AS unit_token_n"
    )
}

/// PG 账本上的 [`VALID_CHAR`]：同样只认 token 全为 0 的字符计量行，带 token 的算口径冲突、留在未知里。
pub(super) const PG_VALID_CHAR: &str = "b.input_unit = 'characters' AND b.input_characters IS NOT NULL AND b.prompt_tokens = 0 AND b.completion_tokens = 0 AND b.cached_tokens = 0 AND COALESCE(b.cache_write_tokens, 0) = 0 AND b.reasoning_tokens = 0 AND COALESCE(b.audio_prompt_tokens, 0) = 0 AND COALESCE(b.audio_completion_tokens, 0) = 0 AND COALESCE(b.image_prompt_tokens, 0) = 0 AND COALESCE(b.image_completion_tokens, 0) = 0";

pub(super) fn pg_sql() -> String {
    // billing_records 的生成列：input_characters 只认 JSON 整数，缺失、JSON null（按 token 计的请求就这样写）
    // 与脏值都是 NULL，与 CH 的 isNull 同义；没有 input_unit 的旧记录两边都不算，留在未知里。
    format!(
        "'unit_observed', COUNT(*), 'unit_characters', SUM(b.input_characters) FILTER (WHERE {PG_VALID_CHAR}), 'unit_character_n', COUNT(*) FILTER (WHERE {PG_VALID_CHAR}), 'unit_token_n', COUNT(*) FILTER (WHERE b.input_unit = 'tokens' AND b.input_characters IS NULL)"
    )
}

/// Internal SQL only; callers retain parameter binding for all string filters.
pub(super) fn correction_source(keys: &str, predicate: &str) -> String {
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    format!(
        "(WITH toStartOfFiveMinutes(ts) AS ts5,toStartOfHour(ts) AS hour,toDate(ts) AS day,toUInt8(1) AS source_scope SELECT {keys},accurateCast(sum(toInt128(characters)*copies),'Int64') AS legacy_characters,accurateCast(sum(toInt128(copies)),'Int64') AS legacy_character_n FROM legacy_speech_units_v1 FINAL WHERE {predicate} AND basis='legacy_speech_contract_v1' GROUP BY {keys})"
    )
}

/// Internal SQL expressions only; exclude proven characters before sorting/folding.
pub(super) fn corrected_sql(total: &str, quantity: &str) -> String {
    format!(
        "accurateCast({total},'Int64')-toInt64({quantity})+toInt64(throwIf({quantity}<0 OR {quantity}>accurateCast({total},'Int64')))"
    )
}

/// Apply once to uncalibrated primary totals. Financial values remain unchanged.
pub(super) fn correct_totals(row: &mut Value, evidence: &Value) -> Result<(), AppError> {
    let quantity = ch_i64(evidence, "legacy_characters");
    let count = ch_i64(evidence, "legacy_character_n");
    if quantity < 0 || count < 0 || count > ch_i64(row, "requests") || (count == 0 && quantity != 0)
    {
        return Err(AppError::internal());
    }
    for field in ["prompt_tokens", "tokens"] {
        if row.get(field).is_some_and(|v| !v.is_null()) {
            let total = ch_i64(row, field)
                .checked_sub(quantity)
                .filter(|n| *n >= 0)
                .ok_or_else(AppError::internal)?;
            row[field] = json!(total);
        }
    }
    Ok(())
}

pub(super) const MERGED: &str = "countMerge(requests) AS unit_observed, sumIfMerge(unit_characters) AS unit_characters, countIfMerge(unit_character_n) AS unit_character_n, countIfMerge(unit_token_n) AS unit_token_n";

pub(super) fn metrics(row: &Value, records: i64) -> Map<String, Value> {
    let characters = ch_i64(row, "unit_character_n");
    let tokens = ch_i64(row, "unit_token_n");
    let observed = ch_i64(row, "unit_observed");
    let quantity = ch_i64(row, "unit_characters");
    let legacy_n = ch_i64(row, "legacy_character_n");
    let legacy_qty = ch_i64(row, "legacy_characters");
    let valid = characters >= 0
        && tokens >= 0
        && observed >= 0
        && observed <= records
        && characters.saturating_add(tokens) <= observed
        && quantity >= 0
        && legacy_n >= 0
        && legacy_n <= characters
        && legacy_qty >= 0
        && legacy_qty <= quantity
        && (legacy_n > 0 || legacy_qty == 0)
        && (characters > 0 || quantity == 0);
    let (characters, tokens) = if valid { (characters, tokens) } else { (0, 0) };
    let known = characters.saturating_add(tokens);
    let complete = records > 0 && known == records;
    let subtotal = if characters > 0 || complete {
        json!(quantity)
    } else {
        Value::Null
    };
    json!({"input_units":{
        "characters":if complete {subtotal.clone()} else {Value::Null},
        "observed_characters":subtotal,
        "character_requests":characters,"token_requests":tokens,
        "known_requests":known,"unknown_requests":records.saturating_sub(known),
        "coverage_bp":if records>0 {json!(scaled_ratio(known,records,10000))} else {Value::Null},
        "complete":complete,
        "historical_character_requests":if valid {legacy_n}else{0},
        "historical_characters_excluded_from_tokens":if valid {legacy_qty}else{0},
        "historical_basis":if valid && legacy_n>0 {json!(okapi_store::legacy_speech::BASIS)}else{Value::Null},
    }})
    .as_object()
    .cloned()
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_characters_zero_partial_unknown_and_conflicting_populations_differ() {
        for (records, row, quantity, observed, unknown) in [
            (
                2,
                json!({"unit_observed":2,"unit_character_n":1,"unit_token_n":1,"unit_characters":11}),
                json!(11),
                json!(11),
                0,
            ),
            (
                1,
                json!({"unit_observed":1,"unit_character_n":1,"unit_characters":0}),
                json!(0),
                json!(0),
                0,
            ),
            (
                1,
                json!({"unit_observed":1,"unit_token_n":1}),
                json!(0),
                json!(0),
                0,
            ),
            (
                2,
                json!({"unit_observed":2,"unit_character_n":1,"unit_characters":11}),
                Value::Null,
                json!(11),
                1,
            ),
            (1, json!({"unit_observed":1}), Value::Null, Value::Null, 1),
            (
                1,
                json!({"unit_observed":1,"unit_token_n":1,"unit_characters":11}),
                Value::Null,
                Value::Null,
                1,
            ),
            (
                1,
                json!({"unit_observed":2,"unit_character_n":2,"unit_characters":22}),
                Value::Null,
                Value::Null,
                1,
            ),
            (0, json!({}), Value::Null, Value::Null, 0),
        ] {
            let result = metrics(&row, records);
            assert_eq!(result["input_units"]["characters"], quantity);
            assert_eq!(result["input_units"]["observed_characters"], observed);
            assert_eq!(result["input_units"]["unknown_requests"], unknown);
        }
    }
}
