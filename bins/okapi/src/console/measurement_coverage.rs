//! Cheap coverage probes choose a small query before expanding legacy recovery.
//! A fast path is allowed only when counts agree at every grain, not just in total.
use super::stats::ch_i64;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;

const KEYS: &str = "hour, user_id, api_key_id, group_code, model, channel_id";
const DIMS: &str = "requested_model, upstream_model, endpoint, upstream_endpoint, node, stream, request_type, billing_type";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Mode {
    Aggregate,
    RawComplete,
    #[default]
    Recover,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Modes {
    pub ttft: Mode,
    pub latency: Mode,
    pub usage: Mode,
    pub details: Mode,
    pub ttl: Mode,
    pub cache: Mode,
    pub units: Mode,
    pub output_rate: Mode,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Coverage {
    pub detail: Modes,
    pub legacy: Modes,
    pub historical_units: bool,
}

/// For complete raw history, retain complete aggregate grains and recover only
/// the other grains. This still prefers the aggregate on ties, never sums overlap.
pub(super) fn fast_source(
    mode: Mode,
    keys: &str,
    expected_count: &str,
    aggregate_count: &str,
    aggregate: &str,
    raw: &str,
) -> String {
    if mode == Mode::Aggregate {
        return format!("({aggregate} GROUP BY {keys})");
    }
    debug_assert_eq!(mode, Mode::RawComplete);
    format!(
        "(WITH missing AS (SELECT {keys} FROM ( \
            SELECT {keys}, toInt64(n) AS delta FROM ({expected_count}) UNION ALL \
            SELECT {keys}, -toInt64(n) AS delta FROM ({aggregate_count})) \
            GROUP BY {keys} HAVING sum(delta) != 0) \
        SELECT * FROM ({aggregate} AND ({keys}) NOT IN (SELECT {keys} FROM missing) GROUP BY {keys}) \
        UNION ALL SELECT * FROM ({raw} AND ({keys}) IN (SELECT {keys} FROM missing) GROUP BY {keys}))"
    )
}

fn counts(keys: &str, table: &str, predicate: &str, raw: bool) -> String {
    let (time, count) = if raw {
        ("toStartOfHour(ts) AS hour", "count()")
    } else {
        (
            super::observation_sources::Grain::for_table(table).time_sql(),
            "countMerge(requests)",
        )
    };
    format!(
        "WITH {time} SELECT toJSONString(tuple({keys})) AS grain, {count} AS n FROM {table} WHERE {predicate} GROUP BY {keys}"
    )
}

impl Coverage {
    /// Whether the window holds historical character units. Confirm absence in the full
    /// base scope; this sparse probe is deliberately fresh so an old cached absence
    /// cannot hide proof.
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

    /// Core-only queries read nothing but `prompt_tokens` and friends straight from the
    /// materialized views; the single thing they need to know is whether `prompt_tokens`
    /// needs the historical character correction. The other modes (left at the safe
    /// `Recover` default) only matter to the full source, so skip its 16-branch probe.
    pub async fn read_historical(
        state: &AppState,
        predicate: &str,
        params: &[(&str, &str)],
    ) -> Result<Self, AppError> {
        Ok(Self {
            historical_units: Self::has_historical_units(state, predicate, params).await?,
            ..Self::default()
        })
    }

    pub async fn read(
        state: &AppState,
        predicate: &str,
        params: &[(&str, &str)],
        cached: bool,
    ) -> Result<Self, AppError> {
        let historical_units = Self::has_historical_units(state, predicate, params).await?;
        let mut branches = Vec::new();
        for (level, keys, expected) in [
            ("detail", format!("{KEYS}, {DIMS}"), "mv_analysis_hour"),
            ("legacy", KEYS.to_owned(), "mv_cube_hour"),
        ] {
            for (kind, table) in std::iter::once(("expected", expected)).chain(
                super::observation_sources::SOURCES
                    .iter()
                    .map(|source| (source.name, source.table)),
            ) {
                let query = counts(&keys, table, predicate, false);
                branches.push(format!(
                    "SELECT '{level}' AS level, '{kind}' AS kind, grain, n FROM ({query})"
                ));
            }
        }
        let rows = super::stats_cache::query(state, &format!(
            "SELECT level, countIf(e != t) AS ttft, countIf(e != l) AS latency, countIf(e != u) AS usage, countIf(e != d) AS details, countIf(e != w) AS ttl, countIf(e != c) AS cache, countIf(e != i) AS units, countIf(e != o) AS output_rate FROM ( \
            SELECT level, grain, sumIf(n, kind = 'expected') AS e, sumIf(n, kind = 'ttft') AS t, \
                sumIf(n, kind = 'latency') AS l, sumIf(n, kind = 'usage') AS u, sumIf(n, kind = 'details') AS d, sumIf(n, kind = 'ttl') AS w, sumIf(n, kind = 'cache') AS c, sumIf(n, kind = 'units') AS i, sumIf(n, kind = 'output_rate') AS o \
            FROM ({}) GROUP BY level, grain) GROUP BY level", branches.join(" UNION ALL ")
        ), params, cached).await?;
        let missing = |level: &str, kind: &str| {
            rows.iter()
                .find(|r| r["level"].as_str() == Some(level))
                .is_some_and(|r| ch_i64(r, kind) > 0)
        };
        // Most modern installations stop after one small aggregate-only probe.
        // Raw retention is examined only for levels with incomplete aggregates.
        let mut raw_branches = Vec::new();
        for (level, keys, table) in [
            ("detail", format!("{KEYS}, {DIMS}"), "mv_analysis_hour"),
            ("legacy", KEYS.to_owned(), "mv_cube_hour"),
        ] {
            if [
                "ttft",
                "latency",
                "usage",
                "details",
                "ttl",
                "cache",
                "units",
                "output_rate",
            ]
            .iter()
            .any(|kind| missing(level, kind))
            {
                for (sign, source, raw) in [(1, table, false), (-1, "request_log_calls", true)] {
                    let query = counts(&keys, source, predicate, raw);
                    raw_branches.push(format!("SELECT '{level}' AS level, grain, {sign} * toInt64(n) AS delta FROM ({query})"));
                }
            }
        }
        let raw_rows = if raw_branches.is_empty() {
            Vec::new()
        } else {
            super::stats_cache::query(state, &format!(
                "SELECT level, countIf(difference != 0) AS missing FROM (SELECT level, grain, sum(delta) AS difference FROM ({}) GROUP BY level, grain) GROUP BY level",
                raw_branches.join(" UNION ALL ")
            ), params, cached).await?
        };
        let modes = |level: &str| {
            let raw_complete = raw_rows
                .iter()
                .find(|r| r["level"].as_str() == Some(level))
                .is_some_and(|r| ch_i64(r, "missing") == 0);
            let mode = |kind| {
                if !missing(level, kind) {
                    Mode::Aggregate
                } else if raw_complete {
                    Mode::RawComplete
                } else {
                    Mode::Recover
                }
            };
            Modes {
                ttft: mode("ttft"),
                latency: mode("latency"),
                usage: mode("usage"),
                details: mode("details"),
                ttl: mode("ttl"),
                cache: mode("cache"),
                units: mode("units"),
                output_rate: mode("output_rate"),
            }
        };
        Ok(Self {
            detail: modes("detail"),
            legacy: modes("legacy"),
            historical_units,
        })
    }
}
