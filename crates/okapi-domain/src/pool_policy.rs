//! 渠道池调度策略中心领域模型（纯领域值对象，零 IO、零浮点、零 panic）。
//!
//! 渠道池（`channel_pools`）统一承载 5 维正交策略：
//! - 多跳降级链（`fallback_pools`）
//! - 层内选路与打分（`selection`）
//! - 粘性与 Prompt Cache 亲和（`affinity`）
//! - 容灾深度与首字超时（`failover`）
//! - 成本/供应商/标签硬护栏（`guardrails`）
//! - 灰度比例（`canary_percent`）与条件覆写规则（`rules`：按模型通配、长短上下文阈值、工具需求局部覆写）。

use serde::{Deserialize, Serialize};

pub const VALID_STRATEGIES: &[&str] = &[
    "priority_weighted",
    "least_latency",
    "least_usage",
    "least_concurrency",
    "soonest_reset",
];

pub const VALID_AFFINITY_MODES: &[&str] = &["prefer_warm", "strict", "off"];
pub const VALID_MEMBER_ROLES: &[&str] = &["primary", "backup", "canary"];

/// 单次请求的调度特征（供规则匹配与未来可插拔 `CandidateRanker` / 神经网络打分器使用）。
#[derive(Debug, Clone, Default)]
pub struct RequestFeatures<'a> {
    pub model: &'a str,
    pub prompt_tokens: u32,
    pub has_tools: bool,
    pub has_warm_cache: bool,
    pub session_key: Option<&'a str>,
}

