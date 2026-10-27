//! 请求级路由偏好（IMPLEMENTATION §11.24，形状对齐 OpenRouter `provider` 对象）。
//!
//! 此前 okapi 的路由控制全在**配置侧**（`api_keys.pool_override` > 分组 `pool_code` >
//! `default`），调用方一点也管不着：既没法说"这次别给我路由到贵渠道"，也没法说
//! "这次失败就直接返回、别换渠道重试"。
//!
//! 取 OpenRouter 的字段形状（存量客户端已经在发 `provider: {...}`），先落两个子集：
//! - `allow_fallbacks`（缺省 true）：false = 首次失败即返回，不做 failover
//! - `max_price.{prompt,completion}`：单价上限（USD / 1M token），超了直接拒而不是先扣后悔；
//!   按次计价的模型没有 token 单价可比，给了上限就拒
//!
//! 其余 OpenRouter 键（`order` / `only` / `zdr` / `data_collection` …）不认、不报错，随整个键剥掉。
//!
//! 解析从**原文**做，与入口方言无关（chat / responses / messages 三个入口共用）；
//! 请求体没有 `provider` 键时零解析开销。这些是 okapi 自己的指令，转发前必须剥掉——
//! 上游不认识它，会 400。

use bytes::Bytes;
use serde::Deserialize;

/// 请求体里承载路由偏好的顶层键（OpenRouter 同名）。
pub const PROVIDER_FIELD: &str = "provider";

/// 单价上限（USD / 1M token），micro-USD 定点。只在调用方显式给了对应键时才判。
/// 按请求原文的十进制字面量解析，不经浮点（计费红线：比价也不引入浮点舍入）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaxPrice {
    pub prompt: Option<i64>,
    pub completion: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoutingPrefs {
    /// 允许失败后改投其它渠道。缺省 true（保持既有行为）。
    pub allow_fallbacks: bool,
    pub max_price: MaxPrice,
    /// 请求体顶层确实带了 `provider` 键（含 `null`、含我们不认识的内容）：转发前要把它剥掉。
    /// 解析时已经看过一遍，没带就不必在每次尝试时再把整份请求体解析一次去确认「没有」。
    pub directive_present: bool,
}

impl Default for RoutingPrefs {
    fn default() -> Self {
        Self {
            allow_fallbacks: true,
            max_price: MaxPrice::default(),
            directive_present: false,
        }
    }
}

impl RoutingPrefs {
    /// `"provider": null`：没有偏好，但键在，转发前仍要剥掉（上游不认识它）。
    const fn null_directive() -> Self {
        Self {
            allow_fallbacks: true,
            max_price: MaxPrice {
                prompt: None,
                completion: None,
            },
            directive_present: true,
        }
    }

    /// 是否有任何一项生效（都没配时热路径可整段跳过）。
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.allow_fallbacks
            && self.max_price.prompt.is_none()
            && self.max_price.completion.is_none()
    }
}

#[derive(Deserialize)]
struct ProviderBlock {
    #[serde(default)]
    allow_fallbacks: Option<bool>,
    #[serde(default)]
    max_price: Option<MaxPriceBlock>,
}

#[derive(Deserialize)]
struct MaxPriceBlock {
    #[serde(default)]
    prompt: Option<serde_json::Number>,
    #[serde(default)]
    completion: Option<serde_json::Number>,
}

/// USD / 1M 的上限字面量 → micro-USD。价格本身是整 micro，小数第 6 位之后截掉不改变任何
/// 比较结果；负数与指数写法拒收。
fn cap_micros(number: &serde_json::Number) -> Option<i64> {
    // 开了 arbitrary_precision：Display 原样给出请求里的字面量
    let literal = number.to_string();
    let literal = match literal.split_once('.') {
        Some((int, frac)) if frac.len() > 6 => format!("{int}.{}", &frac[..6]),
        _ => literal,
    };
    okapi_pricing::ratio::parse_scaled_1e6(&literal).ok()
}

