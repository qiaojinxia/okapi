//! 内置厂商目录：编译进二进制的静态数据（做法同内置的 LiteLLM 目录——内嵌 JSON、带夹具、
//! `verified_at`），随版本发布、按月对照来源复核。
//!
//! 厂商层只对**明确选了厂商**的渠道生效；按主机识别厂商只做建议（[`suggest_for_host`]），
//! 不会悄悄改变现有渠道的行为。

use crate::probe::{self, BalanceProbe, Probes};
use crate::resolve::{Layer, LayerInput};
use crate::rule::Rule;
use crate::select::DIALECTS;
use crate::validate::{Validated, validate};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

const DATA: &str = include_str!("../data/vendors.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Cn,
    Western,
    SelfHosted,
}

/// 「协议→基址」表的一行。同一厂商常有多个端点（国内 / 国际、chat / Anthropic 兼容…），
/// 先匹配先用；创建渠道时按渠道协议取第一条补全地址，界面有多条时让管理员选。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    /// 上游方言：`openai.chat` / `anthropic.messages` / …（同选择器的 `dialect`）。
    pub protocol: String,
    /// 含版本段的基址，如 `https://api.deepseek.com/v1`。
    pub base: String,
    /// 给人看的区分（多条端点时）：`国内` / `International`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: String,
    display_name: String,
    group: Group,
    hosts: Vec<String>,
    endpoints: Vec<Endpoint>,
    #[serde(default)]
    probes: Probes,
    rules: Vec<Rule>,
}

/// 一个内置厂商。`rules` 已通过与线上相同的校验，`probes` 已通过自检。
#[derive(Debug, Clone)]
pub struct Vendor {
    pub id: String,
    pub display_name: String,
    pub group: Group,
    pub hosts: Vec<String>,
    pub endpoints: Vec<Endpoint>,
    /// 声明式探针（余额）。
    pub probes: Probes,
    rules: Arc<Validated>,
}

impl Vendor {
    /// 这家厂商专属的余额探针；没有则 `None`（调用方回退到通用口径）。
    #[must_use]
    pub fn balance_probe(&self) -> Option<&BalanceProbe> {
        self.probes.balance.as_ref()
    }

    /// 该协议的第一个端点；没有则 `None`。
    #[must_use]
    pub fn endpoint_for(&self, protocol: &str) -> Option<&Endpoint> {
        self.endpoints.iter().find(|e| e.protocol == protocol)
    }

    /// `channels.vendor` 里存的带命名空间的 id。
    #[must_use]
    pub fn namespaced(&self) -> String {
        namespaced(&self.id)
    }

    #[must_use]
    pub fn rules(&self) -> &Validated {
        &self.rules
    }

    /// 厂商层的解析输入。
    #[must_use]
    pub fn layer(&self) -> LayerInput {
        LayerInput {
            layer: Layer::Vendor,
            label: self.namespaced(),
            rules: Arc::clone(&self.rules),
        }
    }
}

#[must_use]
pub fn namespaced(id: &str) -> String {
    crate::select::normalize_vendor(id)
}

struct Catalog {
    vendors: Vec<Vendor>,
    errors: Vec<String>,
}

fn build() -> Catalog {
    let mut catalog = Catalog {
        vendors: Vec::new(),
        errors: Vec::new(),
    };
    let raws: Vec<Raw> = match serde_json::from_str(DATA) {
        Ok(raws) => raws,
        Err(error) => {
            catalog.errors.push(format!("vendors.json: {error}"));
            return catalog;
        }
    };
    for raw in raws {
        if let Some(problem) = endpoint_problem(&raw) {
            catalog.errors.push(format!("{}: {problem}", raw.id));
            continue;
        }
        if let Some(Err(problem)) = raw.probes.balance.as_ref().map(BalanceProbe::check) {
            catalog
                .errors
                .push(format!("{}: probes.balance: {problem}", raw.id));
            continue;
        }
        let doc = match serde_json::to_value(&raw.rules) {
            Ok(doc) => doc,
            Err(error) => {
                catalog.errors.push(format!("{}: {error}", raw.id));
                continue;
            }
        };
        match validate(&doc) {
            Ok(rules) => catalog.vendors.push(Vendor {
                id: raw.id,
                display_name: raw.display_name,
                group: raw.group,
                hosts: raw.hosts,
                endpoints: raw.endpoints,
                probes: raw.probes,
                rules: Arc::new(rules),
            }),
            Err(issues) => {
                for issue in issues {
                    catalog.errors.push(format!("{}: {issue}", raw.id));
                }
            }
        }
    }
    catalog
}

