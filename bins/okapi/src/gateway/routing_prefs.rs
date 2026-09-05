//! 请求级路由偏好（IMPLEMENTATION §11.24，形状对齐 OpenRouter `provider` 对象）。
//!
//! 此前 okapi 的路由控制全在**配置侧**（`api_keys.pool_override` > 分组 `pool_code` >
//! `default`），调用方一点也管不着：既没法说"这次别给我路由到贵渠道"，也没法说
//! "这次失败就直接返回、别换渠道重试"，更没法要求"只走不留存数据的上游"。
//!
//! 取 OpenRouter 的字段形状（存量客户端已经在发 `provider: {...}`），先落三个子集：
//! - `allow_fallbacks`（缺省 true）：false = 首次失败即返回，不做 failover
//! - `max_price.{prompt,completion}`：单价上限（USD / 1M token），超了直接拒而不是先扣后悔
//! - `zdr` / `data_collection: "deny"`：只路由到声明不留存数据的渠道
//!
//! 解析从**原文**做，与入口方言无关（chat / responses / messages 三个入口共用）；
//! 请求体没有 `provider` 键时零解析开销。这些是 okapi 自己的指令，转发前必须剥掉——
//! 上游不认识它，会 400。

use bytes::Bytes;
use serde::Deserialize;

/// 请求体里承载路由偏好的顶层键（OpenRouter 同名）。
pub const PROVIDER_FIELD: &str = "provider";

/// 单价上限（USD / 1M token）。只在调用方显式给了对应键时才判。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MaxPrice {
    pub prompt: Option<f64>,
    pub completion: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoutingPrefs {
    /// 允许失败后改投其它渠道。缺省 true（保持既有行为）。
    pub allow_fallbacks: bool,
    pub max_price: MaxPrice,
    /// 只走声明「不留存」的渠道（`zdr: true` 或 `data_collection: "deny"`）。
    pub zero_retention: bool,
}

impl Default for RoutingPrefs {
    fn default() -> Self {
        Self {
            allow_fallbacks: true,
            max_price: MaxPrice::default(),
            zero_retention: false,
        }
    }
}

impl RoutingPrefs {
    /// 是否有任何一项生效（都没配时热路径可整段跳过）。
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.allow_fallbacks
            && self.max_price.prompt.is_none()
            && self.max_price.completion.is_none()
            && !self.zero_retention
    }
}

#[derive(Deserialize)]
struct ProviderBlock {
    #[serde(default)]
    allow_fallbacks: Option<bool>,
    #[serde(default)]
    max_price: Option<MaxPriceBlock>,
    #[serde(default)]
    zdr: Option<bool>,
    /// OpenRouter 的 `allow` / `deny`；deny = 不接受会留存数据的上游。
    #[serde(default)]
    data_collection: Option<String>,
}

#[derive(Deserialize)]
struct MaxPriceBlock {
    #[serde(default)]
    prompt: Option<f64>,
    #[serde(default)]
    completion: Option<f64>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    provider: Option<ProviderBlock>,
}

/// 从请求原文解析路由偏好。
///
/// 解析不了、或压根没有 `provider` 键 → 缺省偏好（行为与此前完全一致）。
/// **不因为偏好写错就拒请求**：这是路由提示不是计费输入，宁可按缺省走通，
/// 也不要让一个拼错的字段打断主链（与 §3.7 的 fail-safe 取向一致）。
#[must_use]
pub fn parse(body: &Bytes) -> RoutingPrefs {
    // 绝大多数请求没有这个键，先做一次子串预检省掉整体反序列化
    if !contains_provider_key(body) {
        return RoutingPrefs::default();
    }
    let Ok(env) = serde_json::from_slice::<Envelope>(body) else {
        return RoutingPrefs::default();
    };
    let Some(p) = env.provider else {
        return RoutingPrefs::default();
    };
    RoutingPrefs {
        allow_fallbacks: p.allow_fallbacks.unwrap_or(true),
        max_price: p.max_price.map_or_else(MaxPrice::default, |m| MaxPrice {
            // 负数/NaN 视为没写：上限只有正有限值才有意义
            prompt: m.prompt.filter(|v| v.is_finite() && *v >= 0.0),
            completion: m.completion.filter(|v| v.is_finite() && *v >= 0.0),
        }),
        zero_retention: p.zdr.unwrap_or(false) || p.data_collection.as_deref() == Some("deny"),
    }
}

/// 粗筛：请求体里是否出现过 `"provider"` 键名。
fn contains_provider_key(body: &Bytes) -> bool {
    memchr_find(body, b"\"provider\"")
}

fn memchr_find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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

/// 渠道的数据留存声明是否满足「零留存」要求。
///
/// **未声明按不满足处理**：不知道对方留不留，不能当成不留。
#[must_use]
pub fn retention_ok(declared: Option<&str>, require_zero: bool) -> bool {
    !require_zero || declared == Some("none")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(json: &str) -> RoutingPrefs {
        parse(&Bytes::from(json.to_owned()))
    }

    #[test]
    fn absent_or_broken_falls_back_to_defaults() {
        let base = RoutingPrefs::default();
        assert_eq!(prefs(r#"{"model":"m"}"#), base, "没有 provider 键 = 缺省");
        assert_eq!(prefs(r#"{"provider":"openai"}"#), base, "类型不对不该炸");
        assert_eq!(prefs("not json at all"), base);
        assert!(base.is_default());
        assert!(
            base.allow_fallbacks,
            "缺省必须允许 failover（保持既有行为）"
        );
    }

    #[test]
    fn parses_the_three_subsets() {
        let p = prefs(
            r#"{"model":"m","provider":{"allow_fallbacks":false,
                 "max_price":{"prompt":3.0,"completion":9.5},"zdr":true}}"#,
        );
        assert!(!p.allow_fallbacks);
        assert_eq!(p.max_price.prompt, Some(3.0));
        assert_eq!(p.max_price.completion, Some(9.5));
        assert!(p.zero_retention);
        assert!(!p.is_default());
    }

    #[test]
    fn data_collection_deny_is_the_same_ask_as_zdr() {
        assert!(prefs(r#"{"provider":{"data_collection":"deny"}}"#).zero_retention);
        assert!(!prefs(r#"{"provider":{"data_collection":"allow"}}"#).zero_retention);
    }

    #[test]
    fn nonsense_price_ceilings_are_ignored_not_enforced() {
        // 负数/NaN 当没写：把它当成"上限 0"会把所有请求拒光
        let p = prefs(r#"{"provider":{"max_price":{"prompt":-1.0}}}"#);
        assert_eq!(p.max_price.prompt, None);
        assert!(p.is_default());
    }

    #[test]
    fn strip_removes_only_the_directive() {
        let body = Bytes::from(r#"{"model":"m","provider":{"zdr":true},"stream":true}"#.to_owned());
        let out = strip(&body).expect("有 provider 键应返回新体");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(v.get("provider").is_none(), "指令必须剥掉，上游不认识它");
        assert_eq!(v["model"], "m");
        assert_eq!(v["stream"], true);
        assert!(strip(&Bytes::from(r#"{"model":"m"}"#.to_owned())).is_none());
    }

    #[test]
    fn undeclared_retention_fails_closed() {
        assert!(retention_ok(Some("none"), true));
        assert!(!retention_ok(Some("transient"), true));
        assert!(!retention_ok(Some("trains"), true));
        assert!(!retention_ok(None, true), "未声明 ≠ 不留存");
        // 没要求零留存时一律放行
        assert!(retention_ok(None, false));
        assert!(retention_ok(Some("trains"), false));
    }
}
