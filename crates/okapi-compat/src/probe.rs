//! 声明式探针（`ProbeSpec`）：厂商数据里描述「怎么向上游查余额」。
//!
//! 引擎只做两件纯事：把规格加渠道基址拼成要请求的 URL 列表（[`BalanceProbe::urls`]），把上游的
//! JSON 响应按规格解析成统一形状（[`BalanceProbe::parse`]）。HTTP 本身——渠道凭证、渠道自己的
//! 代理与额外头、响应体上限、不跟随重定向——由网关执行。
//!
//! 金额是**该货币**的 micro 整数：上游的十进制数字 / 数字字符串经 [`decimal_micro`] 定点解析，
//! 不经浮点；规格里只有整数除法（美分 → 美元），没有表达式。
//!
//! 模型列表探针没有做：各家都是 `GET {base}/models`，没有需要数据化的差异，等出现了再加。

use crate::rule::RuleDoc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// 没有专属探针时的通用口径名（new-api / one-api / OpenAI 老 dashboard）。
pub const DASHBOARD_NAME: &str = "openai_dashboard";

const MAX_REQUESTS: usize = 3;
const MAX_QUERY: usize = 4;
const MAX_MAP: usize = 16;
const MAX_TEXT: usize = 128;
const MAX_DIVIDE: i64 = 1_000_000;
const MAX_DAYS: i64 = 3650;
/// 整数的 micro 放大倍数（6 位小数）。
const MICRO: i64 = 1_000_000;

/// 一家厂商声明的探针。目前只有余额。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<BalanceProbe>,
}

/// 余额探针：依次 GET `requests`，再按下面的字段从各个响应里取数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BalanceProbe {
    pub requests: Vec<Request>,
    /// 剩余额度。不写则由 `total − used` 算出（两者都要有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<Amount>,
    /// 总额；只用于展示（和算剩余额度）。读不出来不影响已声明的 `remaining`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<Amount>,
    /// 已用；同上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<Amount>,
    pub currency: Currency,
    /// 来源与核实日期（内置厂商的探针必填，按月复核）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<RuleDoc>,
}

/// 一次 GET 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// 以 `/` 开头，接在渠道基址后面。
    pub path: String,
    /// 先去掉基址末尾的 `/v1` 再拼：有的余额接口在站点根，不在版本段下（DeepSeek）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub root: bool,
    /// 查询参数，按名字排序拼接。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, QueryValue>,
}

/// 查询参数的值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum QueryValue {
    /// 原样的文本（只允许字母数字与 `_ . -`，不需要转义）。
    Text(String),
    /// 今天（UTC）偏移若干天的日期，`YYYY-MM-DD`。
    Date { days_from_today: i64 },
}

/// 从某个响应里取一个金额。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Amount {
    /// 取自第几个请求的响应（从 0 起）。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub from: usize,
    /// JSON 指针（RFC 6901），指向十进制数字或数字字符串。
    pub at: String,
    /// 整数除数，上游不是该货币的主单位时用：美分换美元写 100。
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub divide: i64,
}

/// 币种：常量，或从响应里取一个字段再查映射表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Currency {
    /// 固定币种（三位大写字母，ISO 4217）。
    Fixed(String),
    Field {
        #[serde(default, skip_serializing_if = "is_zero")]
        from: usize,
        at: String,
        /// 上游写法 → 币种；不在表里（或字段缺失）用 `default`。
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        map: BTreeMap<String, String>,
        default: String,
    },
}

/// 统一的余额形状，金额都是该货币的 micro 整数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Balance {
    pub currency: String,
    pub remaining_micro: i64,
    pub total_micro: Option<i64>,
    pub used_micro: Option<i64>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 skip_serializing_if 只给引用
fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(value: &i64) -> bool {
    *value == 1
}

fn one() -> i64 {
    1
}

impl BalanceProbe {
    /// 构建期自检：内置数据与将来的自定义探针共用。
    pub fn check(&self) -> Result<(), String> {
        let count = self.requests.len();
        if !(1..=MAX_REQUESTS).contains(&count) {
            return Err(format!(
                "requests 要 1 到 {MAX_REQUESTS} 个，现在 {count} 个"
            ));
        }
        for (index, request) in self.requests.iter().enumerate() {
            request
                .check()
                .map_err(|problem| format!("requests[{index}]: {problem}"))?;
        }
        for (name, amount) in [
            ("remaining", &self.remaining),
            ("total", &self.total),
            ("used", &self.used),
        ] {
            if let Some(amount) = amount {
                amount
                    .check(count)
                    .map_err(|problem| format!("{name}: {problem}"))?;
            }
        }
        if self.remaining.is_none() && (self.total.is_none() || self.used.is_none()) {
            return Err("不写 remaining 时 total 与 used 都要写（剩余 = 总额 − 已用）".to_owned());
        }
        self.currency
            .check(count)
            .map_err(|problem| format!("currency: {problem}"))
    }

