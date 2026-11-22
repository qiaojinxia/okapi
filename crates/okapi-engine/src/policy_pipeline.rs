//! 渠道池调度策略管线（`SchedulingPipeline`）：
//!
//! 统一承接「策略求值 → 灰度一致性分桶 → 成员级模型过滤与池护栏硬过滤 →
//! `(pool_rank, role_tier, priority)` 分层 → 层内选路 → Dry-Run 决策追踪」，
//! 并为后续阶段预留零开销扩展接缝（`CandidateRanker`、`SchedulingOutcome`）。

use crate::candidates::{cached_candidates, queue_with_options};
use crate::scheduler::{CandidateQueue, CandidateSet as _, LayerOptions, Strategy};
use crate::state::AppState;
use okapi_domain::pool_policy::{EffectivePoolPolicy, RequestFeatures, wildcard_match};
use okapi_store::ChannelCandidate;
use okapi_store::pool_policy::StoredPoolPolicy;
use rand::RngExt;
use serde::Serialize;
use std::sync::Arc;

/// 后续阶段（Phase 2 质量自动降级 / Phase 4 轻量在线神经 Bandit）的可插拔候选打分器接缝。
/// 现阶段仅保留抽象边界，不引入任何额外热路径分配或浮点开销。
pub trait CandidateRanker: Send + Sync {
    fn name(&self) -> &'static str;
    /// 返回越大越优先的候选得分（整数标度，便于确定性排序与日志回放）。
    fn score(&self, features: &RequestFeatures<'_>, candidate: &ChannelCandidate) -> i64;
}

/// 后续阶段（Phase 2/4）调度结果反馈事件接缝：请求完成后由网关异步回流，
/// 供质量统计与代价函数（成本 + 时延 + 缓存切换惩罚）在线更新使用。
#[derive(Debug, Clone)]
pub struct SchedulingOutcome {
    pub pool_code: String,
    pub model: String,
    pub channel_id: i64,
    pub channel_key_id: i64,
    pub success: bool,
    pub ttft_ms: u32,
    pub cost_micro: i64,
    pub cache_hit_ratio_milli: u16,
    pub switched_channel: bool,
}

/// Dry-Run 决策追踪里被过滤掉的候选及原因。
#[derive(Debug, Clone, Serialize)]
pub struct RejectedCandidate {
    pub channel_id: i64,
    pub channel_key_id: i64,
    pub channel_name: String,
    pub provider: String,
    pub pool_rank: i32,
    pub pool_role: String,
    pub reason: &'static str,
}

/// Dry-Run 决策追踪里排好序的候选摘要（不泄凭证与内部代理密文）。
#[derive(Debug, Clone, Serialize)]
pub struct SelectedCandidateSummary {
    pub channel_id: i64,
    pub channel_key_id: i64,
    pub channel_name: String,
    pub provider: String,
    pub pool_rank: i32,
    pub pool_role: String,
    pub priority: i32,
    pub weight: i32,
    pub cost_milli: i64,
    pub tags: Vec<String>,
}

/// 策略模拟（Dry-Run）与决策解释结果。
#[derive(Debug, Clone, Serialize)]
pub struct DecisionTrace {
    pub policy_version: i64,
    pub effective_policy: EffectivePoolPolicy,
    pub canary_selected: bool,
    pub ordered_candidates: Vec<SelectedCandidateSummary>,
    pub rejected_candidates: Vec<RejectedCandidate>,
}

/// 灰度流量一致性分桶：
/// - `percent == 0` 恒 false，`percent >= 100` 恒 true；
/// - 携带非空 `session_key` 时做确定性 FNV-1a 64-bit 哈希取模 100，保证同一会话跨轮次稳定落桶，
///   不因反复切桶破坏 Prompt Cache 粘性；
/// - 无 `session_key` 时按请求独立均匀抽样。
#[must_use]
pub fn canary_selected(session_key: Option<&str>, percent: u8) -> bool {
    if percent == 0 {
        return false;
    }
    if percent >= 100 {
        return true;
    }
    if let Some(key) = session_key.map(str::trim).filter(|s| !s.is_empty()) {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in key.as_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        (hash % 100) < u64::from(percent)
    } else {
        rand::rng().random_range(0..100u8) < percent
    }
}

/// 单条候选的硬护栏与成员约束检查；不通过返回结构化拒绝原因码。
pub fn check_candidate(
    c: &ChannelCandidate,
    model: &str,
    policy: &EffectivePoolPolicy,
    canary_active: bool,
) -> Result<(), &'static str> {
    if c.pool_role == "canary" && !canary_active {
        return Err("canary_not_selected");
    }
    if let Some(patterns) = &c.pool_model_filter
        && !patterns.is_empty()
        && !patterns.iter().any(|p| wildcard_match(p, model))
    {
        return Err("member_model_filter");
    }
    let gr = &policy.guardrails;
    if gr.max_cost_milli.is_some_and(|max| c.cost_milli > max) {
        return Err("guardrail_max_cost_milli");
    }
    if !gr.allowed_providers.is_empty() && !gr.allowed_providers.iter().any(|p| p == &c.provider) {
        return Err("guardrail_allowed_providers");
    }
    if gr.denied_providers.iter().any(|p| p == &c.provider) {
        return Err("guardrail_denied_providers");
    }
    if !gr.required_tags.is_empty()
        && !gr
            .required_tags
            .iter()
            .any(|req| c.pool_tags.iter().any(|t| t == req))
    {
        return Err("guardrail_required_tags");
    }
    if gr
        .excluded_tags
        .iter()
        .any(|ex| c.pool_tags.iter().any(|t| t == ex))
    {
        return Err("guardrail_excluded_tags");
    }
    Ok(())
}

