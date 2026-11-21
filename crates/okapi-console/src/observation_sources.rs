//! Grain/calendar metadata shared by ClickHouse observation query builders.

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
        assert_eq!(Grain::for_table("mv_analysis_5min"), Grain::FiveMinutes);
        assert_eq!(Grain::for_table("mv_model_hour"), Grain::Hour);
    }
}