/// 端点表的自洽检查：至少一条、协议是认识的方言、基址是无空白的 https 地址，且主机在厂商的 `hosts` 里
/// （按主机识别建议与端点补全指向同一批主机，不会各说各话）。
fn endpoint_problem(raw: &Raw) -> Option<String> {
    if raw.endpoints.is_empty() {
        return Some("endpoints 不能为空".to_owned());
    }
    for endpoint in &raw.endpoints {
        if !DIALECTS.contains(&endpoint.protocol.as_str()) {
            return Some(format!("endpoints: 未知协议 `{}`", endpoint.protocol));
        }
        let Some(rest) = endpoint.base.strip_prefix("https://") else {
            return Some(format!("endpoints: `{}` 不是 https 地址", endpoint.base));
        };
        if endpoint.base.chars().any(char::is_whitespace) || endpoint.base.ends_with('/') {
            return Some(format!("endpoints: `{}` 含空白或以 / 结尾", endpoint.base));
        }
        let host = rest
            .split('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !raw.hosts.contains(&host) {
            return Some(format!("endpoints: 主机 `{host}` 不在 hosts 里"));
        }
    }
    None
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(build)
}

#[must_use]
pub fn all() -> &'static [Vendor] {
    &catalog().vendors
}

/// 内置数据本身的问题（解析失败 / 规则校验不过的厂商会被整个丢掉）。CI 断言它为空。
#[must_use]
pub fn catalog_errors() -> &'static [String] {
    &catalog().errors
}

/// 按 id 取内置厂商；`deepseek` 与 `builtin:deepseek` 等价。
#[must_use]
pub fn get(id: &str) -> Option<&'static Vendor> {
    let wanted = namespaced(id);
    all().iter().find(|v| v.namespaced() == wanted)
}

/// 厂商为某协议给出的第一个基址（`vendor` 可带或不带 `builtin:`）；用来在创建渠道时补全地址。
#[must_use]
pub fn endpoint_base(vendor: &str, protocol: &str) -> Option<&'static str> {
    get(vendor)?
        .endpoint_for(protocol)
        .map(|endpoint| endpoint.base.as_str())
}

/// 按 `api_base` 的主机名建议厂商：主机相同或是它的子域。**只做建议，调用方不得据此静默应用。**
#[must_use]
pub fn suggest_for_host(host: &str) -> Option<&'static Vendor> {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    all().iter().find(|vendor| {
        vendor
            .hosts
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
    })
}

/// 余额探针的选择结果。
#[derive(Debug, Clone, Copy)]
pub struct BalanceChoice {
    /// 回给前端与留痕，让管理员知道这个数是按哪家口径算的：厂商 id，或 [`probe::DASHBOARD_NAME`]。
    pub name: &'static str,
    pub probe: &'static BalanceProbe,
}

