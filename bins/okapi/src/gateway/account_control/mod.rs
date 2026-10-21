//! Shared channel admission, independent of ingress, OAuth and provider extensions.
pub mod policy;
pub mod quota;

use super::{diagnostics, scheduler::CandidateSet, state::AppState};
use okapi_providers::UpstreamError;
use okapi_store::ChannelCandidate;
use policy::Policy;
use std::{future::Future, sync::Arc};

pub async fn policy(state: &AppState, channel: i64) -> Result<Arc<Policy>, UpstreamError> {
    if let Some(value) = state.channel_control_cache.get(&channel).await {
        return Ok(value);
    }
    let settings: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT settings FROM channels WHERE id=$1 AND deleted_at IS NULL AND status=1",
    )
    .bind(channel)
    .fetch_optional(&state.pg)
    .await
    .map_err(|_| blocked("unavailable"))?;
    let settings = settings.ok_or_else(|| blocked("channel_disabled"))?;
    let policy = Arc::new(Policy::parse(&settings).map_err(|_| blocked("invalid_policy"))?);
    state
        .channel_control_cache
        .insert(channel, policy.clone())
        .await;
    Ok(policy)
}

/// 诊断里最多记这么多条被排除的 key。
const MAX_EXCLUDED: usize = 24;

/// `blocked()` 的原因（去掉 `channel_control:` 前缀）。
fn reason(error: &UpstreamError) -> String {
    match error {
        UpstreamError::Build(text) => text
            .strip_prefix("channel_control:")
            .unwrap_or(text)
            .to_owned(),
        other => other.error_code().to_owned(),
    }
}

pub fn blocked(reason: &str) -> UpstreamError {
    UpstreamError::Build(format!("channel_control:{reason}"))
}

pub fn attempt_error(error: &UpstreamError) -> super::error::AppError {
    super::error::AppError::new(
        if error.error_code() == okapi_api::codes::NO_AVAILABLE_CHANNEL {
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        } else {
            axum::http::StatusCode::BAD_GATEWAY
        },
        error.error_code(),
    )
}

fn quota_blocked(
    policy: &Policy,
    quota: &okapi_providers::account::quota::Snapshot,
    now: i64,
) -> bool {
    quota_block(policy, quota, now).is_some()
}

/// 账号为什么被上游额度挡下：写进请求诊断的结构化原因（哪条规则、哪个窗口、用了多少、
/// 上限多少、何时重置），不只是一个 `upstream_quota`。用尽 / 低于停用线时的 `resets_at` 取最近一次
/// 窗口重置，是「最早可能恢复」的时刻。
fn quota_block(
    policy: &Policy,
    quota: &okapi_providers::account::quota::Snapshot,
    now: i64,
) -> Option<serde_json::Value> {
    if let Some(headroom) = quota.headroom(now) {
        if headroom == 0 {
            return Some(serde_json::json!({
                "rule": "exhausted",
                "headroom_percent": 0,
                "resets_at": quota.next_reset(now),
            }));
        }
        if policy.quota_aware && headroom <= 100 - policy.quota_threshold_pct {
            return Some(serde_json::json!({
                "rule": "threshold",
                "headroom_percent": headroom,
                "threshold_percent": policy.quota_threshold_pct,
                "window": quota.threshold_window,
                "resets_at": quota.next_reset(now),
            }));
        }
    }
    if !quota.fresh(now) {
        return None;
    }
    quota.windows.iter().find_map(|window| {
        let cap = window
            .window_secs
            .and_then(|seconds| policy.quota_limits.get(&seconds))?;
        (window.resets_at.is_none_or(|at| at > now) && window.used_percent >= *cap).then(|| {
            serde_json::json!({
                "rule": "window_limit",
                "window": window.name,
                "window_secs": window.window_secs,
                "used_percent": window.used_percent,
                "limit_percent": cap,
                "resets_at": window.resets_at,
            })
        })
    })
}

/// 额度观测怎样参与同一层里的先后（排除在任何情况下都生效）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaOrder {
    /// 缺省策略：按余量分三档，档内保持池策略排好的顺序。
    HeadroomTiers,
    /// 按使用量 / 并发 / 时延的池：只筛不排，层内先后全由池策略决定。
    Keep,
    /// `soonest_reset` 池：层内按最近一次窗口重置升序，先用快要重置的额度。
    SoonestReset,
}

/// 余量分档：≥50 高、20–49 中、<20 低；没有观测按中档——既不冒充空闲账号，也不压到最后。
/// 只分三档：余量每 5 分钟才观测一次，按精确余量排，两次观测之间的新会话会全涌向同一个账号。
const fn headroom_tier(headroom: Option<u8>) -> u8 {
    match headroom {
        Some(value) if value >= 50 => 2,
        Some(value) if value >= 20 => 1,
        Some(_) => 0,
        None => 1,
    }
}

