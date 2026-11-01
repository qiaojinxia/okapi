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

/// 额度预判（见 `forecast`）对这条渠道怎么起作用。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForecastMode {
    /// 不评估（也不写诊断）。
    Off,
    /// 只评估并写进请求诊断，不影响选路。
    #[default]
    Shadow,
    /// 按评估排除「这个请求会把窗口推过线」的候选。
    Enforce,
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
    /// 同一批窗口的「保留线」：缓存还热的会话（粘性命中且距上次请求不到 5 分钟）可以用到这条线，
    /// 新放置与冷请求仍按 `quota_limits`。缺省 = 没有保留线（与放置线相同，行为不变）。
    /// 保留线低于放置线按放置线算。
    pub quota_keep_limits: BTreeMap<i64, u8>,
    /// 额度预判：缺省 `shadow`（只记诊断）；`enforce` 才据此排除候选；`off` 关掉。
    pub quota_forecast: ForecastMode,
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
            quota_keep_limits: BTreeMap::new(),
            quota_forecast: ForecastMode::default(),
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
            || policy.quota_keep_limits.len() > 8
            || policy.quota_keep_limits.iter().any(|(seconds, percent)| {
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
            json!({"quota_keep_limits":{"18000":101}}),
            json!({"quota_keep_limits":{"0":50}}),
            json!({"quota_forecast":"auto"}),
        ] {
            assert!(Policy::parse(&json!({"account_control":value})).is_err());
        }
        assert!(Policy::parse(&json!({"account_control":{"usage":{"period":"month","requests":100,"tokens":10000,"cost_micro":1_000_000},"refresh_mode":"external"}})).unwrap().usage.enabled());
    }

    /// 预判缺省只记诊断；保留线缺省为空，既有渠道的行为不变。
    #[test]
    fn forecast_defaults_to_shadow_without_keep_lines() {
        let policy = Policy::parse(&json!({})).unwrap();
        assert_eq!(policy.quota_forecast, ForecastMode::Shadow);
        assert!(policy.quota_keep_limits.is_empty());
        let policy = Policy::parse(&json!({"account_control":{
            "quota_limits":{"18000":80},
            "quota_keep_limits":{"18000":95},
            "quota_forecast":"enforce"
        }}))
        .unwrap();
        assert_eq!(policy.quota_forecast, ForecastMode::Enforce);
        assert_eq!(policy.quota_keep_limits.get(&18_000), Some(&95));
        // 序列化回去能原样读回（管理端保存时整份回传，字段不能丢）
        let echoed = serde_json::to_value(&policy).unwrap();
        assert_eq!(echoed["quota_forecast"], "enforce");
        assert_eq!(echoed["quota_keep_limits"], json!({"18000": 95}));
        let off = Policy::parse(&json!({"account_control":{"quota_forecast":"off"}})).unwrap();
        assert_eq!(off.quota_forecast, ForecastMode::Off);
    }
}
