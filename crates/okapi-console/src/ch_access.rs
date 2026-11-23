//! 读 ClickHouse 的公共小件：统计 / 分析 / 日志 / 门户各页共用，别再各抄一份。

use axum::http::StatusCode;
use okapi_api::codes;
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_store::ChClient;

pub(crate) use okapi_store::ch::row::{int as ch_i64, text as ch_str};

/// 没配 ClickHouse 的部署读不了统计类接口：统一回 501 `stats_disabled`。
pub(crate) fn ch_or_disabled(state: &AppState) -> Result<&ChClient, AppError> {
    state
        .ch
        .as_ref()
        .ok_or_else(|| AppError::new(StatusCode::NOT_IMPLEMENTED, codes::STATS_DISABLED))
}
