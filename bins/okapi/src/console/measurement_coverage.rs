//! 测量来源一律直接读各自的预聚合物化视图。
//!
//! 每个来源 MV 都由 `request_log_raw` 的每一次写入无条件喂入，新数据按构造就是完整的。
//! 开发阶段不保留旧历史的兼容（库可以重置），所以不再有「逐粒度对账 → 回原始表补齐」
//! 的恢复路径：那条路径把一次查询膨胀到几十 KB，ClickHouse 光规划就要秒级。
use super::stats::ch_i64;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;

/// 预聚合来源：`aggregate` 是不带 GROUP BY 的 `SELECT {keys}, … FROM mv_… WHERE …`。
pub(super) fn aggregate_source(keys: &str, aggregate: &str) -> String {
    format!("({aggregate} GROUP BY {keys})")
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Coverage {
    pub historical_units: bool,
}

impl Coverage {
    async fn has_historical_units(
        state: &AppState,
        predicate: &str,
        params: &[(&str, &str)],
    ) -> Result<bool, AppError> {
        let history_sql = format!(
            "WITH toStartOfHour(ts) AS hour SELECT count() AS n FROM legacy_speech_units_v1 FINAL WHERE {predicate} AND basis='legacy_speech_contract_v1'"
        );
        let history = super::stats_cache::query(state, &history_sql, params, false).await?;
        Ok(history.first().is_some_and(|row| ch_i64(row, "n") > 0))
    }

    /// 唯一还要探测的是窗口里有没有历史字符口径（老语音计费）：有的话 `prompt_tokens`
    /// 要做字符校正。
    pub async fn read(
        state: &AppState,
        predicate: &str,
        params: &[(&str, &str)],
    ) -> Result<Self, AppError> {
        Ok(Self {
            historical_units: Self::has_historical_units(state, predicate, params).await?,
        })
    }
}
