//! Registry of retained observation sources and their field ownership.
//! Recovery strategies remain in the owning modules; grain/calendar metadata
//! and join ownership have one definition shared by probes and query builders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Ttft,
    Latency,
    Usage,
    Details,
    Ttl,
    Cache,
    Units,
    OutputRate,
}

pub(super) struct Source {
    pub kind: Kind,
    pub name: &'static str,
    pub table: &'static str,
    pub alias: &'static str,
    pub fields: &'static [&'static str],
}

pub(super) const SOURCES: &[Source] = &[
    Source {
        kind: Kind::Ttft,
        name: "ttft",
        table: "mv_ttft_reporting_hour",
        alias: "tf",
        fields: &["ttft_sum", "ttft_samples", "ttft_observed"],
    },
    Source {
        kind: Kind::Latency,
        name: "latency",
        table: "mv_latency_reporting_hour",
        alias: "lf",
        fields: &[
            "latency_sum",
            "latency_samples",
            "latency_observed",
            "latency_output",
        ],
    },
    Source {
        kind: Kind::Usage,
        name: "usage",
        table: "mv_usage_sources_5min",
        alias: "us",
        fields: &super::usage_sources::FIELDS,
    },
    Source {
        kind: Kind::Ttl,
        name: "ttl",
        table: "mv_cache_ttl_5min",
        alias: "td",
        fields: &super::token_details::TTL_FIELDS,
    },
    Source {
        kind: Kind::Details,
        name: "details",
        table: "mv_token_details_5min",
        alias: "td",
        fields: &DETAILS,
    },
    Source {
        kind: Kind::Cache,
        name: "cache",
        table: "mv_cache_totals_5min",
        alias: "cs",
        fields: &super::cache_usage::FIELDS,
    },
    Source {
        kind: Kind::Units,
        name: "units",
        table: "mv_input_units_5min",
        alias: "td",
        fields: &super::input_units::FIELDS,
    },
    Source {
        kind: Kind::OutputRate,
        name: "output_rate",
        table: "mv_output_rate_5min",
        alias: "rf",
        fields: &super::output_rate::FIELDS,
    },
];
// TTL has its own source/population even though token_details assembles the view.
const DETAILS: [&str; 19] = {
    let mut fields = [""; 19];
    let mut i = 0;
    while i < fields.len() {
        fields[i] = super::token_details::FIELDS[i];
        i += 1;
    }
    fields
};

pub(super) fn owner(field: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|source| source.fields.contains(&field))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Grain {
    FiveMinutes,
    Hour,
    Day,
}
impl Grain {
    pub fn for_table(table: &str) -> Self {
        if table.ends_with("_5min") {
            Self::FiveMinutes
        } else if table.ends_with("_day") {
            Self::Day
        } else {
            Self::Hour
        }
    }
    pub fn time_sql(self) -> &'static str {
        match self {
            Self::FiveMinutes => "toStartOfHour(ts5) AS hour, toDate(ts5) AS day",
            Self::Hour => "toDate(hour) AS day",
            Self::Day => "toStartOfDay(day) AS hour",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_observation_field_has_exactly_one_owner() {
        let mut names = std::collections::HashSet::new();
        for source in SOURCES {
            assert!(!source.fields.is_empty());
            for field in source.fields {
                assert!(names.insert(field), "ambiguous field {field}");
            }
        }
        for (field, function) in super::super::analysis_source::METRICS {
            assert!(
                owner(field).is_some() || !function.is_empty(),
                "unregistered observation {field}"
            );
        }
    }
    #[test]
    fn all_daily_sources_use_the_same_calendar_grain() {
        for table in [
            "mv_user_day",
            "mv_group_day",
            "mv_client_day",
            "mv_key_model_day",
            "mv_apikey_day",
        ] {
            assert_eq!(Grain::for_table(table), Grain::Day);
        }
        assert_eq!(Grain::for_table("mv_channel_5min"), Grain::FiveMinutes);
        assert_eq!(Grain::for_table("mv_analysis_hour"), Grain::Hour);
    }
}
