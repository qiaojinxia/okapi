//! 后台任务的策略（存在 `settings` 里）：控制台写入时校验，worker 按它跑，网关的出口核实也读同一份。
//! 只有形状、缺省与取值域，不碰任何 IO。
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `settings.oauth_refresh_policy`：订阅 OAuth 凭证的后台预刷新（worker `oauth_refresh`）。
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RefreshPolicy {
    pub enabled: bool,
    pub interval_secs: u64,
    pub refresh_margin_secs: i64,
    pub batch_size: u16,
    pub concurrency: u8,
    pub requests_per_second: u8,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 30,
            refresh_margin_secs: 300,
            batch_size: 100,
            concurrency: 2,
            requests_per_second: 1,
        }
    }
}

impl RefreshPolicy {
    pub fn parse(value: &Value) -> Option<Self> {
        let policy = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(value.clone()).ok()?
        };
        ((5..=300).contains(&policy.interval_secs)
            && (120..=3600).contains(&policy.refresh_margin_secs)
            && (1..=1000).contains(&policy.batch_size)
            && (1..=8).contains(&policy.concurrency)
            && (1..=5).contains(&policy.requests_per_second))
        .then_some(policy)
    }
}

/// `settings.egress_probe_policy`；缺省开启、10 分钟一轮。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProbePolicy {
    pub enabled: bool,
    pub interval_secs: u64,
    /// 探测地址；None = Cloudflare trace（一次拿到出口 IP 与国家）。
    pub target: Option<String>,
    pub concurrency: u8,
}

impl Default for ProbePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 600,
            target: None,
            concurrency: 4,
        }
    }
}

impl ProbePolicy {
    /// 形状与取值域校验（设置写入与 worker 读取共用）。探测地址只认 http(s) 绝对 URL；
    /// SSRF 闸由写入端（console）另行检查。
    pub fn parse(value: &Value) -> Option<Self> {
        let policy = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(value.clone()).ok()?
        };
        let target_ok = policy.target.as_deref().is_none_or(|target| {
            reqwest::Url::parse(target)
                .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
        });
        ((60..=86_400).contains(&policy.interval_secs)
            && (1..=16).contains(&policy.concurrency)
            && target_ok)
            .then_some(policy)
    }

    pub fn target(&self) -> &str {
        self.target
            .as_deref()
            .unwrap_or(okapi_providers::egress_probe::DEFAULT_TARGET)
    }
}

/// `settings` 里探测策略的键。
pub const EGRESS_PROBE_SETTING: &str = "egress_probe_policy";