/// 请求体顶层 `provider` 键的三种状态：没有这个键 / 键在但值是 `null` / 键在且是对象。
/// 「没有」与「null」都不带偏好，但只有后者转发前还要剥掉，所以要分得开。
#[derive(Default)]
enum Directive {
    #[default]
    Absent,
    Null,
    Block(ProviderBlock),
}

fn directive<'de, D>(deserializer: D) -> Result<Directive, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<ProviderBlock>::deserialize(deserializer)? {
        Some(block) => Directive::Block(block),
        None => Directive::Null,
    })
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default, deserialize_with = "directive")]
    provider: Directive,
}

/// 从请求原文解析路由偏好。
///
/// 压根没有 `provider` 键 → 缺省偏好（行为与此前完全一致）；OpenRouter 的其余键照常忽略。
/// 有 `provider` 但形状不对（不是对象、`allow_fallbacks` 不是布尔、`max_price` 不是合法价格）→ 400
/// `param=provider*`：`max_price` 是调用方给自己设的花费上限，按缺省走通等于悄悄把上限拿掉、
/// 按更贵的渠道扣钱；`allow_fallbacks:false` 写错同理会悄悄允许换渠道。宁可拒，让调用方看见。
pub fn parse(body: &Bytes) -> Result<RoutingPrefs, super::error::AppError> {
    // 绝大多数请求没有这个键，先做一次子串预检省掉整体反序列化
    if !contains_provider_key(body) {
        return Ok(RoutingPrefs::default());
    }
    let env: Envelope = serde_json::from_slice(body)
        .map_err(|_| super::error::AppError::bad_request().with_param("provider"))?;
    match env.provider {
        Directive::Absent => Ok(RoutingPrefs::default()),
        Directive::Null => Ok(RoutingPrefs::null_directive()),
        Directive::Block(block) => from_block(&block),
    }
}

/// 同 [`parse`]，但请求体已经解析成 `Value`（chat 入口手里就有一份）：直接取顶层 `provider`，
/// 不必为它再把整份原文读一遍。没有这个键时零开销——模块文档说的「零解析开销」在这条路径上是真的。
/// 结果与 [`parse`] 逐项相同（见测试里的等价性用例）。
///
/// 有这个键时只把 `provider` 这一小块重新序列化再解析，而不是把 `Value` 直接当 Deserializer 用：
/// 后者会把「转成 f64 再转回来写法不变」的数字重新格式化（`0.000001` 变成 `1e-6`），而价格上限按请求原文的
/// 十进制字面量解析、不收指数写法，同一个请求会因此在两条路上得到不同的结果。序列化会原样写出数字的原始字面量。
pub fn parse_value(body: &serde_json::Value) -> Result<RoutingPrefs, super::error::AppError> {
    match body.get(PROVIDER_FIELD) {
        None => Ok(RoutingPrefs::default()),
        Some(serde_json::Value::Null) => Ok(RoutingPrefs::null_directive()),
        Some(provider) => {
            let rejected = || super::error::AppError::bad_request().with_param("provider");
            let raw = serde_json::to_vec(provider).map_err(|_| rejected())?;
            let block: ProviderBlock = serde_json::from_slice(&raw).map_err(|_| rejected())?;
            from_block(&block)
        }
    }
}

/// `provider` 对象 → 偏好。形状对但价格不合法同样是 400：见 [`parse`] 的说明。
fn from_block(p: &ProviderBlock) -> Result<RoutingPrefs, super::error::AppError> {
    let cap = |number: Option<&serde_json::Number>| {
        number
            .map(|number| {
                cap_micros(number).ok_or_else(|| {
                    super::error::AppError::bad_request().with_param("provider.max_price")
                })
            })
            .transpose()
    };
    let max_price = match &p.max_price {
        Some(m) => MaxPrice {
            prompt: cap(m.prompt.as_ref())?,
            completion: cap(m.completion.as_ref())?,
        },
        None => MaxPrice::default(),
    };
    Ok(RoutingPrefs {
        allow_fallbacks: p.allow_fallbacks.unwrap_or(true),
        max_price,
        directive_present: true,
    })
}