/// 渠道池的声明式策略文档（落 `channel_pools.policy jsonb`；空对象 `{}` 完全向后兼容既有列）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolPolicy {
    /// 多跳降级池链（优先于单跳 `fallback_pool_code`；写入端校验全部存在且无环）。
    #[serde(default)]
    pub fallback_pools: Vec<String>,
    #[serde(default)]
    pub selection: SelectionPolicy,
    #[serde(default)]
    pub affinity: AffinityPolicy,
    #[serde(default)]
    pub failover: FailoverPolicy,
    #[serde(default)]
    pub guardrails: GuardrailPolicy,
    /// 灰度流量百分比（0..=100）：命中桶的请求优先走 `role = 'canary'` 成员，失败再回落 `primary`。
    #[serde(default)]
    pub canary_percent: u8,
    /// 有序条件覆写规则（长短上下文分流、按模型差异化策略等）。
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionPolicy {
    /// 层内选路算法；None = 继承 `channel_pools.routing_strategy`。
    #[serde(default)]
    pub mode: Option<String>,
    /// 是否严格按 priority 分层（缺省 true）。
    #[serde(default)]
    pub respect_priority_layers: Option<bool>,
    /// 加权抽样时是否忽略 `cost_milli` 除数（VIP 质量池可设 true）。
    #[serde(default)]
    pub ignore_cost_in_weight: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AffinityPolicy {
    /// `prefer_warm`（默认）| `strict` | `off`。
    #[serde(default)]
    pub mode: Option<String>,
    /// 池级粘性 TTL 覆写（60..=86400 秒）。
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailoverPolicy {
    /// 单次请求最大跨候选尝试次数（1..=16）。
    #[serde(default)]
    pub max_attempts: Option<u8>,
    /// 瞬态失败时同 key 重试上限（0..=5）。
    #[serde(default)]
    pub max_same_channel_retries: Option<i16>,
    /// 首字输出超时覆写（1..=600 秒）。
    #[serde(default)]
    pub first_output_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardrailPolicy {
    /// 相对成本天花板（千分比，0..=100000）；超过则硬过滤。
    #[serde(default)]
    pub max_cost_milli: Option<i64>,
    /// 仅允许的 provider 列表（空 = 不限）。
    #[serde(default)]
    pub allowed_providers: Vec<String>,
    /// 禁止的 provider 列表。
    #[serde(default)]
    pub denied_providers: Vec<String>,
    /// 候选成员的 `pool_channels.tags` 须命中至少一个标签（空 = 不限）。
    #[serde(default)]
    pub required_tags: Vec<String>,
    /// 候选成员带任一排除标签时硬过滤。
    #[serde(default)]
    pub excluded_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingRule {
    pub name: String,
    #[serde(default)]
    pub when: RuleMatch,
    #[serde(default)]
    pub apply: RuleAction,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleMatch {
    /// 模型通配模式（`*` 通配，如 `claude-*`、`*-mini`；空 = 匹配任意模型）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 估算输入 token 下界（含），用于长上下文分流。
    #[serde(default)]
    pub min_prompt_tokens: Option<u32>,
    /// 估算输入 token 上界（含），用于短上下文分流。
    #[serde(default)]
    pub max_prompt_tokens: Option<u32>,
    /// 是否携带工具（None = 不限）。
    #[serde(default)]
    pub has_tools: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleAction {
    #[serde(default)]
    pub selection: Option<SelectionPolicy>,
    #[serde(default)]
    pub affinity: Option<AffinityPolicy>,
    #[serde(default)]
    pub failover: Option<FailoverPolicy>,
    #[serde(default)]
    pub guardrails: Option<GuardrailPolicy>,
    #[serde(default)]
    pub canary_percent: Option<u8>,
    /// 直接改道至指定池链（例如超长上下文直切专用大上下文池）。
    #[serde(default)]
    pub override_pools: Option<Vec<String>>,
}

/// 解析完默认策略 + 命中条件规则后的最终生效策略（不可变快照，供管线执行与 Dry-Run 解释）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectivePoolPolicy {
    pub primary_pool: String,
    pub pool_chain: Vec<String>,
    pub strategy: String,
    pub respect_priority_layers: bool,
    pub ignore_cost_in_weight: bool,
    pub affinity_mode: String,
    pub affinity_ttl_secs: Option<i64>,
    pub max_attempts: Option<u8>,
    pub max_same_channel_retries: Option<i16>,
    pub first_output_timeout_secs: Option<u64>,
    pub guardrails: GuardrailPolicy,
    pub canary_percent: u8,
    pub matched_rules: Vec<String>,
}

/// 线性两指针 `*` 通配匹配（无回溯、零分配，与模型白名单/别名同一语义）。
#[must_use]
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let (mut star_p, mut star_t) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star_p = Some(pi);
            pi += 1;
            star_t = ti;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn valid_short_token(s: &str, max_len: usize) -> bool {
    let t = s.trim();
    !t.is_empty() && t.len() <= max_len && !t.chars().any(char::is_control)
}

impl SelectionPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(mode) = &self.mode
            && !VALID_STRATEGIES.contains(&mode.as_str())
        {
            return Err("policy.selection.mode");
        }
        Ok(())
    }
}

impl AffinityPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Some(mode) = &self.mode
            && !VALID_AFFINITY_MODES.contains(&mode.as_str())
        {
            return Err("policy.affinity.mode");
        }
        if self.ttl_secs.is_some_and(|s| !(60..=86_400).contains(&s)) {
            return Err("policy.affinity.ttl_secs");
        }
        Ok(())
    }
}

impl FailoverPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_attempts.is_some_and(|n| !(1..=16).contains(&n)) {
            return Err("policy.failover.max_attempts");
        }
        if self
            .max_same_channel_retries
            .is_some_and(|n| !(0..=5).contains(&n))
        {
            return Err("policy.failover.max_same_channel_retries");
        }
        if self
            .first_output_timeout_secs
            .is_some_and(|s| !(1..=600).contains(&s))
        {
            return Err("policy.failover.first_output_timeout_secs");
        }
        Ok(())
    }
}

