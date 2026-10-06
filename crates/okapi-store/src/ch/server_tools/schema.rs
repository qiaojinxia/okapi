//! Own schema family: generic legacy population parsing must not rewrite these states.
use super::{DIMENSIONS, TOOLS};
use crate::{ChClient, StoreError};
use std::fmt::Write as _;

pub(super) fn measure_names() -> Vec<String> {
    let mut names = ["calls", "records", "covered_records", "raw_records"]
        .map(str::to_owned)
        .to_vec();
    for tool in TOOLS {
        for field in [
            "quantity",
            "observed",
            "fee_observed",
            "amount",
            "original",
            "discount",
        ] {
            names.push(format!("{tool}_{field}"));
        }
    }
    names
}

pub(super) fn sums() -> String {
    measure_names()
        .iter()
        .map(|name| format!("sum({name}) AS {name}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn valid_integer(expression: &str, max: &str) -> String {
    format!(
        "(match({expression},'^(0|[1-9][0-9]*)$') AND isNotNull(toInt128OrNull({expression})) AND toInt128OrZero({expression})<={max})"
    )
}

fn valid_discount(expression: &str) -> String {
    format!(
        "(match({expression},'^(0|-?[1-9][0-9]*)$') AND isNotNull(toInt128OrNull({expression})) AND toInt128OrZero({expression})>=-9223372036854775808 AND toInt128OrZero({expression})<=9223372036854775807)"
    )
}

/// One row produces independent observations. Unknown counters and prices never
/// become observed zero; refund snapshots produce only signed financial fees.
pub(super) fn observation_select() -> String {
    let mut aliases =
        vec!["JSONExtractArrayRaw(ratio_snapshot,'server_tool_fees') AS fees".to_owned()];
    let mut fields = vec![
        "toUInt64(log_type IN (2,5)) AS calls".to_owned(),
        "toUInt64(1) AS financial_records".to_owned(),
    ];
    for tool in TOOLS {
        let native = format!("JSONExtractRaw(server_tool_usage,'{tool}_requests')");
        let quantity_valid = format!(
            "(log_type IN (2,5) AND isValidJSON(server_tool_usage) AND JSONType(server_tool_usage)='Object' AND JSONExtractString(server_tool_usage,'provider')='anthropic' AND {})",
            valid_integer(&native, "2147483647")
        );
        aliases.push(format!("{quantity_valid} AS {tool}_quantity_valid"));
        fields.push(format!(
            "if({tool}_quantity_valid,toInt128OrZero({native}),toInt128(0)) AS {tool}_quantity"
        ));
        fields.push(format!(
            "toUInt64({tool}_quantity_valid) AS {tool}_observed"
        ));
        if tool == "code_execution" {
            fields.push(format!("toUInt64(0) AS {tool}_fee_observed"));
            for money in ["amount", "original", "discount"] {
                fields.push(format!("toInt128(0) AS {tool}_{money}"));
            }
            continue;
        }
        let selected = format!("{tool}_fees");
        let row = format!("{tool}_fee");
        aliases.push(format!(
            "arrayFilter(f -> JSONExtractString(f,'tool')='{tool}',fees) AS {selected}"
        ));
        aliases.push(format!("arrayElement({selected},1) AS {row}"));
        let mut valid = format!(
            "(log_type IN (2,5,6) AND isValidJSON(ratio_snapshot) AND JSONType(ratio_snapshot,'server_tool_fees')='Array' AND length({selected})=1 AND JSONExtractString({row},'usage_contract')='anthropic_server_tool_use_v1' AND JSONExtractString({row},'unit')='request'"
        );
        for money in ["amount", "original", "discount"] {
            let name = format!("{tool}_{money}_raw");
            aliases.push(
                format!("JSONExtractRaw({row},'{money}_amount_micro') AS {name}")
                    .replace("'amount_amount_micro'", "'amount_micro'")
                    .replace("'discount_amount_micro'", "'discount_micro'"),
            );
            let predicate = if money == "discount" {
                let marker = format!("{tool}_discount_valid_v2");
                aliases.push(format!("{} AS {marker}", valid_discount(&name)));
                marker
            } else {
                valid_integer(&name, "9223372036854775807")
            };
            let _ = write!(valid, " AND {predicate}");
        }
        let price = format!("JSONExtractRaw({row},'pricing','price_per_request_micro')");
        let quantity = format!("JSONExtractRaw({row},'quantity')");
        let list = format!("JSONExtractRaw({row},'list_price_micro')");
        let zero = format!(
            "(toInt128OrZero({tool}_original_raw)=0 AND toInt128OrZero({tool}_amount_raw)=0 AND toInt128OrZero({tool}_discount_raw)=0 AND {list}='0')"
        );
        let requested = format!("JSONExtractRaw({row},'requested')");
        let additional = format!(
            "(JSONExtractString({row},'pricing','billing')='additional' AND {} AND {} AND ({} OR (toInt128OrZero({price})=0 AND {quantity}='null')) AND toInt128OrZero({list})=toInt128OrZero({price})*toInt128OrZero({quantity}))",
            valid_integer(&price, "9223372036854775807"),
            valid_integer(&list, "9223372036854775807"),
            valid_integer(&quantity, "2147483647")
        );
        let _ = write!(
            valid,
            " AND toInt128OrZero({tool}_original_raw)-toInt128OrZero({tool}_amount_raw)=toInt128OrZero({tool}_discount_raw) AND (({requested}='false' AND {zero} AND {quantity} IN ('null','0')) OR ({requested} IN ('true','') AND ((JSONExtractString({row},'pricing','billing')='included' AND {zero}) OR {additional}))))"
        );
        aliases.push(format!("{valid} AS {tool}_fee_valid"));
        fields.push(format!("toUInt64({tool}_fee_valid) AS {tool}_fee_observed"));
        for money in ["amount", "original", "discount"] {
            fields.push(format!("if({tool}_fee_valid,if(log_type=6,-toInt128OrZero({tool}_{money}_raw),toInt128OrZero({tool}_{money}_raw)),toInt128(0)) AS {tool}_{money}"));
        }
    }
    format!(
        "WITH {} SELECT toStartOfMinute(ts) AS minute,{DIMENSIONS},{} FROM request_log_raw",
        aliases.join(","),
        fields.join(",")
    )
}

fn projection() -> String {
    let mut fields = vec![
        "countStateIf(calls=1) AS calls".to_owned(),
        "countState() AS financial_records".to_owned(),
    ];
    for tool in TOOLS {
        fields.push(format!("sumState({tool}_quantity) AS {tool}_quantity"));
        for field in ["observed", "fee_observed"] {
            fields.push(format!("countIfState({tool}_{field}=1) AS {tool}_{field}"));
        }
        for field in ["amount", "original", "discount"] {
            fields.push(format!("sumState({tool}_{field}) AS {tool}_{field}"));
        }
    }
    format!(
        "SELECT minute,{DIMENSIONS},{} FROM ({}) GROUP BY minute,{DIMENSIONS}",
        fields.join(","),
        observation_select()
    )
}

pub(in crate::ch) async fn ensure(ch: &ChClient) -> Result<(), StoreError> {
    let projection = projection();
    ch.execute(&format!(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS server_tool_minute_v1 ENGINE=AggregatingMergeTree() PARTITION BY toYYYYMM(minute) ORDER BY (minute,{DIMENSIONS}) SETTINGS non_replicated_deduplication_window=1000 AS {projection}"
    )).await?;
    let rows = ch.query_json_each_row(
        "SELECT create_table_query FROM system.tables WHERE database=currentDatabase() AND name='server_tool_minute_v1'"
    ).await?;
    let [row] = rows.as_slice() else {
        return Err(StoreError::InvalidData("statistics_tool_data_invalid"));
    };
    let query = row["create_table_query"]
        .as_str()
        .ok_or(StoreError::InvalidData("statistics_tool_data_invalid"))?;
    if !query.contains("web_search_discount_valid_v2")
        || !query.contains("web_fetch_discount_valid_v2")
    {
        // Same columns and aggregate-state types: preserve the implicit storage UUID/history.
        ch.execute(&format!(
            "ALTER TABLE server_tool_minute_v1 MODIFY QUERY {projection}"
        ))
        .await?;
    }
    Ok(())
}

pub(super) fn retained() -> String {
    let mut fields = vec![
        "toInt128(countMerge(calls)) AS calls".to_owned(),
        "toInt128(countMerge(financial_records)) AS records".to_owned(),
    ];
    for tool in TOOLS {
        fields.push(format!("sumMerge({tool}_quantity) AS {tool}_quantity"));
        for field in ["observed", "fee_observed"] {
            fields.push(format!(
                "toInt128(countIfMerge({tool}_{field})) AS {tool}_{field}"
            ));
        }
        for field in ["amount", "original", "discount"] {
            fields.push(format!("sumMerge({tool}_{field}) AS {tool}_{field}"));
        }
    }
    fields.join(",")
}
