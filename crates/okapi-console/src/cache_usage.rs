//! Cache quantities and sample counts always describe one selected population.
use okapi_store::{ChClient, StoreError};
use serde_json::Value;

pub(super) const FIELDS: [&str; 4] = [
    "cache_observed",
    "cache_writes",
    "cache_write_n",
    "cache_read_n",
];
const MERGED: &str = "countMerge(requests) AS cache_observed, sumMerge(cache_writes) AS cache_writes, countIfMerge(cache_write_n) AS cache_write_n, countIfMerge(cache_read_n) AS cache_read_n";

/// 缓存数量与样本数读预聚合 `mv_cache_totals_5min`；`cache_expected` 即该粒度的请求数。
/// keys / predicate 都是校验过的内部 SQL，用户字符串保持参数绑定。
pub(super) fn source(keys: &str, predicate: &str) -> String {
    let keys = if keys.is_empty() {
        "source_scope"
    } else {
        keys
    };
    let aggregate = format!(
        "WITH toStartOfHour(ts5) AS hour, toDate(ts5) AS day, toUInt8(1) AS source_scope SELECT {keys}, {MERGED} FROM mv_cache_totals_5min WHERE {predicate}"
    );
    let selected = super::measurement_coverage::aggregate_source(keys, &aggregate);
    format!("(SELECT *, toInt64(cache_observed) AS cache_expected FROM {selected})")
}

pub(super) async fn query(
    ch: &ChClient,
    keys: &str,
    predicate: &str,
) -> Result<Vec<Value>, StoreError> {
    ch.query_json_each_row(&format!("SELECT * FROM {}", source(keys, predicate)))
        .await
}
