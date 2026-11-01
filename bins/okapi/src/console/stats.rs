//! 运营看板查询（IMPLEMENTATION §10 指标字典）。
//!
//! 只读 ClickHouse 物化视图；升级前的 TTFT 在原始明细完整时重算。
//! 不碰热路径、不落 PG 写。CH 不可用时按既有约定 501 `stats_disabled`。
//!
//! SQL 用 format! 拼装但**只插入 clamp 过的整数**（与 leaderboard 同一纪律），
//! 请求里的字符串一律不进 SQL。
//!
//! 本文件放共用的窗口参数、取数小工具与成本覆盖率注解；各看板端点按主题分在子模块
//! （channels / models / overview / health / finance / portal），并从这里原路径再导出。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::http::StatusCode;
use okapi_api::codes;
use okapi_store::ChClient;
use serde::Deserialize;
use serde_json::{Value, json};

mod channels;
mod finance;
mod health;
mod models;
mod overview;
mod portal;

pub use self::channels::{TimelineQuery, channel_timeline, channel_usage, channels};
pub use self::finance::{cashflow, groups, margin};
pub use self::health::{clients, diagnose, errors};
pub use self::models::{model_trend, models};
pub use self::overview::{OverviewQuery, RealtimeQuery, overview, realtime};
pub use self::portal::{BreakdownQuery, my_breakdown, my_daily};

fn ch_or_disabled(state: &AppState) -> Result<&ChClient, AppError> {
    state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))
}

/// CH 的 JSONEachRow 把 64 位整数序列化为字符串，需两种形态都认。
pub(super) fn ch_i64(row: &Value, key: &str) -> i64 {
    row.get(key).map_or(0, |v| {
        v.as_str()
            .map_or_else(|| v.as_i64(), |s| s.parse::<i64>().ok())
            .unwrap_or(0)
    })
}

#[derive(Deserialize)]
pub struct WindowQuery {
    #[serde(default)]
    pub cached: bool,
    /// 回看天数（1–90，缺省 7）。
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: i64,
}

impl WindowQuery {
    fn days(&self) -> u32 {
        self.days.unwrap_or(7).clamp(1, 90)
    }
    fn limit(&self) -> u32 {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
    fn offset(&self) -> i64 {
        self.offset.max(0)
    }
}

// Window totals are computed before LIMIT. An out-of-range page has no row to
// carry them, so recover just the totals instead of reporting an empty dataset.
async fn page_summary(ch: &ChClient, rows: &[Value], sql: &str) -> Result<Value, AppError> {
    if let Some(row) = rows.first() {
        return Ok(row.clone());
    }
    Ok(ch
        .query_json_each_row(sql)
        .await?
        .into_iter()
        .next()
        .unwrap_or_else(|| json!({})))
}

/// 占比 → 基点（万分之一；整数运算，分母 0 返 0）。
pub(super) fn rate_bp(part: i64, total: i64) -> i64 {
    scaled_ratio(part, total, 10_000)
}

/// Scale after widening: saturating the numerator first silently understates large totals.
/// Only the final result saturates, retaining signed changes and margin rates.
pub(super) fn scaled_ratio(part: i64, total: i64, scale: i64) -> i64 {
    if total <= 0 {
        return 0;
    }
    let value = i128::from(part) * i128::from(scale) / i128::from(total);
    i64::try_from(value.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or_default()
}

/// Known zero cost counts as covered; absent historical costs never become fictitious margin.
fn apply_cost_coverage(row: &mut Value, records: i64, values: [i64; 3]) {
    let [known, revenue, cost] = values;
    let margin = revenue.saturating_sub(cost);
    row["financial_records"] = json!(records);
    row["cost_known_records"] = json!(known);
    row["cost_known_requests"] = json!(known);
    row["cost_coverage_bp"] = if records > 0 {
        json!(rate_bp(known, records))
    } else {
        Value::Null
    };
    row["known_cost_micro"] = json!(cost);
    row["known_amount_micro"] = json!(revenue);
    row["known_margin_micro"] = if known > 0 {
        json!(margin)
    } else {
        Value::Null
    };
    row["margin_micro"] = if known == records && records > 0 {
        json!(margin)
    } else {
        Value::Null
    };
    row["margin_rate_bp"] = if known == records && revenue > 0 {
        json!(rate_bp(margin, revenue))
    } else {
        Value::Null
    };
}
