//! Independent input units; unknown history never becomes a measured character zero.
use super::measurement_coverage::Mode;
use super::stats::{ch_i64, scaled_ratio};
use crate::gateway::error::AppError;
use serde_json::{Map, Value, json};

const CORE_FIELDS: [&str; 4] = [
    "unit_observed",
    "unit_characters",
    "unit_character_n",
    "unit_token_n",
];
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

pub(super) fn pg_sql() -> String {
    let unit = "b.usage_details->>'input_unit'";
    let quantity = "b.usage_details->>'input_characters'";
    // The ledger validates new character metadata before writing JSON. Missing old JSON stays unknown.
    format!(
        "'unit_observed', COUNT(*), 'unit_characters', SUM(({quantity})::bigint) FILTER (WHERE {unit} = 'characters'), 'unit_character_n', COUNT({quantity}) FILTER (WHERE {unit} = 'characters'), 'unit_token_n', COUNT(*) FILTER (WHERE {unit} = 'tokens' AND {quantity} IS NULL)"
    )
}

fn time_sql(table: &str) -> &'static str {
    match table {
        "mv_channel_5min" => "toStartOfHour(ts5) AS hour, toDate(ts5) AS day",
        "mv_key_model_day" | "mv_user_model_day" | "mv_user_day" | "mv_apikey_day" => {
            "toStartOfDay(day) AS hour"
        }
        _ => "toDate(hour) AS day",
    }
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

pub(super) fn prepared_with_calibration(
    keys: &str,
    table: &str,
    predicate: &str,
    mode: Mode,
    historical: bool,
) -> String {
    let base = uncalibrated(keys, table, predicate, mode);
    if !historical {
        return format!(
            "(SELECT u.*,toInt64(0) AS legacy_characters,toInt64(0) AS legacy_character_n FROM {base} u)"
        );
    }
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let legacy = correction_source(keys, predicate);
    let time = time_sql(table);
    let expected = format!(
        "(WITH {time},toUInt8(1) AS source_scope SELECT {keys},countMerge(requests) AS expected FROM {table} WHERE {predicate} GROUP BY {keys})"
    );
    let selected = keys
        .split(", ")
        .map(|key| format!("u.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    // Evidence can cover an old row whose original unit aggregate was never
    // installed. Known units are disjoint; observations must never exceed bills.
    let count = "ifNull(c.legacy_character_n,0)";
    let quantity = "ifNull(c.legacy_characters,0)";
    let guard = format!("toInt64(throwIf({count}+u.unit_character_n+u.unit_token_n>e.expected))");
    format!(
        "(SELECT {selected},greatest(u.unit_observed,u.unit_character_n+u.unit_token_n+{count})+{guard} AS unit_observed,u.unit_characters+{quantity} AS unit_characters,u.unit_character_n+{count} AS unit_character_n,u.unit_token_n AS unit_token_n,{quantity} AS legacy_characters,{count} AS legacy_character_n FROM {base} u LEFT JOIN {legacy} c USING ({keys}) LEFT JOIN {expected} e USING ({keys}))"
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

fn uncalibrated(keys: &str, table: &str, predicate: &str, mode: Mode) -> String {
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let time = time_sql(table);
    let merged = "countMerge(requests) AS unit_observed, sumIfMerge(unit_characters) AS unit_characters, countIfMerge(unit_character_n) AS unit_character_n, countIfMerge(unit_token_n) AS unit_token_n";
    let raw = raw_sql();
    if mode != Mode::Recover {
        let expected = format!(
            "WITH {time}, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS n FROM {table} WHERE {predicate} GROUP BY {keys}"
        );
        let counts = format!(
            "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, countMerge(requests) AS n FROM mv_input_units_5min WHERE {predicate} GROUP BY {keys}"
        );
        let aggregate = format!(
            "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, {merged} FROM mv_input_units_5min WHERE {predicate}"
        );
        let raw = format!(
            "WITH toStartOfFiveMinutes(ts) AS ts5, toStartOfHour(ts) AS hour, toDate(ts) AS day, toUInt8(1) AS source_scope SELECT {keys}, {raw} FROM request_log_raw WHERE {predicate}"
        );
        return super::measurement_coverage::fast_source(
            mode, keys, &expected, &counts, &aggregate, &raw,
        );
    }
    let selected_keys = keys
        .split(", ")
        .map(|key| format!("e.{key} AS {key}"))
        .collect::<Vec<_>>()
        .join(", ");
    let selected=CORE_FIELDS.map(|field|format!("toInt64(if(use_aggregate, ifNull(a.{field}, 0), if(ifNull(r.unit_observed, 0) <= e.expected, ifNull(r.{field}, 0), 0))) AS {field}")).join(", ");
    format!(
        "(WITH \
        e AS (SELECT {keys}, countMerge(requests) AS expected FROM (SELECT *, {time}, toUInt8(1) AS source_scope FROM {table}) WHERE {predicate} GROUP BY {keys}), \
        a AS (SELECT {keys}, {merged} FROM (SELECT *, toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope FROM mv_input_units_5min) WHERE {predicate} GROUP BY {keys}), \
        missing AS (SELECT {keys} FROM e LEFT JOIN a USING ({keys}) WHERE e.expected != ifNull(a.unit_observed, 0)), \
        r AS (SELECT {keys}, {raw} FROM (SELECT *, toStartOfFiveMinutes(ts) AS ts5, toStartOfHour(ts) AS hour, toDate(ts) AS day, toUInt8(1) AS source_scope FROM request_log_raw) raw_rows INNER JOIN missing USING ({keys}) WHERE {predicate} GROUP BY {keys}) \
        SELECT {selected_keys}, {selected}, \
        (ifNull(a.unit_observed, 0) <= e.expected AND ifNull(a.unit_observed, 0) >= if(ifNull(r.unit_observed, 0) <= e.expected, ifNull(r.unit_observed, 0), 0)) AS use_aggregate \
        FROM e LEFT JOIN a USING ({keys}) LEFT JOIN r USING ({keys}))"
    )
}

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
