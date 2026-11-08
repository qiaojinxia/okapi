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

/// `settings.channel_probe_policy`：渠道 key 的后台探测与自动恢复（worker `channel_probe`）。缺省**关**——
/// 探测会在上游真实产生请求（一次 16 token 的最小补全），要站长明确打开。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelProbePolicy {
    pub enabled: bool,
    pub mode: ChannelProbeMode,
    /// 同一把 key 两次探测的最小间隔（秒）。
    pub interval_secs: u64,
    pub concurrency: u8,
    /// 每一轮最多探几把 key。
    pub batch_size: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelProbeMode {
    /// 只探被摘出轮转的 key（冷却中 / 配额耗尽 / 凭证失效）：探通就放回去。
    #[default]
    Recover,
    /// 连正常的 key 也定期探：探不通按真实失败登记（同一张状态机表），不用等客户端先踩一脚。
    All,
}

impl Default for ChannelProbePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ChannelProbeMode::Recover,
            interval_secs: 900,
            concurrency: 2,
            batch_size: 50,
        }
    }
}

impl ChannelProbePolicy {
    /// 形状与取值域校验（设置写入与 worker 读取共用）。
    pub fn parse(value: &Value) -> Option<Self> {
        let policy = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(value.clone()).ok()?
        };
        ((60..=86_400).contains(&policy.interval_secs)
            && (1..=8).contains(&policy.concurrency)
            && (1..=200).contains(&policy.batch_size))
        .then_some(policy)
    }
}

/// `settings` 里渠道探测策略的键。
pub const CHANNEL_PROBE_SETTING: &str = "channel_probe_policy";

/// 一组 HTTP 状态码范围，JSON 里写成字符串数组：`["404", "520-529"]`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusRanges(Vec<(u16, u16)>);

impl StatusRanges {
    pub fn contains(&self, status: u16) -> bool {
        self.0
            .iter()
            .any(|(from, to)| (*from..=*to).contains(&status))
    }

    fn parse_one(text: &str) -> Option<(u16, u16)> {
        let text = text.trim();
        let (from, to) = if let Some((from, to)) = text.split_once('-') {
            (
                from.trim().parse::<u16>().ok()?,
                to.trim().parse::<u16>().ok()?,
            )
        } else {
            let single = text.parse::<u16>().ok()?;
            (single, single)
        };
        ((100..=599).contains(&from) && (100..=599).contains(&to) && from <= to)
            .then_some((from, to))
    }
}

impl<'de> Deserialize<'de> for StatusRanges {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let texts = Vec::<String>::deserialize(deserializer)?;
        if texts.len() > MAX_POLICY_ENTRIES {
            return Err(serde::de::Error::custom("too many status ranges"));
        }
        texts
            .iter()
            .map(|text| Self::parse_one(text))
            .collect::<Option<Vec<_>>>()
            .map(Self)
            .ok_or_else(|| {
                serde::de::Error::custom(
                    "status range must look like \"404\" or \"520-529\" within 100-599",
                )
            })
    }
}

impl Serialize for StatusRanges {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(|(from, to)| {
            if from == to {
                from.to_string()
            } else {
                format!("{from}-{to}")
            }
        }))
    }
}

/// 状态码范围、关键词各自最多几条。
const MAX_POLICY_ENTRIES: usize = 32;
/// 只在错误正文开头这么多字节里找关键词（上游的错误体通常几百字节，网页类的大体不值得扫）。
const KEYWORD_SCAN_BYTES: usize = 16 * 1024;

/// `settings.request_policy`：上游请求失败后怎么重试、怎么判 key 的状态。缺省 = 现行行为，只在站长写了才变。
/// 状态码和关键词是**追加**规则：默认的重试矩阵与账号级错误识别照常生效，这里只多认几种。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RequestPolicy {
    /// 一个请求最多试几个渠道 / key（含第一次）。
    pub max_attempts: u8,
    /// 默认的重试矩阵之外，上游回这些状态码时也换渠道重试（对话 / 向量 / 计数；图片、视频、语音不是幂等的，不受影响）。
    pub retry_statuses: StatusRanges,
    /// 上游回这些状态码时按「凭证失效」登记 key（摘出轮转）。
    pub invalid_statuses: StatusRanges,
    /// 错误正文含其中任一关键词（不分大小写）时按「凭证失效」登记 key。
    pub invalid_keywords: Vec<String>,
    /// 错误正文含其中任一关键词时按「配额耗尽」登记 key（冷却到次日 0 点）。
    pub quota_keywords: Vec<String>,
}

/// 请求策略对一次上游错误的判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyVerdict {
    None,
    Invalid,
    QuotaExhausted,
}