/// 过滤候选集合：零拒绝快路径直接复用原 `Arc<Vec<_>>`（零拷贝），仅在存在被筛项时收缩，
/// 让后续 `least_latency` 中位数与 Redis 批量读不把被拒候选算进去。
pub fn filter_raw(
    raw: Arc<Vec<ChannelCandidate>>,
    model: &str,
    policy: &EffectivePoolPolicy,
    canary_active: bool,
    mut rejected_out: Option<&mut Vec<RejectedCandidate>>,
) -> Arc<Vec<ChannelCandidate>> {
    let mut any_rejected = false;
    for c in raw.iter() {
        if let Err(reason) = check_candidate(c, model, policy, canary_active) {
            any_rejected = true;
            if let Some(out) = rejected_out.as_deref_mut() {
                out.push(RejectedCandidate {
                    channel_id: c.channel_id,
                    channel_key_id: c.channel_key_id,
                    channel_name: c.channel_name.clone(),
                    provider: c.provider.clone(),
                    pool_rank: c.pool_rank,
                    pool_role: c.pool_role.clone(),
                    reason,
                });
            }
        }
    }
    if !any_rejected {
        return raw;
    }
    let kept: Vec<ChannelCandidate> = raw
        .iter()
        .filter(|c| check_candidate(c, model, policy, canary_active).is_ok())
        .cloned()
        .collect();
    Arc::new(kept)
}

/// 根据生效策略构建过滤并分层排好序的 `CandidateQueue`，同时返回是否命中灰度桶。
pub async fn build_queue(
    state: &AppState,
    raw: Arc<Vec<ChannelCandidate>>,
    model: &str,
    policy: &EffectivePoolPolicy,
    session_key: Option<&str>,
) -> (CandidateQueue, bool) {
    let canary_active = canary_selected(session_key, policy.canary_percent);
    let filtered = filter_raw(raw, model, policy, canary_active, None);
    let strategy = Strategy::parse(Some(&policy.strategy));
    let opts = LayerOptions {
        respect_priority_layers: policy.respect_priority_layers,
        ignore_cost_in_weight: policy.ignore_cost_in_weight,
        canary_active,
    };
    let queue = queue_with_options(state, filtered, strategy, opts).await;
    (queue, canary_active)
}