impl GuardrailPolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .max_cost_milli
            .is_some_and(|c| !(0..=100_000).contains(&c))
        {
            return Err("policy.guardrails.max_cost_milli");
        }
        for (field, list) in [
            (
                "policy.guardrails.allowed_providers",
                &self.allowed_providers,
            ),
            ("policy.guardrails.denied_providers", &self.denied_providers),
            ("policy.guardrails.required_tags", &self.required_tags),
            ("policy.guardrails.excluded_tags", &self.excluded_tags),
        ] {
            if list.len() > 32 || list.iter().any(|item| !valid_short_token(item, 64)) {
                return Err(field);
            }
        }
        Ok(())
    }

    fn merge_from(&mut self, over: &Self) {
        if over.max_cost_milli.is_some() {
            self.max_cost_milli = over.max_cost_milli;
        }
        if !over.allowed_providers.is_empty() {
            self.allowed_providers.clone_from(&over.allowed_providers);
        }
        if !over.denied_providers.is_empty() {
            self.denied_providers.clone_from(&over.denied_providers);
        }
        if !over.required_tags.is_empty() {
            self.required_tags.clone_from(&over.required_tags);
        }
        if !over.excluded_tags.is_empty() {
            self.excluded_tags.clone_from(&over.excluded_tags);
        }
    }
}

impl RuleMatch {
    #[must_use]
    pub fn matches(&self, f: &RequestFeatures<'_>) -> bool {
        if !self.models.is_empty() && !self.models.iter().any(|p| wildcard_match(p, f.model)) {
            return false;
        }
        if self
            .min_prompt_tokens
            .is_some_and(|min| f.prompt_tokens < min)
        {
            return false;
        }
        if self
            .max_prompt_tokens
            .is_some_and(|max| f.prompt_tokens > max)
        {
            return false;
        }
        if self.has_tools.is_some_and(|need| f.has_tools != need) {
            return false;
        }
        true
    }
}