async fn token_blocked(
    state: &AppState,
    channel: i64,
    policy: &Policy,
) -> Result<bool, UpstreamError> {
    let Some(limit) = &policy.local_tokens else {
        return Ok(false);
    };
    let usage = okapi_store::channel_usage::token_snapshot(&state.pg, channel, limit.period.name())
        .await
        .map_err(|_| blocked("token_usage_unavailable"))?;
    Ok(usage.tokens >= limit.cap)
}

/// Pre-filter before sticky promotion; blocked account affinity never overrides admission.
/// 被挡下的 key 与原因记进请求诊断（`excluded`，只在管理端日志里）：选路被筛空时，
/// 「没有可用渠道」到底是停用、本地上限还是上游额度，看日志就知道；额度类原因带窗口、
/// 用量、上限与重置时刻。留下的候选按 `order` 在层内调整先后（见 [`QuotaOrder`]）。
/// 返回被额度挡下的候选里最早的重置时刻（Unix 秒），候选全空时据此告诉客户端何时再试。
#[allow(clippy::too_many_lines)] // 逐 key 准入与诊断记录共用同一份缓存，拆开要来回传四张表
pub async fn retain_available(
    state: &AppState,
    candidates: &mut impl CandidateSet,
    order: QuotaOrder,
) -> Option<i64> {
    let mut excluded: Vec<serde_json::Value> = Vec::new();
    let mut earliest_reset: Option<i64> = None;
    let mut exclude =
        |candidate: &ChannelCandidate, reason: &str, detail: Option<serde_json::Value>| {
            if excluded.len() < MAX_EXCLUDED {
                let mut entry = serde_json::json!({
                    "channel_id": candidate.channel_id,
                    "channel_key_id": candidate.channel_key_id,
                    "reason": reason,
                });
                if let Some(serde_json::Value::Object(detail)) = detail {
                    entry
                        .as_object_mut()
                        .into_iter()
                        .for_each(|e| e.extend(detail.clone()));
                }
                excluded.push(entry);
            }
        };
    let mut policies: std::collections::HashMap<i64, Result<Arc<Policy>, String>> =
        std::collections::HashMap::new();
    let mut available = std::collections::HashSet::with_capacity(candidates.len());
    let mut headroom = std::collections::HashMap::new();
    let mut resets = std::collections::HashMap::new();
    let mut token_limits = std::collections::HashMap::new();
    for candidate in candidates.iter() {
        let policy = match policies.entry(candidate.channel_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let value = policy(state, candidate.channel_id)
                    .await
                    .map_err(|error| reason(&error));
                e.insert(value).clone()
            }
        };
        let policy = match policy {
            Ok(policy) => policy,
            Err(reason) => {
                exclude(candidate, &reason, None);
                continue;
            }
        };
        let is_token_blocked = match token_limits.entry(candidate.channel_id) {
            std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
            std::collections::hash_map::Entry::Vacant(entry) => *entry.insert(
                token_blocked(state, candidate.channel_id, &policy)
                    .await
                    .unwrap_or(true),
            ),
        };
        if is_token_blocked {
            exclude(candidate, "local_tokens", None);
            continue;
        }
        if let Some(quota) = quota::read_for(state, candidate).await {
            let now = chrono::Utc::now().timestamp();
            if let Some(detail) = quota_block(&policy, &quota, now) {
                if let Some(at) = detail["resets_at"].as_i64() {
                    earliest_reset = Some(earliest_reset.map_or(at, |seen| seen.min(at)));
                }
                exclude(candidate, "upstream_quota", Some(detail));
                continue;
            }
            if let Some(remaining) = quota.headroom(now) {
                headroom.insert(candidate.channel_key_id, remaining);
            }
            if let Some(at) = quota.next_reset(now) {
                resets.insert(candidate.channel_key_id, at);
            }
        }
        available.insert(candidate.channel_key_id);
    }
    candidates.retain(|c| available.contains(&c.channel_key_id));
    if !excluded.is_empty()
        && let Some(trace) = diagnostics::Trace::current()
    {
        trace.set("excluded", serde_json::Value::Array(excluded));
    }
    order_by_quota(candidates, &headroom, &resets, order);
    earliest_reset
}