impl Default for RequestPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            retry_statuses: StatusRanges::default(),
            invalid_statuses: StatusRanges::default(),
            invalid_keywords: Vec::new(),
            quota_keywords: Vec::new(),
        }
    }
}

impl RequestPolicy {
    /// 形状与取值域校验（设置写入与网关读取共用）。关键词统一小写、去首尾空白、去重。
    pub fn parse(value: &Value) -> Option<Self> {
        let mut policy = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(value.clone()).ok()?
        };
        for keywords in [&mut policy.invalid_keywords, &mut policy.quota_keywords] {
            if keywords.len() > MAX_POLICY_ENTRIES {
                return None;
            }
            let mut normalized: Vec<String> = Vec::with_capacity(keywords.len());
            for keyword in keywords.iter() {
                let keyword = keyword.trim().to_lowercase();
                if keyword.is_empty() || keyword.chars().count() > 64 {
                    return None;
                }
                if !normalized.contains(&keyword) {
                    normalized.push(keyword);
                }
            }
            *keywords = normalized;
        }
        (1..=10).contains(&policy.max_attempts).then_some(policy)
    }

    /// 默认矩阵之外，这个状态码也换渠道重试吗。
    pub fn extra_retry(&self, status: u16) -> bool {
        self.retry_statuses.contains(status)
    }

    /// 一次上游错误按策略该判成什么（优先级：凭证失效的状态码 → 凭证失效的关键词 → 配额耗尽的关键词）。
    pub fn classify(&self, status: u16, body: &[u8]) -> PolicyVerdict {
        if self.invalid_statuses.contains(status) {
            return PolicyVerdict::Invalid;
        }
        if self.invalid_keywords.is_empty() && self.quota_keywords.is_empty() {
            return PolicyVerdict::None;
        }
        let head = &body[..body.len().min(KEYWORD_SCAN_BYTES)];
        let text = String::from_utf8_lossy(head).to_lowercase();
        if self
            .invalid_keywords
            .iter()
            .any(|keyword| text.contains(keyword.as_str()))
        {
            PolicyVerdict::Invalid
        } else if self
            .quota_keywords
            .iter()
            .any(|keyword| text.contains(keyword.as_str()))
        {
            PolicyVerdict::QuotaExhausted
        } else {
            PolicyVerdict::None
        }
    }
}

/// `settings` 里请求策略的键。
pub const REQUEST_POLICY_SETTING: &str = "request_policy";

/// 渠道余额低于阈值时怎么办。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LowBalanceAction {
    /// 只发 `channel_balance_low` 通知，渠道照常调度。
    #[default]
    Notify,
    /// 通知，并把渠道停用（渠道状态 2）；余额回到阈值之上、且是这里停的，自动再启用。
    Stop,
}

/// `settings.channel_balance_policy`：渠道上游余额的定时刷新（worker `channel_balance`）。缺省**关**——
/// 刷新会定时请求每个渠道的上游余额接口，要站长明确打开。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelBalancePolicy {
    pub enabled: bool,
    /// 同一个渠道两次刷新的最小间隔（秒）。
    pub interval_secs: u64,
    pub concurrency: u8,
    /// 每一轮最多刷新几个渠道。
    pub batch_size: u16,
    /// 渠道自己的 `low_balance` 没写动作时用它。
    pub low_action: LowBalanceAction,
}

impl Default for ChannelBalancePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 3600,
            concurrency: 2,
            batch_size: 50,
            low_action: LowBalanceAction::Notify,
        }
    }
}

impl ChannelBalancePolicy {
    /// 形状与取值域校验（设置写入与 worker 读取共用）。
    pub fn parse(value: &Value) -> Option<Self> {
        let policy = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(value.clone()).ok()?
        };
        ((300..=86_400).contains(&policy.interval_secs)
            && (1..=4).contains(&policy.concurrency)
            && (1..=200).contains(&policy.batch_size))
        .then_some(policy)
    }
}

/// `settings` 里渠道余额刷新策略的键。
pub const CHANNEL_BALANCE_SETTING: &str = "channel_balance_policy";

/// 渠道 `settings.low_balance`：这个渠道的余额低到什么程度算「低」。金额是上游余额接口报的那个货币的 micro 整数
/// （`channel_balance_policy` 刷新出来的 `balance_micro` 同一个单位）；余额小于等于阈值即为低。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LowBalance {
    pub threshold_micro: i64,
    /// 缺省用 `channel_balance_policy.low_action`。
    #[serde(default)]
    pub action: Option<LowBalanceAction>,
}

/// 阈值的上限（10^15 micro = 10^9 个货币单位）：只为拦住手滑写的天文数字。
const MAX_LOW_BALANCE_MICRO: i64 = 1_000_000_000_000_000;