/// 选余额探针：明确选了的内置厂商优先（`custom:<slug>` 档案由调用方换成它叠加的内置厂商），
/// 其次是按 `api_base` 主机识别出的厂商，再没有专属探针就用通用 dashboard 口径。
///
/// 按主机识别在这里**不是**静默改变行为：这些主机的余额接口只有一种写法，探针只决定「去哪个
/// 地址问、怎么读」，不碰请求规则；选了厂商的渠道则不看主机（中转站换了域名也照样查得到）。
#[must_use]
pub fn balance_probe_for(vendor: Option<&str>, host: &str) -> BalanceChoice {
    [vendor.and_then(get), suggest_for_host(host)]
        .into_iter()
        .flatten()
        .find_map(|vendor| {
            vendor.balance_probe().map(|probe| BalanceChoice {
                name: vendor.id.as_str(),
                probe,
            })
        })
        .unwrap_or_else(|| BalanceChoice {
            name: probe::DASHBOARD_NAME,
            probe: probe::openai_dashboard(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_catalog_is_valid() {
        assert!(catalog_errors().is_empty(), "{:?}", catalog_errors());
        assert!(!all().is_empty());
    }

    #[test]
    fn vendor_ids_and_hosts_are_unique() {
        let mut ids = std::collections::HashSet::new();
        let mut hosts = std::collections::HashSet::new();
        for vendor in all() {
            assert!(ids.insert(&vendor.id), "重复的厂商 id {}", vendor.id);
            for host in &vendor.hosts {
                assert!(hosts.insert(host.as_str()), "主机 {host} 被两个厂商认领");
                assert_eq!(host, &host.to_ascii_lowercase());
            }
        }
    }

    #[test]
    fn every_builtin_rule_carries_its_provenance() {
        for vendor in all() {
            for rule in vendor.rules().rules() {
                let doc = rule
                    .doc
                    .as_ref()
                    .unwrap_or_else(|| panic!("{}/{} 缺 doc", vendor.id, rule.id));
                assert!(
                    doc.why.as_deref().is_some_and(|w| !w.trim().is_empty()),
                    "{}/{} 缺 why",
                    vendor.id,
                    rule.id
                );
                assert!(
                    is_iso_date(doc.verified_at.as_deref().unwrap_or_default()),
                    "{}/{} 的 verified_at 不是 YYYY-MM-DD",
                    vendor.id,
                    rule.id
                );
            }
        }
    }

    fn is_iso_date(text: &str) -> bool {
        text.len() == 10
            && text.bytes().enumerate().all(|(i, b)| match i {
                4 | 7 => b == b'-',
                _ => b.is_ascii_digit(),
            })
    }

    #[test]
    fn every_builtin_probe_carries_its_provenance() {
        let mut with_probe = 0;
        for vendor in all() {
            let Some(probe) = vendor.balance_probe() else {
                continue;
            };
            with_probe += 1;
            let doc = probe
                .doc
                .as_ref()
                .unwrap_or_else(|| panic!("{} 的余额探针缺 doc", vendor.id));
            assert!(
                doc.why.as_deref().is_some_and(|w| !w.trim().is_empty()),
                "{} 的余额探针缺 why",
                vendor.id
            );
            assert!(
                doc.url
                    .as_deref()
                    .is_some_and(|u| u.starts_with("https://")),
                "{} 的余额探针缺来源链接",
                vendor.id
            );
            assert!(
                is_iso_date(doc.verified_at.as_deref().unwrap_or_default()),
                "{} 的余额探针 verified_at 不是 YYYY-MM-DD",
                vendor.id
            );
        }
        assert!(
            with_probe >= 4,
            "DeepSeek / Moonshot / SiliconFlow / OpenRouter 都要有"
        );
    }

    #[test]
    fn the_balance_probe_follows_the_chosen_vendor_then_the_host_then_the_generic_one() {
        let name = |vendor: Option<&str>, host: &str| balance_probe_for(vendor, host).name;
        assert_eq!(name(None, "api.deepseek.com"), "deepseek");
        assert_eq!(name(None, "API.SiliconFlow.cn"), "siliconflow");
        assert_eq!(name(None, "api.siliconflow.com"), "siliconflow");
        assert_eq!(name(None, "openrouter.ai"), "openrouter");
        assert_eq!(name(None, "api.moonshot.ai"), "moonshot");
        assert_eq!(name(None, "relay.example.com"), probe::DASHBOARD_NAME);
        assert_eq!(name(None, ""), probe::DASHBOARD_NAME);
        // 选了厂商就不看主机（中转站换了域名也照样查得到）
        assert_eq!(
            name(Some("builtin:deepseek"), "relay.example.com"),
            "deepseek"
        );
        assert_eq!(name(Some("deepseek"), "127.0.0.1"), "deepseek");
        // 选了没有专属探针的厂商：按主机，再回通用
        assert_eq!(
            name(Some("builtin:zhipu"), "relay.example.com"),
            probe::DASHBOARD_NAME
        );
        assert_eq!(name(Some("builtin:xai"), "api.deepseek.com"), "deepseek");
        // 未知厂商、没换成内置厂商的档案：忽略
        assert_eq!(
            name(Some("builtin:nope"), "relay.example.com"),
            probe::DASHBOARD_NAME
        );
        assert_eq!(
            name(Some("custom:acme"), "relay.example.com"),
            probe::DASHBOARD_NAME
        );
        // 通用口径回的就是那一份
        assert!(std::ptr::eq(
            balance_probe_for(None, "relay.example.com").probe,
            probe::openai_dashboard()
        ));
    }

    #[test]
    fn builtin_rules_never_need_elevated_grants() {
        // 内置规则要能被任何管理员选用：除了改用量事实的 usage_map（校验器恒要求确认），不依赖 billing_critical_ack
        for vendor in all() {
            for rule in vendor.rules().rules() {
                let usage_map = rule
                    .ops
                    .iter()
                    .any(|op| matches!(op, crate::rule::Op::UsageMap { .. }));
                assert_eq!(
                    rule.billing_critical_ack, usage_map,
                    "{}/{}",
                    vendor.id, rule.id
                );
            }
        }
    }

    #[test]
    fn every_vendor_has_an_endpoint_table_that_agrees_with_its_hosts() {
        for vendor in all() {
            assert!(!vendor.endpoints.is_empty(), "{} 没有端点", vendor.id);
            assert!(
                vendor.endpoint_for("openai.chat").is_some(),
                "{} 缺 openai.chat 端点（兼容渠道创建时补地址用）",
                vendor.id
            );
            for endpoint in &vendor.endpoints {
                let host = endpoint
                    .base
                    .strip_prefix("https://")
                    .and_then(|rest| rest.split('/').next())
                    .unwrap_or_default();
                assert!(
                    vendor.hosts.iter().any(|h| h == host),
                    "{} 的端点主机 {host} 不在 hosts 里",
                    vendor.id
                );
                assert_eq!(
                    suggest_for_host(host).map(|v| v.id.as_str()),
                    Some(vendor.id.as_str()),
                    "端点主机应当识别回同一个厂商"
                );
            }
        }
    }

    #[test]
    fn the_first_endpoint_of_a_protocol_fills_the_address() {
        assert_eq!(
            endpoint_base("deepseek", "openai.chat"),
            Some("https://api.deepseek.com/v1")
        );
        assert_eq!(
            endpoint_base("builtin:moonshot", "openai.chat"),
            Some("https://api.moonshot.cn/v1"),
            "多条端点时第一条是缺省"
        );
        assert_eq!(endpoint_base("deepseek", "gemini.generate"), None);
        assert_eq!(endpoint_base("custom:acme", "openai.chat"), None);
    }

    #[test]
    fn lookup_accepts_both_spellings() {
        assert_eq!(get("deepseek").map(|v| v.id.as_str()), Some("deepseek"));
        assert_eq!(
            get("builtin:deepseek").map(|v| v.id.as_str()),
            Some("deepseek")
        );
        assert_eq!(get("custom:deepseek").map(|v| v.id.as_str()), None);
        assert_eq!(get("nope").map(|v| v.id.as_str()), None);
    }

    #[test]
    fn host_suggestions_match_exact_hosts_and_subdomains_only() {
        let id = |host: &str| suggest_for_host(host).map(|v| v.id.as_str());
        assert_eq!(id("api.deepseek.com"), Some("deepseek"));
        assert_eq!(id("API.DeepSeek.com."), Some("deepseek"));
        assert_eq!(id("eu.api.deepseek.com"), Some("deepseek"));
        assert_eq!(id("api.moonshot.ai"), Some("moonshot"));
        assert_eq!(id("open.bigmodel.cn"), Some("zhipu"));
        assert_eq!(id("evilapi.deepseek.com.attacker.net"), None);
        assert_eq!(id("notapi.deepseek.com"), None, "必须在点边界上");
        assert_eq!(id("example.com"), None);
    }
}
