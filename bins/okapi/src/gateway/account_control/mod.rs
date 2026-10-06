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
    let legacy_blocked = quota.headroom(now).is_some_and(|value| {
        value == 0 || policy.quota_aware && value <= 100 - policy.quota_threshold_pct
    });
    legacy_blocked
        || now.saturating_sub(quota.observed_at) <= 120
            && quota.windows.iter().any(|window| {
                window.resets_at.is_none_or(|at| at > now)
                    && window
                        .window_secs
                        .and_then(|seconds| policy.quota_limits.get(&seconds))
                        .is_some_and(|cap| window.used_percent >= *cap)
            })
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
pub async fn retain_available(state: &AppState, candidates: &mut impl CandidateSet) {
    let mut policies: std::collections::HashMap<i64, Option<Arc<Policy>>> =
        std::collections::HashMap::new();
    let mut available = std::collections::HashSet::with_capacity(candidates.len());
    let mut headroom = std::collections::HashMap::new();
    let mut token_limits = std::collections::HashMap::new();
    for candidate in candidates.iter() {
        let policy = match policies.entry(candidate.channel_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let value = policy(state, candidate.channel_id).await.ok();
                e.insert(value).clone()
            }
        };
        let Some(policy) = policy else {
            continue;
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
            continue;
        }
        if let Some(quota) = quota::read_for(state, candidate).await {
            let now = chrono::Utc::now().timestamp();
            let remaining = quota.headroom(now);
            if quota_blocked(&policy, &quota, now) {
                continue;
            }
            if let Some(remaining) = remaining {
                headroom.insert(candidate.channel_key_id, remaining);
            }
        }
        available.insert(candidate.channel_key_id);
    }
    // Stable ordering preserves the existing weighted/latency order when headroom ties.
    // Unknown quota is neutral, rather than falsely claiming an unused account.
    candidates.retain(|c| available.contains(&c.channel_key_id));
    candidates.sort_by_key(|c| {
        (
            c.pool_rank,
            std::cmp::Reverse(c.priority),
            std::cmp::Reverse(headroom.get(&c.channel_key_id).copied().unwrap_or(50)),
        )
    });
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
            let policy = Policy::parse(
                &json!({"account_control":{"quota_limits":{"18000":21,"604800":81}}}),
            )
            .unwrap();
            assert!(!quota_blocked(&policy, &quota, 1000));
            assert!(!quota_blocked(&policy, &quota, 1121));
            let mut fresh = quota.clone();
            fresh.observed_at = 1900;
            let policy =
                Policy::parse(&json!({"account_control":{"quota_limits":{"18000":20}}})).unwrap();
            assert!(!quota_blocked(&policy, &fresh, 1900));
        }
    }
}