    /// 要依次请求的 URL。`base` 是渠道基址（含版本段，如 `https://x.com/v1`，末尾斜杠会被去掉）；
    /// `date(n)` 给出「今天偏移 n 天」的 `YYYY-MM-DD`（引擎不碰时钟，由调用方提供）。
    #[must_use]
    pub fn urls(&self, base: &str, date: &dyn Fn(i64) -> String) -> Vec<String> {
        let base = base.trim_end_matches('/');
        self.requests
            .iter()
            .map(|request| request.url(base, date))
            .collect()
    }

    /// 把各个响应（与 [`Self::urls`] 同序）解析成统一形状；形状对不上返回 `None`。
    #[must_use]
    pub fn parse(&self, bodies: &[Value]) -> Option<Balance> {
        let total = self.total.as_ref().and_then(|amount| amount.read(bodies));
        let used = self.used.as_ref().and_then(|amount| amount.read(bodies));
        let remaining = match &self.remaining {
            Some(amount) => amount.read(bodies)?,
            None => total?.checked_sub(used?)?,
        };
        Some(Balance {
            currency: self.currency.resolve(bodies),
            remaining_micro: remaining,
            total_micro: total,
            used_micro: used,
        })
    }
}

impl Request {
    fn check(&self) -> Result<(), String> {
        let path = &self.path;
        let clean = path.len() <= MAX_TEXT
            && path.starts_with('/')
            && path.bytes().all(|b| b.is_ascii_graphic())
            && !path.contains(['?', '#'])
            && path.split('/').all(|segment| segment != "..");
        if !clean {
            return Err(format!(
                "path `{path}` 要以 / 开头，不含空白、? # 与 ..，不超过 {MAX_TEXT} 字节"
            ));
        }
        if self.query.len() > MAX_QUERY {
            return Err(format!("query 最多 {MAX_QUERY} 个参数"));
        }
        for (key, value) in &self.query {
            if !is_word(key, false) || key.bytes().any(|b| b.is_ascii_uppercase()) {
                return Err(format!("query 参数名 `{key}` 只能是小写字母数字与 _ -"));
            }
            match value {
                QueryValue::Text(text) if !is_word(text, true) => {
                    return Err(format!("query `{key}` 的值只能是字母数字与 _ . -"));
                }
                QueryValue::Date { days_from_today } if days_from_today.abs() > MAX_DAYS => {
                    return Err(format!("query `{key}` 的 days_from_today 超过 ±{MAX_DAYS}"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn url(&self, base: &str, date: &dyn Fn(i64) -> String) -> String {
        let base = if self.root {
            base.strip_suffix("/v1").unwrap_or(base)
        } else {
            base
        };
        let mut url = format!("{base}{}", self.path);
        for (index, (key, value)) in self.query.iter().enumerate() {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str(key);
            url.push('=');
            match value {
                QueryValue::Text(text) => url.push_str(text),
                QueryValue::Date { days_from_today } => url.push_str(&date(*days_from_today)),
            }
        }
        url
    }
}

impl Amount {
    fn check(&self, requests: usize) -> Result<(), String> {
        check_source(self.from, &self.at, requests)?;
        if !(1..=MAX_DIVIDE).contains(&self.divide) {
            return Err(format!(
                "divide 要在 1 到 {MAX_DIVIDE}，现在 {}",
                self.divide
            ));
        }
        Ok(())
    }

    fn read(&self, bodies: &[Value]) -> Option<i64> {
        let value = bodies.get(self.from)?.pointer(&self.at)?;
        decimal_micro(value)?.checked_div(self.divide)
    }
}

impl Currency {
    fn check(&self, requests: usize) -> Result<(), String> {
        match self {
            Self::Fixed(code) => check_code(code),
            Self::Field {
                from,
                at,
                map,
                default,
            } => {
                check_source(*from, at, requests)?;
                if map.len() > MAX_MAP {
                    return Err(format!("map 最多 {MAX_MAP} 项"));
                }
                for (raw, code) in map {
                    if raw.is_empty() || raw.len() > 16 {
                        return Err(format!("map 的键 `{raw}` 要 1 到 16 字节"));
                    }
                    check_code(code)?;
                }
                check_code(default)
            }
        }
    }

    fn resolve(&self, bodies: &[Value]) -> String {
        match self {
            Self::Fixed(code) => code.clone(),
            Self::Field {
                from,
                at,
                map,
                default,
            } => bodies
                .get(*from)
                .and_then(|body| body.pointer(at))
                .and_then(Value::as_str)
                .and_then(|raw| map.get(raw))
                .unwrap_or(default)
                .clone(),
        }
    }
}

fn check_source(from: usize, at: &str, requests: usize) -> Result<(), String> {
    if from >= requests {
        return Err(format!("from {from} 超出 requests（共 {requests} 个）"));
    }
    if !at.starts_with('/') || at.len() > MAX_TEXT {
        return Err(format!(
            "at `{at}` 不是 JSON 指针（以 / 开头，不超过 {MAX_TEXT} 字节）"
        ));
    }
    Ok(())
}

fn check_code(code: &str) -> Result<(), String> {
    if code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(format!("币种 `{code}` 不是三位大写字母（ISO 4217）"))
    }
}

/// 非空、不超过 64 字节，只含字母数字与 `_ -`（`dots` 为真时再允许 `.`）。
fn is_word(text: &str, dots: bool) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-') || (dots && b == b'.'))
}

/// JSON 数字或十进制字符串 → micro 整数。负数保留符号；超过 6 位小数截断；
/// 科学计数法、非 ASCII 数字等认不出的形状返回 `None`。不经浮点。
#[must_use]
pub fn decimal_micro(value: &Value) -> Option<i64> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.trim().to_owned(),
        _ => return None,
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.as_str()),
    };
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    // 先确认全是 ASCII 数字再按字节截断，免得切在多字节字符中间
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let frac_part = &frac_part[..frac_part.len().min(6)];
    let mut whole: i64 = 0;
    for b in int_part.bytes() {
        whole = whole.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
    }
    let mut micro = whole.checked_mul(MICRO)?;
    let mut frac: i64 = 0;
    for b in frac_part.bytes() {
        frac = frac * 10 + i64::from(b - b'0');
    }
    for _ in frac_part.len()..6 {
        frac *= 10;
    }
    micro = micro.checked_add(frac)?;
    Some(if negative { -micro } else { micro })
}

/// 通用口径：new-api / one-api 与 OpenAI 老 dashboard 的两个端点，额度（美元）− 用量（美分 ÷ 100）。
/// new-api 自己实现的这两个端点忽略日期；OpenAI 老接口要求 `end_date` 是开区间，所以区间取
/// 「100 天前 → 明天」。没有专属探针的 openai / openai_compat 渠道一律走它。
#[must_use]
pub fn openai_dashboard() -> &'static BalanceProbe {
    static PROBE: OnceLock<BalanceProbe> = OnceLock::new();
    PROBE.get_or_init(|| BalanceProbe {
        requests: vec![
            Request {
                path: "/dashboard/billing/subscription".to_owned(),
                root: false,
                query: BTreeMap::new(),
            },
            Request {
                path: "/dashboard/billing/usage".to_owned(),
                root: false,
                query: BTreeMap::from([
                    (
                        "end_date".to_owned(),
                        QueryValue::Date { days_from_today: 1 },
                    ),
                    (
                        "start_date".to_owned(),
                        QueryValue::Date {
                            days_from_today: -100,
                        },
                    ),
                ]),
            },
        ],
        remaining: None,
        total: Some(Amount {
            from: 0,
            at: "/hard_limit_usd".to_owned(),
            divide: 1,
        }),
        used: Some(Amount {
            from: 1,
            at: "/total_usage".to_owned(),
            divide: 100,
        }),
        currency: Currency::Fixed("USD".to_owned()),
        doc: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixed_date(offset: i64) -> String {
        format!("D{offset:+}")
    }

    fn probe(doc: &Value) -> BalanceProbe {
        serde_json::from_value(doc.clone()).expect("探针 JSON")
    }

    fn simple() -> Value {
        json!({
            "requests": [{"path": "/user/balance"}],
            "remaining": {"at": "/balance"},
            "currency": "CNY"
        })
    }

    #[test]
    fn decimal_parsing_is_exact_and_tolerant() {
        assert_eq!(decimal_micro(&json!("110.00")), Some(110_000_000));
        assert_eq!(decimal_micro(&json!(49.58)), Some(49_580_000));
        assert_eq!(decimal_micro(&json!(100)), Some(100_000_000));
        assert_eq!(decimal_micro(&json!("-3.5")), Some(-3_500_000));
        assert_eq!(decimal_micro(&json!(" 7 ")), Some(7_000_000));
        // 7 位小数截断而非拒绝
        assert_eq!(decimal_micro(&json!("1.23456789")), Some(1_234_567));
        assert_eq!(decimal_micro(&json!(true)), None);
        assert_eq!(decimal_micro(&json!(null)), None);
        assert_eq!(decimal_micro(&json!("abc")), None);
        assert_eq!(decimal_micro(&json!("")), None);
        assert_eq!(decimal_micro(&json!(".")), None);
        assert_eq!(decimal_micro(&json!("-")), None);
        assert_eq!(decimal_micro(&json!("1.2.3")), None);
        assert_eq!(decimal_micro(&json!("+5")), None);
        // 溢出不 panic
        assert_eq!(decimal_micro(&json!("99999999999999999999")), None);
        assert_eq!(decimal_micro(&json!("9223372036854.775808")), None);
    }

    #[test]
    fn decimal_parsing_accepts_a_bare_integer_or_fraction() {
        assert_eq!(decimal_micro(&json!(".5")), Some(500_000));
        assert_eq!(decimal_micro(&json!("5.")), Some(5_000_000));
    }

    #[test]
    fn non_ascii_fraction_is_rejected_before_truncation() {
        assert_eq!(decimal_micro(&json!("1.aaaaaé")), None);
        assert_eq!(decimal_micro(&json!("1.1234567")), Some(1_123_456));
        assert_eq!(
            decimal_micro(&json!("１２")),
            None,
            "全角数字不是 ASCII 数字"
        );
    }

    #[test]
    fn the_generic_dashboard_probe_is_well_formed_and_builds_the_old_urls() {
        let dashboard = openai_dashboard();
        dashboard.check().expect("通用口径自检");
        let urls = dashboard.urls("https://relay.example.com/v1/", &fixed_date);
        assert_eq!(
            urls,
            vec![
                "https://relay.example.com/v1/dashboard/billing/subscription",
                "https://relay.example.com/v1/dashboard/billing/usage?end_date=D+1&start_date=D-100",
            ]
        );
    }

    #[test]
    fn dashboard_balance_is_limit_minus_usage_cents() {
        let subscription = json!({"hard_limit_usd": 100.0, "has_payment_method": true});
        let usage = json!({"total_usage": 1234.5});
        let balance = openai_dashboard().parse(&[subscription, usage]).unwrap();
        assert_eq!(balance.currency, "USD");
        assert_eq!(balance.total_micro, Some(100_000_000));
        assert_eq!(balance.used_micro, Some(12_345_000));
        assert_eq!(balance.remaining_micro, 87_655_000);
        // 少一个响应或字段缺失：形状不认
        assert!(
            openai_dashboard()
                .parse(&[json!({"hard_limit_usd": 1})])
                .is_none()
        );
        assert!(
            openai_dashboard()
                .parse(&[json!({"hard_limit_usd": 1}), json!({"nothing": 1})])
                .is_none()
        );
    }

    #[test]
    fn root_drops_a_trailing_version_segment_only() {
        let rooted = probe(&json!({
            "requests": [{"path": "/user/balance", "root": true}],
            "remaining": {"at": "/b"}, "currency": "USD"
        }));
        let urls = |base: &str| rooted.urls(base, &fixed_date);
        assert_eq!(
            urls("https://api.deepseek.com/v1"),
            ["https://api.deepseek.com/user/balance"]
        );
        assert_eq!(
            urls("https://api.deepseek.com"),
            ["https://api.deepseek.com/user/balance"]
        );
        assert_eq!(
            urls("https://x.com/proxy/v1/"),
            ["https://x.com/proxy/user/balance"],
            "只去末尾的 /v1，且先去掉尾斜杠"
        );
        assert_eq!(
            urls("https://x.com/v1beta"),
            ["https://x.com/v1beta/user/balance"],
            "/v1beta 不是 /v1"
        );
    }

    #[test]
    fn remaining_is_derived_or_declared() {
        let derived = probe(&json!({
            "requests": [{"path": "/a"}],
            "total": {"at": "/t"}, "used": {"at": "/u"}, "currency": "USD"
        }));
        let balance = derived.parse(&[json!({"t": 10.0, "u": "3.5"})]).unwrap();
        assert_eq!(balance.remaining_micro, 6_500_000);
        assert_eq!(balance.total_micro, Some(10_000_000));
        assert_eq!(balance.used_micro, Some(3_500_000));
        // 可以为负（透支）
        let overdrawn = derived.parse(&[json!({"t": 1, "u": 3})]).unwrap();
        assert_eq!(overdrawn.remaining_micro, -2_000_000);
        // 派生需要两边都读得出
        assert!(derived.parse(&[json!({"t": 1})]).is_none());
        assert!(derived.parse(&[json!({"u": 1})]).is_none());
        // 溢出的减法不 panic：9.2e12 − (−9.2e12) 超出 i64 的 micro 范围
        assert!(
            derived
                .parse(&[json!({"t": "9223372036854", "u": "-9223372036854"})])
                .is_none()
        );

        // 声明了 remaining：total / used 只是展示，读不出来不影响
        let declared = probe(&json!({
            "requests": [{"path": "/a"}],
            "remaining": {"at": "/r"}, "total": {"at": "/t"}, "currency": "USD"
        }));
        let balance = declared.parse(&[json!({"r": "2.5"})]).unwrap();
        assert_eq!(balance.remaining_micro, 2_500_000);
        assert_eq!(balance.total_micro, None);
        assert!(
            declared.parse(&[json!({"t": 9})]).is_none(),
            "remaining 读不出就是形状不认"
        );
    }

    #[test]
    fn divide_is_integer_division_on_the_micro_value() {
        let cents = probe(&json!({
            "requests": [{"path": "/a"}],
            "remaining": {"at": "/c", "divide": 100}, "currency": "USD"
        }));
        assert_eq!(
            cents
                .parse(&[json!({"c": 1234.5})])
                .unwrap()
                .remaining_micro,
            12_345_000
        );
        assert_eq!(
            cents.parse(&[json!({"c": "-1"})]).unwrap().remaining_micro,
            -10_000
        );
    }

    #[test]
    fn currency_from_a_field_uses_the_map_then_the_default() {
        let field = probe(&json!({
            "requests": [{"path": "/a"}],
            "remaining": {"at": "/b/0/v"},
            "currency": {"at": "/b/0/cur", "map": {"USD": "USD", "RMB": "CNY"}, "default": "CNY"}
        }));
        let code = |body: &Value| field.parse(std::slice::from_ref(body)).unwrap().currency;
        assert_eq!(code(&json!({"b": [{"v": 1, "cur": "USD"}]})), "USD");
        assert_eq!(code(&json!({"b": [{"v": 1, "cur": "RMB"}]})), "CNY");
        assert_eq!(
            code(&json!({"b": [{"v": 1, "cur": "EUR"}]})),
            "CNY",
            "不在表里用默认"
        );
        assert_eq!(code(&json!({"b": [{"v": 1}]})), "CNY", "字段缺失用默认");
        assert_eq!(
            code(&json!({"b": [{"v": 1, "cur": 7}]})),
            "CNY",
            "不是字符串用默认"
        );
    }

    #[test]
    fn responses_are_addressed_by_request_index() {
        let two = probe(&json!({
            "requests": [{"path": "/a"}, {"path": "/b"}],
            "total": {"at": "/x"}, "used": {"from": 1, "at": "/x"},
            "currency": {"from": 1, "at": "/cur", "default": "USD"}
        }));
        let balance = two
            .parse(&[json!({"x": 9, "cur": "EUR"}), json!({"x": 4})])
            .unwrap();
        assert_eq!(balance.remaining_micro, 5_000_000);
        assert_eq!(
            balance.currency, "USD",
            "币种取自第二个响应，那里没有 cur 字段"
        );
        // 响应数量不足不 panic
        assert!(two.parse(&[json!({"x": 9})]).is_none());
        assert!(two.parse(&[]).is_none());
    }

    #[test]
    fn a_valid_probe_passes_the_check() {
        probe(&simple()).check().unwrap();
    }

    #[test]
    fn unknown_fields_are_rejected_when_reading() {
        let mut doc = simple();
        doc["extra"] = json!(1);
        assert!(serde_json::from_value::<BalanceProbe>(doc).is_err());
        let mut doc = simple();
        doc["requests"][0]["method"] = json!("POST");
        assert!(
            serde_json::from_value::<BalanceProbe>(doc).is_err(),
            "只做 GET"
        );
        let mut doc = simple();
        doc["remaining"]["scale"] = json!(2);
        assert!(serde_json::from_value::<BalanceProbe>(doc).is_err());
        // untagged 的两个枚举也不能漏过未知字段
        let mut doc = simple();
        doc["currency"] = json!({"at": "/c", "default": "CNY", "extra": 1});
        assert!(serde_json::from_value::<BalanceProbe>(doc).is_err());
        let mut doc = simple();
        doc["requests"][0]["query"] = json!({"d": {"days_from_today": 1, "extra": 1}});
        assert!(serde_json::from_value::<BalanceProbe>(doc).is_err());
    }

    #[test]
    fn the_check_names_what_is_wrong() {
        let broken = |patch: &dyn Fn(&mut Value)| {
            let mut doc = simple();
            patch(&mut doc);
            let probe: BalanceProbe = serde_json::from_value(doc).expect("形状本身合法");
            probe.check().expect_err("应当被自检拒绝")
        };
        let problem = broken(&|d| d["requests"] = json!([]));
        assert!(problem.contains("requests"), "{problem}");
        let problem = broken(&|d| {
            d["requests"] = json!([{"path": "/a"}, {"path": "/b"}, {"path": "/c"}, {"path": "/d"}]);
        });
        assert!(problem.contains("requests"), "{problem}");
        for bad in ["a", "/a b", "/a?x=1", "/a#x", "/../x", "/é", ""] {
            let problem = broken(&|d| d["requests"][0]["path"] = json!(bad));
            assert!(problem.contains("path"), "{bad}: {problem}");
        }
        let problem = broken(&|d| d["requests"][0]["query"] = json!({"Bad": "x"}));
        assert!(problem.contains("query"), "{problem}");
        let problem = broken(&|d| d["requests"][0]["query"] = json!({"k": "a b"}));
        assert!(problem.contains("query"), "{problem}");
        let problem =
            broken(&|d| d["requests"][0]["query"] = json!({"k": {"days_from_today": 99999}}));
        assert!(problem.contains("days_from_today"), "{problem}");
        let problem = broken(&|d| {
            d["requests"][0]["query"] = json!({"a": "1", "b": "1", "c": "1", "d": "1", "e": "1"});
        });
        assert!(problem.contains("query"), "{problem}");
        let problem = broken(&|d| d["remaining"]["from"] = json!(1));
        assert!(
            problem.contains("remaining") && problem.contains("from"),
            "{problem}"
        );
        let problem = broken(&|d| d["remaining"]["at"] = json!("balance"));
        assert!(
            problem.contains("remaining") && problem.contains("JSON 指针"),
            "{problem}"
        );
        for divide in [0, -5, 1_000_001] {
            let problem = broken(&|d| d["remaining"]["divide"] = json!(divide));
            assert!(problem.contains("divide"), "{divide}: {problem}");
        }
        let problem = broken(&|d| {
            d.as_object_mut().unwrap().remove("remaining");
            d["total"] = json!({"at": "/t"});
        });
        assert!(
            problem.contains("remaining") && problem.contains("used"),
            "{problem}"
        );
        for code in ["cny", "CN", "CNYY", "¥¥¥", ""] {
            let problem = broken(&|d| d["currency"] = json!(code));
            assert!(problem.contains("币种"), "{code}: {problem}");
        }
        let problem = broken(&|d| {
            d["currency"] = json!({"at": "/c", "map": {"X": "usd"}, "default": "CNY"});
        });
        assert!(problem.contains("币种"), "{problem}");
        let problem = broken(&|d| d["currency"] = json!({"at": "/c", "default": "cny"}));
        assert!(problem.contains("币种"), "{problem}");
        let problem = broken(&|d| d["currency"] = json!({"from": 3, "at": "/c", "default": "CNY"}));
        assert!(
            problem.contains("currency") && problem.contains("from"),
            "{problem}"
        );
    }

    #[test]
    fn serialization_round_trips() {
        for doc in [
            simple(),
            json!({
                "requests": [{"path": "/a", "root": true, "query": {"d": {"days_from_today": -3}, "k": "v"}}],
                "total": {"at": "/t"}, "used": {"at": "/u", "divide": 100},
                "currency": {"at": "/c", "map": {"A": "USD"}, "default": "CNY"},
                "doc": {"why": "来源", "verified_at": "2026-10-25"}
            }),
        ] {
            let parsed = probe(&doc);
            let back = serde_json::to_value(&parsed).unwrap();
            assert_eq!(back, doc);
            assert_eq!(probe(&back), parsed);
        }
    }
}
