//! Independent retained native counters and frozen fee components.
mod query;
pub(super) mod schema;

use super::ChClient;
use crate::StoreError;
use serde::Deserialize;
use serde_json::{Value, json};

pub const TOOLS: [&str; 3] = ["web_search", "web_fetch", "code_execution"];
pub(super) const DIMENSIONS: &str = "user_id,api_key_id,group_code,model,channel_id,requested_model,upstream_model,endpoint,upstream_endpoint,node,stream,request_type,billing_type";

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    #[default]
    Billed,
    Requested,
    Upstream,
}

impl ModelSource {
    fn column(self) -> &'static str {
        match self {
            Self::Billed => "model",
            Self::Requested => "requested_model",
            Self::Upstream => "upstream_model",
        }
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    #[default]
    Day,
    Hour,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Breakdown {
    #[default]
    Model,
    Channel,
    User,
    ApiKey,
    Group,
    Node,
    Endpoint,
}

impl Breakdown {
    fn column(self, model: ModelSource) -> &'static str {
        match self {
            Self::Model => model.column(),
            Self::Channel => "channel_id",
            Self::User => "user_id",
            Self::ApiKey => "api_key_id",
            Self::Group => "group_code",
            Self::Node => "node",
            Self::Endpoint => "endpoint",
        }
    }
}

#[derive(Default, Deserialize)]
pub struct Filters {
    pub user_id: Option<i64>,
    pub api_key_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub model: Option<String>,
    pub group: Option<String>,
    pub endpoint: Option<String>,
    pub upstream_endpoint: Option<String>,
    pub node: Option<String>,
    pub stream: Option<bool>,
    pub request_type: Option<String>,
    pub billing_type: Option<String>,
}

pub struct StatisticsQuery {
    pub start: chrono::NaiveDate,
    pub end: chrono::NaiveDate,
    pub filters: Filters,
    pub model_source: ModelSource,
    pub granularity: Granularity,
    pub by: Breakdown,
    pub limit: u32,
    pub offset: u32,
}

fn invalid() -> StoreError {
    StoreError::InvalidData("statistics_tool_data_invalid")
}

fn integer(row: &Value, name: &str) -> Result<i64, StoreError> {
    let value = row.get(name).ok_or_else(invalid)?;
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse().ok())
        .ok_or_else(invalid)
}

fn coverage(observed: i64, total: i64) -> Result<Value, StoreError> {
    if observed < 0 || total < observed {
        return Err(invalid());
    }
    Ok(if total == 0 {
        Value::Null
    } else {
        json!(
            i64::try_from(i128::from(observed) * 10_000 / i128::from(total))
                .map_err(|_| invalid())?
        )
    })
}

fn metrics(row: &Value) -> Result<Value, StoreError> {
    let calls = integer(row, "calls")?;
    let records = integer(row, "records")?;
    let covered = integer(row, "covered_records")?;
    let raw = integer(row, "raw_records")?;
    let mut axes = json!({});
    for tool in TOOLS {
        let observed = integer(row, &format!("{tool}_observed"))?;
        let quantity = integer(row, &format!("{tool}_quantity"))?;
        let fee_n = integer(row, &format!("{tool}_fee_observed"))?;
        let amount = integer(row, &format!("{tool}_amount"))?;
        let original = integer(row, &format!("{tool}_original"))?;
        let discount = integer(row, &format!("{tool}_discount"))?;
        if quantity < 0 || original.checked_sub(amount) != Some(discount) {
            return Err(invalid());
        }
        let complete = calls > 0 && observed == calls;
        let fee_complete = records > 0 && fee_n == records;
        let fee = json!({"amount_micro":amount,"original_amount_micro":original,"discount_micro":discount});
        axes[tool] = json!({
            "provider":"anthropic", "usage_contract":"anthropic_server_tool_use_v1",
            "quantity_unit":"request", "quantity":if complete {json!(quantity)} else {Value::Null},
            "observed_quantity":if observed>0 {json!(quantity)} else {Value::Null},
            "observed_calls":observed, "coverage_bp":coverage(observed,calls)?, "complete":complete,
            "billing_unit":if tool=="code_execution" {"container_duration"} else {"request"},
            "fee":if fee_complete {fee.clone()} else {Value::Null},
            "observed_fee":if fee_n>0 {fee} else {Value::Null},
            "fee_observed_records":fee_n, "fee_coverage_bp":coverage(fee_n,records)?,
            "fee_complete":fee_complete, "fee_source":"frozen_pricing_snapshot"
        });
    }
    coverage(calls, records)?;
    coverage(raw, covered)?;
    Ok(
        json!({"calls":calls,"financial_records":records,"tools":axes,"history":{
            "covered_financial_records":covered,"coverage_bp":coverage(covered,records)?,
            "complete":records>0 && covered==records,"retained_records":covered.checked_sub(raw).ok_or_else(invalid)?,
            "raw_recovered_records":raw,"missing_records":records.checked_sub(covered).ok_or_else(invalid)?
        }}),
    )
}

/// Read only the requested scope; every numerical aggregate is range checked.
pub async fn read(ch: &ChClient, q: &StatisticsQuery) -> Result<Value, StoreError> {
    let (sql, bindings) = query::sql(q)?;
    let params = bindings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect::<Vec<_>>();
    let rows = ch.query_with_params(&sql, &params).await?;
    let mut total = None;
    let mut data = Vec::new();
    let mut total_rows = 0;
    for row in rows {
        if integer(&row, "calendar_missing")? != 0 {
            return Err(StoreError::InvalidData(
                "statistics_calendar_history_incomplete",
            ));
        }
        if integer(&row, "source_invalid")? != 0 {
            return Err(StoreError::InvalidData(
                "statistics_request_history_incomplete",
            ));
        }
        let packed = metrics(&row)?;
        total_rows = integer(&row, "total_rows")?;
        if integer(&row, "is_total")? == 1 {
            total = Some(packed);
        } else {
            let bucket = row["bucket"].as_str().ok_or_else(invalid)?;
            let key = row["key"].as_str().ok_or_else(invalid)?;
            let mut packed = packed;
            packed["bucket"] = json!(bucket);
            packed["key"] = json!(key);
            data.push(packed);
        }
    }
    Ok(
        json!({"total":total.ok_or_else(invalid)?,"data":data,"total_rows":total_rows,
        "limit":q.limit.clamp(1,100),"offset":q.offset,"quantity_basis":"physical_calls",
        "fee_basis":"financial_records","tokens_included":false}),
    )
}