/// 层内按额度观测调整先后。稳定排序：键相同的候选保持池策略排好的先后
/// （加权随机 / 时延 / 用量 / 并发）；层（池序、priority）本身从不被推翻。
fn order_by_quota(
    candidates: &mut impl CandidateSet,
    headroom: &std::collections::HashMap<i64, u8>,
    resets: &std::collections::HashMap<i64, i64>,
    order: QuotaOrder,
) {
    match order {
        QuotaOrder::Keep => {}
        QuotaOrder::HeadroomTiers => candidates.sort_by_key(|c| {
            (
                c.pool_rank,
                std::cmp::Reverse(c.priority),
                std::cmp::Reverse(headroom_tier(headroom.get(&c.channel_key_id).copied())),
            )
        }),
        QuotaOrder::SoonestReset => {
            // 没有观测的 key 按本层已知重置时刻的中位数排：不抢在确实快重置的前面，也不被饿死
            let mut layers: std::collections::HashMap<(i32, i32), Vec<i64>> =
                std::collections::HashMap::new();
            for c in candidates.iter() {
                if let Some(at) = resets.get(&c.channel_key_id) {
                    layers
                        .entry((c.pool_rank, c.priority))
                        .or_default()
                        .push(*at);
                }
            }
            let medians: std::collections::HashMap<_, _> = layers
                .into_iter()
                .map(|(layer, mut values)| {
                    values.sort_unstable();
                    (layer, values[values.len() / 2])
                })
                .collect();
            candidates.sort_by_key(|c| {
                let at = resets
                    .get(&c.channel_key_id)
                    .copied()
                    .or_else(|| medians.get(&(c.pool_rank, c.priority)).copied());
                (
                    c.pool_rank,
                    std::cmp::Reverse(c.priority),
                    at.unwrap_or(i64::MAX),
                )
            });
        }
    }
}

/// Every wire attempt, including same-key retries, passes the shared upstream quota gate.
/// A policy denial is never treated as a credential failure.
pub fn execute<'a, T: okapi_providers::response_lifetime::ResponseLifetime + 'a>(
    state: &'a AppState,
    candidate: &'a ChannelCandidate,
    model: &'a str,
    endpoint: &'a str,
    future: impl Future<Output = Result<T, UpstreamError>> + 'a,
) -> impl Future<Output = Result<T, UpstreamError>> + 'a {
    // Keep the shared gate's PG/Redis futures off every ingress task's stack.
    Box::pin(async move {
        admit(state, candidate.channel_id, Some(candidate.channel_key_id)).await?;
        let mut permit =
            super::sched_redis::channel_permit::ChannelPermit::acquire(&state.sched, candidate)
                .await?
                .ok_or_else(|| blocked("concurrency"))?;
        let result = tokio::select! {
            result = diagnostics::upstream(candidate, model, endpoint, future) => result,
            () = permit.expired() => Err(blocked("concurrency_lease_lost")),
        }?;
        Ok(result.with_guard(permit))
    })
}