/// 粗筛：请求体里是否出现过 `"provider"` 键名。
/// 每个请求体都要过一遍，用 SIMD 子串查找；有反斜杠时键名可能写成转义形式，交给完整解析。
fn contains_provider_key(body: &Bytes) -> bool {
    memchr::memmem::find(body, b"\"provider\"").is_some() || memchr::memchr(b'\\', body).is_some()
}

/// 剥掉 `provider` 键后的请求体（转发上游前必做）。无该键时返回 None（零拷贝）。
#[must_use]
pub fn strip(body: &Bytes) -> Option<Bytes> {
    if !contains_provider_key(body) {
        return None;
    }
    let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = value.as_object_mut()?;
    obj.remove(PROVIDER_FIELD)?;
    Some(Bytes::from(serde_json::to_vec(&value).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(json: &str) -> RoutingPrefs {
        parse(&Bytes::from(json.to_owned())).unwrap()
    }

    #[test]
    fn absent_prefs_are_defaults_and_malformed_ones_are_refused() {
        let base = RoutingPrefs::default();
        assert_eq!(prefs(r#"{"model":"m"}"#), base, "没有 provider 键 = 缺省");
        assert!(parse(&Bytes::from_static(br#"{"provider":"openai"}"#)).is_err());

        assert!(base.is_default());
        assert!(
            base.allow_fallbacks,
            "缺省必须允许 failover（保持既有行为）"
        );
    }

    #[test]
    fn parses_the_two_subsets() {
        let p = prefs(
            r#"{"model":"m","provider":{"allow_fallbacks":false,
                 "max_price":{"prompt":3.0,"completion":9.5}}}"#,
        );
        assert!(!p.allow_fallbacks);
        assert_eq!(p.max_price.prompt, Some(3_000_000));
        assert_eq!(p.max_price.completion, Some(9_500_000));
        assert!(!p.is_default());
    }

    #[test]
    fn unsupported_openrouter_keys_are_ignored() {
        let p = prefs(r#"{"provider":{"zdr":true,"data_collection":"deny","order":["a"]}}"#);
        assert!(p.is_default());
    }

    #[test]
    fn invalid_price_rejects_the_request() {
        assert!(
            parse(&Bytes::from_static(
                br#"{"provider":{"max_price":{"prompt":-1}}}"#
            ))
            .is_err()
        );
        let p = prefs(r#"{"provi\u0064er":{"allow_fallbacks":false}}"#);
        assert!(!p.allow_fallbacks);
    }

    #[test]
    fn strip_removes_only_the_directive() {
        let body = Bytes::from(
            r#"{"model":"m","provider":{"allow_fallbacks":false},"stream":true}"#.to_owned(),
        );
        let out = strip(&body).expect("有 provider 键应返回新体");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(v.get("provider").is_none(), "指令必须剥掉，上游不认识它");
        assert_eq!(v["model"], "m");
        assert_eq!(v["stream"], true);
        assert!(strip(&Bytes::from(r#"{"model":"m"}"#.to_owned())).is_none());
    }

    /// 转发前只在解析时确认过「键在」才剥：没带键的请求体（含带反斜杠、内容里出现 "provider" 字样、
    /// 嵌套层里有 provider 的）不剥，而剥一遍本来也什么都不会改——跳过不改变任何一次转发的请求体。
    #[test]
    fn the_directive_flag_agrees_with_what_strip_would_do() {
        let without = [
            r#"{"model":"m"}"#,
            r#"{"model":"m","messages":[{"role":"user","content":"line one\nline two"}]}"#,
            r#"{"messages":[{"role":"user","content":"the \"provider\" word"}]}"#,
            r#"{"messages":[{"provider":{"allow_fallbacks":false}}],"model":"m"}"#,
        ];
        for body in without {
            let bytes = Bytes::from(body.to_owned());
            assert!(!parse(&bytes).unwrap().directive_present, "{body}");
            assert!(strip(&bytes).is_none(), "{body}");
        }
        let with = [
            r#"{"model":"m","provider":{"allow_fallbacks":false}}"#,
            r#"{"model":"m","provider":null}"#,
            r#"{"model":"m","provider":{"zdr":true}}"#,
            r#"{"provider":{"zdr":true},"model":"m"}"#,
        ];
        for body in with {
            let bytes = Bytes::from(body.to_owned());
            assert!(parse(&bytes).unwrap().directive_present, "{body}");
            assert!(strip(&bytes).is_some(), "{body}");
        }
    }

    /// `"provider": null` 没有偏好，但键在，仍要在转发前剥掉（上游不认识它）。
    #[test]
    fn a_null_directive_has_no_preferences_but_is_still_stripped() {
        let p = prefs(r#"{"model":"m","provider":null}"#);
        assert!(p.is_default());
        assert!(p.directive_present);
    }

    /// chat 入口用已解析的 `Value` 取偏好（`parse_value`），其余入口仍按原文（`parse`）：两条路必须逐项一致，
    /// 包括被拒绝的形状（同一个状态码、同一个 param），否则同一个请求换个入口会得到不同的待遇。
    #[test]
    fn parsing_the_value_agrees_with_parsing_the_bytes() {
        let bodies = [
            r#"{"model":"m"}"#,
            r#"{"model":"m","messages":[{"role":"user","content":"line one\nline two"}]}"#,
            r#"{"messages":[{"provider":{"allow_fallbacks":false}}],"model":"m"}"#,
            r#"{"model":"m","provider":null}"#,
            r#"{"model":"m","provider":{}}"#,
            r#"{"model":"m","provider":{"zdr":true,"order":["a"]}}"#,
            r#"{"model":"m","provider":{"allow_fallbacks":false}}"#,
            r#"{"model":"m","provider":{"allow_fallbacks":true,"max_price":{"prompt":3.0}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":3.0,"completion":9.5}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":0.0000019999}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":12345678901234}}}"#,
            // 数字写法：f64 往返不变但 ryu 会写成指数的、带多余零的、极小 / 极大的
            r#"{"model":"m","provider":{"max_price":{"prompt":0.000001}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":0.10,"completion":1.0000000}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":100000000000000000000}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":0.1234567891234}}}"#,
            r#"{"model":"m","provider":{"max_price":{"completion":1e-6}}}"#,
            r#"{"model":"m","provider":{"max_price":{"completion":1E3}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":-0.0}}}"#,
            r#"{"provider":{"allow_fallbacks":false},"model":"m"}"#,
            // 被拒绝的形状
            r#"{"model":"m","provider":"openai"}"#,
            r#"{"model":"m","provider":[1,2]}"#,
            r#"{"model":"m","provider":{"allow_fallbacks":"no"}}"#,
            r#"{"model":"m","provider":{"max_price":5}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":-1}}}"#,
            r#"{"model":"m","provider":{"max_price":{"prompt":"3"}}}"#,
            r#"{"model":"m","provider":{"max_price":{"completion":1e3}}}"#,
        ];
        for body in bodies {
            let bytes = Bytes::from(body.to_owned());
            let value: serde_json::Value = serde_json::from_str(body).unwrap();
            match (parse(&bytes), parse_value(&value)) {
                (Ok(from_bytes), Ok(from_value)) => assert_eq!(from_bytes, from_value, "{body}"),
                (Err(from_bytes), Err(from_value)) => {
                    assert_eq!(from_bytes.status, from_value.status, "{body}");
                    assert_eq!(from_bytes.param, from_value.param, "{body}");
                }
                (from_bytes, from_value) => panic!(
                    "{body}: bytes gave {:?}, value gave {:?}",
                    from_bytes.map(|_| ()),
                    from_value.map(|_| ())
                ),
            }
        }
    }
}