impl PoolPolicy {
    /// 静态配置校验（不触库；池存在性与无环校验由 console 写入端执行）。
    pub fn validate(&self, pool_code: &str) -> Result<(), &'static str> {
        if self.fallback_pools.len() > 8 {
            return Err("policy.fallback_pools");
        }
        for fb in &self.fallback_pools {
            if !valid_short_token(fb, 32) || fb.trim() == pool_code {
                return Err("policy.fallback_pools");
            }
        }
        self.selection.validate()?;
        self.affinity.validate()?;
        self.failover.validate()?;
        self.guardrails.validate()?;
        if self.canary_percent > 100 {
            return Err("policy.canary_percent");
        }
        if self.rules.len() > 32 {
            return Err("policy.rules");
        }
        for rule in &self.rules {
            if !valid_short_token(&rule.name, 64) {
                return Err("policy.rules.name");
            }
            if rule.when.models.len() > 32
                || rule.when.models.iter().any(|m| !valid_short_token(m, 128))
            {
                return Err("policy.rules.when.models");
            }
            if let (Some(min), Some(max)) =
                (rule.when.min_prompt_tokens, rule.when.max_prompt_tokens)
                && min > max
            {
                return Err("policy.rules.when.prompt_tokens");
            }
            if let Some(sel) = &rule.apply.selection {
                sel.validate()?;
            }
            if let Some(aff) = &rule.apply.affinity {
                aff.validate()?;
            }
            if let Some(fo) = &rule.apply.failover {
                fo.validate()?;
            }
            if let Some(gr) = &rule.apply.guardrails {
                gr.validate()?;
            }
            if rule.apply.canary_percent.is_some_and(|p| p > 100) {
                return Err("policy.rules.apply.canary_percent");
            }
            if let Some(pools) = &rule.apply.override_pools
                && (pools.is_empty()
                    || pools.len() > 8
                    || pools.iter().any(|p| !valid_short_token(p, 32)))
            {
                return Err("policy.rules.apply.override_pools");
            }
        }
        Ok(())
    }

    /// 收集策略中引用的全部目标池（用于写入期存在性校验）。
    #[must_use]
    pub fn referenced_pools(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for fb in &self.fallback_pools {
            let trimmed = fb.trim();
            if !trimmed.is_empty() && !out.contains(&trimmed) {
                out.push(trimmed);
            }
        }
        for rule in &self.rules {
            if let Some(pools) = &rule.apply.override_pools {
                for p in pools {
                    let trimmed = p.trim();
                    if !trimmed.is_empty() && !out.contains(&trimmed) {
                        out.push(trimmed);
                    }
                }
            }
        }
        out
    }

    /// 将池级默认策略 + 列级回退字段 + 命中的条件规则合成为本次请求的最终生效策略。
    #[must_use]
    pub fn resolve(
        &self,
        primary_pool: &str,
        column_strategy: &str,
        column_fallback: Option<&str>,
        features: &RequestFeatures<'_>,
    ) -> EffectivePoolPolicy {
        let mut pool_chain = vec![primary_pool.to_owned()];
        if !self.fallback_pools.is_empty() {
            for fb in &self.fallback_pools {
                let t = fb.trim();
                if !t.is_empty() && !pool_chain.iter().any(|p| p == t) {
                    pool_chain.push(t.to_owned());
                }
            }
        } else if let Some(fb) = column_fallback.map(str::trim).filter(|s| !s.is_empty())
            && fb != primary_pool
        {
            pool_chain.push(fb.to_owned());
        }

        let mut eff = EffectivePoolPolicy {
            primary_pool: primary_pool.to_owned(),
            pool_chain,
            strategy: self
                .selection
                .mode
                .clone()
                .unwrap_or_else(|| column_strategy.to_owned()),
            respect_priority_layers: self.selection.respect_priority_layers.unwrap_or(true),
            ignore_cost_in_weight: self.selection.ignore_cost_in_weight.unwrap_or(false),
            affinity_mode: self
                .affinity
                .mode
                .clone()
                .unwrap_or_else(|| "prefer_warm".to_owned()),
            affinity_ttl_secs: self.affinity.ttl_secs,
            max_attempts: self.failover.max_attempts,
            max_same_channel_retries: self.failover.max_same_channel_retries,
            first_output_timeout_secs: self.failover.first_output_timeout_secs,
            guardrails: self.guardrails.clone(),
            canary_percent: self.canary_percent,
            matched_rules: Vec::new(),
        };

        for rule in &self.rules {
            if !rule.when.matches(features) {
                continue;
            }
            eff.matched_rules.push(rule.name.clone());
            if let Some(sel) = &rule.apply.selection {
                if let Some(m) = &sel.mode {
                    eff.strategy.clone_from(m);
                }
                if let Some(v) = sel.respect_priority_layers {
                    eff.respect_priority_layers = v;
                }
                if let Some(v) = sel.ignore_cost_in_weight {
                    eff.ignore_cost_in_weight = v;
                }
            }
            if let Some(aff) = &rule.apply.affinity {
                if let Some(m) = &aff.mode {
                    eff.affinity_mode.clone_from(m);
                }
                if aff.ttl_secs.is_some() {
                    eff.affinity_ttl_secs = aff.ttl_secs;
                }
            }
            if let Some(fo) = &rule.apply.failover {
                if fo.max_attempts.is_some() {
                    eff.max_attempts = fo.max_attempts;
                }
                if fo.max_same_channel_retries.is_some() {
                    eff.max_same_channel_retries = fo.max_same_channel_retries;
                }
                if fo.first_output_timeout_secs.is_some() {
                    eff.first_output_timeout_secs = fo.first_output_timeout_secs;
                }
            }
            if let Some(gr) = &rule.apply.guardrails {
                eff.guardrails.merge_from(gr);
            }
            if let Some(pct) = rule.apply.canary_percent {
                eff.canary_percent = pct;
            }
            if let Some(pools) = &rule.apply.override_pools {
                let mut next = Vec::with_capacity(pools.len());
                for p in pools {
                    let t = p.trim();
                    if !t.is_empty() && !next.iter().any(|x| x == t) {
                        next.push(t.to_owned());
                    }
                }
                if !next.is_empty() {
                    eff.pool_chain = next;
                }
            }
        }
        eff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_policy_preserves_legacy_strategy_and_fallback() {
        let policy = PoolPolicy::default();
        assert!(policy.validate("default").is_ok());
        let eff = policy.resolve(
            "vip",
            "least_latency",
            Some("default"),
            &RequestFeatures {
                model: "claude-sonnet-4-5",
                prompt_tokens: 1000,
                ..Default::default()
            },
        );
        assert_eq!(eff.pool_chain, vec!["vip", "default"]);
        assert_eq!(eff.strategy, "least_latency");
        assert_eq!(eff.affinity_mode, "prefer_warm");
        assert_eq!(eff.matched_rules, Vec::<String>::new());
    }

    #[test]
    fn rules_apply_long_context_and_model_overrides_in_order() {
        let policy = PoolPolicy {
            fallback_pools: vec!["backup-a".into(), "backup-b".into()],
            selection: SelectionPolicy {
                mode: Some("priority_weighted".into()),
                ..Default::default()
            },
            canary_percent: 10,
            rules: vec![
                RoutingRule {
                    name: "claude_sticky".into(),
                    when: RuleMatch {
                        models: vec!["claude-*".into()],
                        ..Default::default()
                    },
                    apply: RuleAction {
                        selection: Some(SelectionPolicy {
                            mode: Some("soonest_reset".into()),
                            ..Default::default()
                        }),
                        affinity: Some(AffinityPolicy {
                            mode: Some("strict".into()),
                            ttl_secs: Some(1800),
                        }),
                        ..Default::default()
                    },
                },
                RoutingRule {
                    name: "long_context".into(),
                    when: RuleMatch {
                        min_prompt_tokens: Some(64_000),
                        ..Default::default()
                    },
                    apply: RuleAction {
                        guardrails: Some(GuardrailPolicy {
                            required_tags: vec!["long_ctx".into()],
                            ..Default::default()
                        }),
                        failover: Some(FailoverPolicy {
                            max_attempts: Some(2),
                            first_output_timeout_secs: Some(90),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                },
            ],
            ..Default::default()
        };
        assert!(policy.validate("main").is_ok());

        let short = policy.resolve(
            "main",
            "priority_weighted",
            None,
            &RequestFeatures {
                model: "claude-sonnet-4-5",
                prompt_tokens: 4_000,
                ..Default::default()
            },
        );
        assert_eq!(short.matched_rules, vec!["claude_sticky"]);
        assert_eq!(short.strategy, "soonest_reset");
        assert_eq!(short.affinity_mode, "strict");
        assert_eq!(short.affinity_ttl_secs, Some(1800));
        assert_eq!(short.guardrails.required_tags, Vec::<String>::new());
        assert_eq!(short.pool_chain, vec!["main", "backup-a", "backup-b"]);

        let long = policy.resolve(
            "main",
            "priority_weighted",
            None,
            &RequestFeatures {
                model: "claude-sonnet-4-5",
                prompt_tokens: 80_000,
                ..Default::default()
            },
        );
        assert_eq!(long.matched_rules, vec!["claude_sticky", "long_context"]);
        assert_eq!(long.guardrails.required_tags, vec!["long_ctx"]);
        assert_eq!(long.max_attempts, Some(2));
        assert_eq!(long.first_output_timeout_secs, Some(90));
    }

    #[test]
    fn validation_rejects_self_fallback_bad_canary_and_inverted_tokens() {
        let mut p = PoolPolicy {
            fallback_pools: vec!["vip".into()],
            ..Default::default()
        };
        assert!(p.validate("vip").is_err());

        p.fallback_pools.clear();
        p.canary_percent = 101;
        assert!(p.validate("vip").is_err());

        p.canary_percent = 10;
        p.rules.push(RoutingRule {
            name: "bad".into(),
            when: RuleMatch {
                min_prompt_tokens: Some(100),
                max_prompt_tokens: Some(50),
                ..Default::default()
            },
            apply: RuleAction::default(),
        });
        assert!(p.validate("vip").is_err());
    }
}
