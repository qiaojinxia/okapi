//! Channel controls belong to scheduling, never to a provider's wire profile.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenPeriod {
    #[default]
    Total,
    Day,
    Week,
}

impl TokenPeriod {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::Day => "day",
            Self::Week => "week",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TokenLimit {
    pub cap: i64,
    #[serde(default)]
    pub period: TokenPeriod,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

impl Period {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageLimits {
    pub period: Period,
    pub requests: Option<i64>,
    pub tokens: Option<i64>,
    pub cost_micro: Option<i64>,
}

impl UsageLimits {
    pub fn enabled(&self) -> bool {
        self.requests.is_some() || self.tokens.is_some() || self.cost_micro.is_some()
    }
    fn valid(&self) -> bool {
        [self.requests, self.tokens, self.cost_micro]
            .into_iter()
            .flatten()
            .all(|n| (1..=9_007_199_254_740_991).contains(&n))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RefreshMode {
    #[default]
    Managed,
    External,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Deserialize and validate historical settings only. Local budget admission
    /// has been retired; never expose it as an active control in API responses.
    #[serde(skip_serializing)]
    pub usage: UsageLimits,
    pub quota_aware: bool,
    pub quota_threshold_pct: u8,
    /// Keys are upstream window durations in seconds, never provider identifiers.
    pub quota_limits: BTreeMap<i64, u8>,
    pub local_tokens: Option<TokenLimit>,
    pub rate_limit_cooldown_secs: i64,
    pub failure_threshold: i64,
    pub failure_cooldown_secs: i64,
    pub refresh_mode: RefreshMode,
    pub refresh_margin_secs: i64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            usage: UsageLimits::default(),
            quota_aware: false,
            quota_threshold_pct: 90,
            quota_limits: BTreeMap::new(),
            local_tokens: None,
            rate_limit_cooldown_secs: 60,
            failure_threshold: 3,
            failure_cooldown_secs: 60,
            refresh_mode: RefreshMode::Managed,
            refresh_margin_secs: 120,
        }
    }
}

impl Policy {
    pub fn parse(settings: &Value) -> Result<Self, &'static str> {
        let policy = match settings.get("account_control") {
            None => Self::default(),
            Some(value) => {
                serde_json::from_value::<Self>(value.clone()).map_err(|_| "account_control")?
            }
        };
        if !policy.usage.valid()
            || policy
                .local_tokens
                .as_ref()
                .is_some_and(|limit| !(1..=9_007_199_254_740_991).contains(&limit.cap))
            || policy.quota_limits.len() > 8
            || policy.quota_limits.iter().any(|(seconds, percent)| {
                !(1..=31_536_000).contains(seconds) || !(1..=100).contains(percent)
            })
            || !(1..=100).contains(&policy.quota_threshold_pct)
            || !(1..=604_800).contains(&policy.rate_limit_cooldown_secs)
            || !(1..=20).contains(&policy.failure_threshold)
            || !(1..=7200).contains(&policy.failure_cooldown_secs)
            || !(120..=3600).contains(&policy.refresh_margin_secs)
        {
            return Err("account_control");
        }
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn absent_policy_preserves_defaults_and_bad_limits_fail_closed() {
        assert!(!Policy::parse(&json!({})).unwrap().usage.enabled());
        for value in [
            json!(null),
            json!({"usage":{"requests":0}}),
            json!({"usage":{"tokens":-1}}),
            json!({"usage":{"period":"year"}}),
            json!({"quota_threshold_pct":101}),
            json!({"refresh_mode":"automatic"}),
            json!({"rate_limit_cooldown_secs":0}),
            json!({"usage":{"cost_micro":1.5}}),
            json!({"request_limit":100}),
            json!({"local_tokens":{"cap":0}}),
            json!({"local_tokens":{"cap":1,"period":"hour"}}),
            json!({"quota_limits":{"18000":101}}),
            json!({"quota_limits":{"0":50}}),
        ] {
            assert!(Policy::parse(&json!({"account_control":value})).is_err());
        }
        assert!(Policy::parse(&json!({"account_control":{"usage":{"period":"month","requests":100,"tokens":10000,"cost_micro":1_000_000},"refresh_mode":"external"}})).unwrap().usage.enabled());
    }
}