pub async fn admit(state: &AppState, channel: i64, key: Option<i64>) -> Result<(), UpstreamError> {
    let policy = policy(state, channel).await?;
    if token_blocked(state, channel, &policy).await? {
        return Err(blocked("local_tokens"));
    }
    if let Some(key) = key
        && let Some(quota) = quota::read(state, key).await
        && quota_blocked(&policy, &quota, chrono::Utc::now().timestamp())
    {
        return Err(blocked("upstream_quota"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn independent_window_caps_follow_actual_duration_and_reset() {
        for provider in ["anthropic_max", "codex"] {
            let data = if provider == "anthropic_max" {
                json!({"five_hour":{"utilization":20,"resets_at":"1970-01-01T00:30:00Z"},"seven_day":{"utilization":80,"resets_at":"1970-01-01T00:40:00Z"}})
            } else {
                // Reversed primary/secondary ordering cannot swap the limits.
                json!({"rate_limit":{"primary_window":{"used_percent":80,"limit_window_seconds":604_800,"reset_at":2400},"secondary_window":{"used_percent":20,"limit_window_seconds":18000,"reset_at":1800}}})
            };
            let quota = okapi_providers::oauth::quota::parse(provider, &data, 1000).unwrap();
            let policy = Policy::parse(
                &json!({"account_control":{"quota_limits":{"18000":90,"604800":80}}}),
            )
            .unwrap();
            assert!(quota_blocked(&policy, &quota, 1000));
            let policy = Policy::parse(
                &json!({"account_control":{"quota_limits":{"18000":20,"604800":90}}}),
            )
            .unwrap();
            assert!(quota_blocked(&policy, &quota, 1000));
            // 两次轮询之间（观测后 200 秒）阈值仍然生效；过了有效期才放行
            assert!(quota_blocked(&policy, &quota, 1200));
            assert!(!quota_blocked(&policy, &quota, 1601));
            let policy = Policy::parse(
                &json!({"account_control":{"quota_limits":{"18000":21,"604800":81}}}),
            )
            .unwrap();
            assert!(!quota_blocked(&policy, &quota, 1000));
            let mut fresh = quota.clone();
            fresh.observed_at = 1900;
            let policy =
                Policy::parse(&json!({"account_control":{"quota_limits":{"18000":20}}})).unwrap();
            assert!(!quota_blocked(&policy, &fresh, 1900));
        }
    }

    fn quota(windows: &serde_json::Value) -> okapi_providers::account::quota::Snapshot {
        serde_json::from_value(json!({"observed_at": 1000, "allowed": null, "windows": windows}))
            .unwrap()
    }

    /// 诊断里的额度原因说清是哪条规则、哪个窗口、用了多少、上限多少、何时重置。
    #[test]
    fn quota_block_explains_the_rule_window_and_reset() {
        let windows = json!([
            {"name": "five_hour", "used_percent": 20, "resets_at": 1800, "window_secs": 18000},
            {"name": "seven_day", "used_percent": 85, "resets_at": 2400, "window_secs": 604_800},
        ]);
        let policy =
            Policy::parse(&json!({"account_control":{"quota_limits":{"604800":80}}})).unwrap();
        assert_eq!(
            quota_block(&policy, &quota(&windows), 1000).unwrap(),
            json!({"rule": "window_limit", "window": "seven_day", "window_secs": 604_800,
                   "used_percent": 85, "limit_percent": 80, "resets_at": 2400})
        );
        let policy = Policy::parse(
            &json!({"account_control":{"quota_aware": true, "quota_threshold_pct": 90}}),
        )
        .unwrap();
        let tight = json!([{"name": "five_hour", "used_percent": 95, "resets_at": 1800, "window_secs": 18000}]);
        assert_eq!(
            quota_block(&policy, &quota(&tight), 1000).unwrap(),
            json!({"rule": "threshold", "headroom_percent": 5, "threshold_percent": 90, "window": null,
                   "resets_at": 1800})
        );
        let full = json!([{"name": "five_hour", "used_percent": 100, "resets_at": 1800, "window_secs": 18000}]);
        assert_eq!(
            quota_block(&policy, &quota(&full), 1000).unwrap()["rule"],
            "exhausted"
        );
        assert!(quota_block(&policy, &quota(&windows), 1000).is_none());
    }

    fn keys(candidates: &[ChannelCandidate]) -> Vec<i64> {
        candidates.iter().map(|c| c.channel_key_id).collect()
    }

    /// 缺省策略只按余量分三档：同一档里保持池策略排好的先后，不按精确余量把新会话全推给一个账号；
    /// 其它负载类策略只筛不排；层本身从不被推翻。
    #[test]
    fn headroom_tiers_keep_strategy_order_and_keep_leaves_it_alone() {
        use crate::gateway::scheduler::tests::cand_with_key;
        let original = vec![
            cand_with_key(9, 10),
            cand_with_key(1, 0),
            cand_with_key(2, 0),
            cand_with_key(3, 0),
            cand_with_key(4, 0),
        ];
        let headroom = std::collections::HashMap::from([(9, 5_u8), (1, 55), (2, 90), (3, 10)]);
        let resets = std::collections::HashMap::new();
        let mut tiered = original.clone();
        order_by_quota(&mut tiered, &headroom, &resets, QuotaOrder::HeadroomTiers);
        assert_eq!(keys(&tiered), vec![9, 1, 2, 4, 3]);
        let mut kept = original.clone();
        order_by_quota(&mut kept, &headroom, &resets, QuotaOrder::Keep);
        assert_eq!(keys(&kept), keys(&original));
    }

    /// soonest_reset：层内按最近一次重置升序，没有观测的按本层中位数，层不被推翻。
    #[test]
    fn soonest_reset_orders_within_layer_with_median_for_unknown() {
        use crate::gateway::scheduler::tests::cand_with_key;
        let mut candidates = vec![
            cand_with_key(9, 10),
            cand_with_key(1, 0),
            cand_with_key(3, 0),
            cand_with_key(2, 0),
            cand_with_key(4, 0),
        ];
        let resets =
            std::collections::HashMap::from([(9, 9_000_i64), (1, 3_000), (2, 1_000), (4, 5_000)]);
        order_by_quota(
            &mut candidates,
            &std::collections::HashMap::new(),
            &resets,
            QuotaOrder::SoonestReset,
        );
        assert_eq!(keys(&candidates), vec![9, 2, 1, 3, 4]);
    }
}