/// 策略 Dry-Run 模拟：复用与生产热路径完全一致的规则求值、灰度分桶、硬过滤与分层选路，
/// 返回完整决策追踪，供控制台验证已保存策略或未保存草稿。
pub async fn simulate(
    state: &AppState,
    stored: &StoredPoolPolicy,
    features: &RequestFeatures<'_>,
) -> Result<DecisionTrace, okapi_store::StoreError> {
    let effective_policy = stored.resolve(features);
    let chain: Vec<&str> = effective_policy
        .pool_chain
        .iter()
        .map(String::as_str)
        .collect();
    let raw = cached_candidates(state, features.model, &chain, true).await?;
    let canary_active = canary_selected(features.session_key, effective_policy.canary_percent);
    let mut rejected_candidates = Vec::new();
    let filtered = filter_raw(
        raw,
        features.model,
        &effective_policy,
        canary_active,
        Some(&mut rejected_candidates),
    );
    let strategy = Strategy::parse(Some(&effective_policy.strategy));
    let opts = LayerOptions {
        respect_priority_layers: effective_policy.respect_priority_layers,
        ignore_cost_in_weight: effective_policy.ignore_cost_in_weight,
        canary_active,
    };
    let queue = queue_with_options(state, filtered, strategy, opts).await;
    let ordered_candidates = queue
        .iter()
        .map(|c| SelectedCandidateSummary {
            channel_id: c.channel_id,
            channel_key_id: c.channel_key_id,
            channel_name: c.channel_name.clone(),
            provider: c.provider.clone(),
            pool_rank: c.pool_rank,
            pool_role: c.pool_role.clone(),
            priority: c.priority,
            weight: c.weight,
            cost_milli: c.cost_milli,
            tags: c.pool_tags.clone(),
        })
        .collect();
    Ok(DecisionTrace {
        policy_version: stored.policy_version,
        effective_policy,
        canary_selected: canary_active,
        ordered_candidates,
        rejected_candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::tests::cand;
    use okapi_domain::pool_policy::PoolPolicy;

    fn make_cand(key_id: i64, role: &str, provider: &str, cost_milli: i64) -> ChannelCandidate {
        let mut c = cand(10, 10, cost_milli);
        c.channel_id = key_id;
        c.channel_key_id = key_id;
        c.pool_role = role.to_owned();
        c.provider = provider.to_owned();
        c
    }

    #[test]
    fn canary_bucketing_is_deterministic_per_session() {
        assert!(!canary_selected(Some("sess-1"), 0));
        assert!(canary_selected(Some("sess-1"), 100));
        let first = canary_selected(Some("stable-session-abc"), 37);
        for _ in 0..20 {
            assert_eq!(canary_selected(Some("stable-session-abc"), 37), first);
        }
    }

    #[test]
    fn guardrails_and_member_model_filter_reject_with_exact_reasons() {
        let policy: PoolPolicy = serde_json::from_value(serde_json::json!({
            "guardrails": {
                "max_cost_milli": 1500,
                "allowed_providers": ["anthropic", "openai"],
                "denied_providers": ["openai"],
                "required_tags": ["long_ctx"],
                "excluded_tags": ["legacy"]
            }
        }))
        .unwrap();
        let eff = policy.resolve(
            "main",
            "priority_weighted",
            None,
            &RequestFeatures {
                model: "claude-sonnet-4-5",
                prompt_tokens: 1000,
                ..Default::default()
            },
        );

        let mut canary_c = make_cand(1, "canary", "anthropic", 1000);
        canary_c.pool_tags = vec!["long_ctx".into()];
        assert_eq!(
            check_candidate(&canary_c, "claude-sonnet-4-5", &eff, false),
            Err("canary_not_selected")
        );
        assert!(check_candidate(&canary_c, "claude-sonnet-4-5", &eff, true).is_ok());

        let mut model_filtered = make_cand(2, "primary", "anthropic", 1000);
        model_filtered.pool_tags = vec!["long_ctx".into()];
        model_filtered.pool_model_filter = Some(vec!["gpt-*".into()]);
        assert_eq!(
            check_candidate(&model_filtered, "claude-sonnet-4-5", &eff, false),
            Err("member_model_filter")
        );

        let mut pricey = make_cand(3, "primary", "anthropic", 2000);
        pricey.pool_tags = vec!["long_ctx".into()];
        assert_eq!(
            check_candidate(&pricey, "claude-sonnet-4-5", &eff, false),
            Err("guardrail_max_cost_milli")
        );

        let mut wrong_prov = make_cand(4, "primary", "gemini", 1000);
        wrong_prov.pool_tags = vec!["long_ctx".into()];
        assert_eq!(
            check_candidate(&wrong_prov, "claude-sonnet-4-5", &eff, false),
            Err("guardrail_allowed_providers")
        );

        let mut denied_prov = make_cand(5, "primary", "openai", 1000);
        denied_prov.pool_tags = vec!["long_ctx".into()];
        assert_eq!(
            check_candidate(&denied_prov, "claude-sonnet-4-5", &eff, false),
            Err("guardrail_denied_providers")
        );

        let missing_tag = make_cand(6, "primary", "anthropic", 1000);
        assert_eq!(
            check_candidate(&missing_tag, "claude-sonnet-4-5", &eff, false),
            Err("guardrail_required_tags")
        );

        let mut excluded = make_cand(7, "primary", "anthropic", 1000);
        excluded.pool_tags = vec!["long_ctx".into(), "legacy".into()];
        assert_eq!(
            check_candidate(&excluded, "claude-sonnet-4-5", &eff, false),
            Err("guardrail_excluded_tags")
        );
    }

    #[test]
    fn role_tier_orders_canary_before_primary_before_backup() {
        let mut backup = make_cand(1, "backup", "anthropic", 1000);
        backup.priority = 100; // 即使 priority 更高，backup 也排在 primary 后面
        let mut primary = make_cand(2, "primary", "anthropic", 1000);
        primary.priority = 10;
        let mut canary = make_cand(3, "canary", "anthropic", 1000);
        canary.priority = 1;

        let raw = Arc::new(vec![backup, primary, canary]);
        let q = CandidateQueue::weighted_with(
            Arc::clone(&raw),
            LayerOptions {
                respect_priority_layers: true,
                ignore_cost_in_weight: false,
                canary_active: true,
            },
        );
        let ids: Vec<i64> = q.iter().map(|c| c.channel_key_id).collect();
        assert_eq!(ids, vec![3, 2, 1], "canary -> primary -> backup");
    }
}
