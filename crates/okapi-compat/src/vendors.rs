//! 内置厂商目录：编译进二进制的静态数据（做法同内置的 LiteLLM 目录——内嵌 JSON、带夹具、
//! `verified_at`），随版本发布、按月对照来源复核。
//!
//! 厂商层只对**明确选了厂商**的渠道生效；按主机识别厂商只做建议（[`suggest_for_host`]），
//! 不会悄悄改变现有渠道的行为。

use crate::resolve::{Layer, LayerInput};
use crate::rule::Rule;
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: String,
    display_name: String,
    group: Group,
    hosts: Vec<String>,
    rules: Vec<Rule>,
}

/// 一个内置厂商。`rules` 已通过与线上相同的校验。
#[derive(Debug, Clone)]
pub struct Vendor {
    pub id: String,
    pub display_name: String,
    pub group: Group,
    pub hosts: Vec<String>,
    rules: Arc<Validated>,
}

impl Vendor {
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
                let verified = doc.verified_at.as_deref().unwrap_or_default();
                let well_formed = verified.len() == 10
                    && verified.bytes().enumerate().all(|(i, b)| match i {
                        4 | 7 => b == b'-',
                        _ => b.is_ascii_digit(),
                    });
                assert!(
                    well_formed,
                    "{}/{} 的 verified_at 不是 YYYY-MM-DD",
                    vendor.id, rule.id
                );
            }
        }
    }

    #[test]
    fn builtin_rules_never_need_elevated_grants() {
        // 内置规则要能被任何管理员选用，不应依赖 billing_critical_ack
        for vendor in all() {
            for rule in vendor.rules().rules() {
                assert!(!rule.billing_critical_ack, "{}/{}", vendor.id, rule.id);
            }
        }
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
