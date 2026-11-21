//! Cache quantities and sample counts read from unified `mv_analysis_5min`.

#[cfg(test)]
pub(super) const FIELDS: [&str; 4] = [
    "cache_observed",
    "cache_writes",
    "cache_write_n",
    "cache_read_n",
];

pub(super) const MERGED: &str = "countMerge(requests) AS cache_observed, sumMerge(cache_writes) AS cache_writes, countIfMerge(cache_write_n) AS cache_write_n, countIfMerge(cache_read_n) AS cache_read_n";