impl LowBalance {
    /// 从渠道 `settings` 读：没写（或写 null）= `Ok(None)`；写了但形状 / 取值不对 = `Err("low_balance")`（写入校验当 param 用）。
    pub fn from_channel_settings(settings: &Value) -> Result<Option<Self>, &'static str> {
        let Some(value) = settings.get("low_balance").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let parsed = serde_json::from_value::<Self>(value.clone()).map_err(|_| "low_balance")?;
        if (0..=MAX_LOW_BALANCE_MICRO).contains(&parsed.threshold_micro) {
            Ok(Some(parsed))
        } else {
            Err("low_balance")
        }
    }

    /// 这个渠道实际用的动作：渠道自己写的优先，没写用全局缺省。
    #[must_use]
    pub fn action_under(&self, policy: &ChannelBalancePolicy) -> LowBalanceAction {
        self.action.unwrap_or(policy.low_action)
    }

    /// 余额算不算低。
    #[must_use]
    pub fn is_low(&self, balance_micro: i64) -> bool {
        balance_micro <= self.threshold_micro
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn channel_probing_is_off_until_the_operator_turns_it_on() {
        assert!(!ChannelProbePolicy::default().enabled);
        assert_eq!(
            ChannelProbePolicy::parse(&Value::Null),
            Some(ChannelProbePolicy::default())
        );
        let on = ChannelProbePolicy::parse(&json!({"enabled": true})).unwrap();
        assert!(on.enabled);
        assert_eq!(
            on.mode,
            ChannelProbeMode::Recover,
            "recover-only unless asked for more"
        );
    }

    #[test]
    fn channel_probe_policy_rejects_unknown_fields_and_out_of_range_values() {
        for bad in [
            json!({"mode": "everything"}),
            json!({"interval_secs": 59}),
            json!({"interval_secs": 86_401}),
            json!({"concurrency": 0}),
            json!({"concurrency": 9}),
            json!({"batch_size": 0}),
            json!({"batch_size": 201}),
            json!({"enable": true}),
            json!("on"),
        ] {
            assert!(ChannelProbePolicy::parse(&bad).is_none(), "{bad}");
        }
        let all = ChannelProbePolicy::parse(
            &json!({"mode": "all", "interval_secs": 60, "concurrency": 8, "batch_size": 200}),
        );
        assert_eq!(all.map(|p| p.mode), Some(ChannelProbeMode::All));
    }

    #[test]
    fn request_policy_defaults_keep_todays_behaviour() {
        let policy = RequestPolicy::parse(&Value::Null).unwrap();
        assert_eq!(policy, RequestPolicy::default());
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(
            policy.classify(401, b"anything"),
            PolicyVerdict::None,
            "the built-in table handles 401, not the policy"
        );
        assert!(!policy.extra_retry(404));
    }

    #[test]
    fn status_ranges_read_single_codes_and_spans() {
        let policy = RequestPolicy::parse(
            &json!({"retry_statuses": ["404", "520-529"], "invalid_statuses": [" 418 "]}),
        )
        .unwrap();
        for hit in [404, 520, 525, 529] {
            assert!(policy.extra_retry(hit), "{hit}");
        }
        for miss in [403, 405, 519, 530] {
            assert!(!policy.extra_retry(miss), "{miss}");
        }
        assert_eq!(policy.classify(418, b""), PolicyVerdict::Invalid);
        let again = serde_json::to_value(&policy).unwrap();
        assert_eq!(
            again["retry_statuses"],
            json!(["404", "520-529"]),
            "serialises back to the same strings"
        );
    }

    #[test]
    fn keywords_match_case_insensitively_and_invalid_beats_quota() {
        let policy = RequestPolicy::parse(&json!({
            "invalid_keywords": ["Account Suspended", "封禁"],
            "quota_keywords": ["credit balance is too low", "ACCOUNT SUSPENDED"],
        }))
        .unwrap();
        assert_eq!(
            policy.classify(
                400,
                br#"{"error":{"message":"Your ACCOUNT SUSPENDED for abuse"}}"#
            ),
            PolicyVerdict::Invalid
        );
        assert_eq!(
            policy.classify(500, "账号已被封禁".as_bytes()),
            PolicyVerdict::Invalid
        );
        assert_eq!(
            policy.classify(400, br#"{"error":"Credit balance is too low"}"#),
            PolicyVerdict::QuotaExhausted
        );
        assert_eq!(
            policy.classify(400, b"plain bad request"),
            PolicyVerdict::None
        );
    }

    #[test]
    fn keywords_are_only_searched_in_the_start_of_the_body() {
        let policy = RequestPolicy::parse(&json!({"quota_keywords": ["needle"]})).unwrap();
        let mut late = vec![b'x'; KEYWORD_SCAN_BYTES];
        late.extend_from_slice(b"needle");
        assert_eq!(policy.classify(400, &late), PolicyVerdict::None);
        let mut early = b"needle".to_vec();
        early.extend(std::iter::repeat_n(b'x', KEYWORD_SCAN_BYTES));
        assert_eq!(policy.classify(400, &early), PolicyVerdict::QuotaExhausted);
    }

    #[test]
    fn request_policy_rejects_bad_shapes_and_out_of_range_values() {
        let too_many: Vec<String> = (0..33).map(|n| format!("kw{n}")).collect();
        for bad in [
            json!({"max_attempts": 0}),
            json!({"max_attempts": 11}),
            json!({"retry_statuses": ["99"]}),
            json!({"retry_statuses": ["600"]}),
            json!({"retry_statuses": ["500-400"]}),
            json!({"retry_statuses": ["abc"]}),
            json!({"retry_statuses": [404]}),
            json!({"invalid_keywords": [""]}),
            json!({"invalid_keywords": ["  "]}),
            json!({"invalid_keywords": ["x".repeat(65)]}),
            json!({"quota_keywords": too_many}),
            json!({"retry": [404]}),
            json!("on"),
        ] {
            assert!(RequestPolicy::parse(&bad).is_none(), "{bad}");
        }
        let kept =
            RequestPolicy::parse(&json!({"invalid_keywords": [" Foo ", "foo", "BAR"]})).unwrap();
        assert_eq!(
            kept.invalid_keywords,
            vec!["foo".to_owned(), "bar".to_owned()],
            "trimmed, lower-cased, deduplicated"
        );
    }

    #[test]
    fn balance_refresh_is_off_until_the_operator_turns_it_on() {
        let policy = ChannelBalancePolicy::parse(&Value::Null).unwrap();
        assert!(!policy.enabled);
        assert_eq!(policy, ChannelBalancePolicy::default());
        assert_eq!(
            policy.low_action,
            LowBalanceAction::Notify,
            "never stops a channel unless told to"
        );
        let on =
            ChannelBalancePolicy::parse(&json!({"enabled": true, "low_action": "stop"})).unwrap();
        assert!(on.enabled);
        assert_eq!(on.low_action, LowBalanceAction::Stop);
    }

    #[test]
    fn balance_policy_rejects_unknown_fields_and_out_of_range_values() {
        for bad in [
            json!({"interval_secs": 299}),
            json!({"interval_secs": 86_401}),
            json!({"concurrency": 0}),
            json!({"concurrency": 5}),
            json!({"batch_size": 0}),
            json!({"batch_size": 201}),
            json!({"low_action": "disable"}),
            json!({"enable": true}),
            json!("on"),
        ] {
            assert!(ChannelBalancePolicy::parse(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_channels_low_balance_rule_is_read_from_its_settings() {
        let none = json!({"health_probe": "off"});
        assert_eq!(LowBalance::from_channel_settings(&none), Ok(None));
        assert_eq!(
            LowBalance::from_channel_settings(&json!({"low_balance": null})),
            Ok(None)
        );
        let rule = LowBalance::from_channel_settings(
            &json!({"low_balance": {"threshold_micro": 5_000_000}}),
        )
        .unwrap()
        .unwrap();
        assert!(
            rule.is_low(5_000_000),
            "equal to the threshold counts as low"
        );
        assert!(rule.is_low(-1));
        assert!(!rule.is_low(5_000_001));
        let global = ChannelBalancePolicy {
            low_action: LowBalanceAction::Stop,
            ..Default::default()
        };
        assert_eq!(
            rule.action_under(&global),
            LowBalanceAction::Stop,
            "falls back to the global action"
        );
        let own = LowBalance::from_channel_settings(
            &json!({"low_balance": {"threshold_micro": 0, "action": "notify"}}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            own.action_under(&global),
            LowBalanceAction::Notify,
            "the channel's own action wins"
        );
        for bad in [
            json!({"low_balance": {"threshold_micro": -1}}),
            json!({"low_balance": {"threshold_micro": 1_000_000_000_000_001_i64}}),
            json!({"low_balance": {"threshold_micro": "5"}}),
            json!({"low_balance": {"threshold": 5}}),
            json!({"low_balance": {"threshold_micro": 1, "action": "pause"}}),
            json!({"low_balance": {}}),
            json!({"low_balance": 5}),
        ] {
            assert_eq!(
                LowBalance::from_channel_settings(&bad),
                Err("low_balance"),
                "{bad}"
            );
        }
    }
}
