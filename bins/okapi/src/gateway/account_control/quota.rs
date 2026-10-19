//! Quota polling is paced and leased across replicas; observations contain no credentials.
use super::{AppState, policy, policy::RefreshMode};
use crate::gateway::credentials::{ResolvedCredential, stored_credential};
use fred::{
    interfaces::KeysInterface,
    types::{Expiration, SetOptions},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize)]
struct Observation {
    identity: String,
    snapshot: okapi_providers::account::quota::Snapshot,
}

fn identity(provider: &str, credential: &ResolvedCredential<'_>) -> String {
    hex::encode(Sha256::digest(
        format!(
            "{provider}:{}",
            credential
                .oauth()
                .and_then(|oauth| oauth.account_id.as_deref())
                .unwrap_or_else(|| credential.material())
        )
        .as_bytes(),
    ))
}

/// 成功后的探测间隔。上游的用量接口限流严格（官方客户端只在用户查看时调用），
/// 每分钟探测会长期停在 429。
const POLL_INTERVAL_SECS: i64 = 300;
/// 观测有效期须长于探测间隔，否则两次探测之间额度限制会失效。
const OBSERVATION_MAX_AGE_SECS: i64 = okapi_providers::account::quota::MAX_AGE_SECS;
/// 失败退避：2、4、8…分钟，封顶 1 小时；成功即清零。
const BACKOFF_BASE_SECS: i64 = 120;
const BACKOFF_MAX_SECS: i64 = 3600;

fn backoff_secs(failures: i64, retry_after: Option<i64>) -> i64 {
    let exponent = u32::try_from(failures.saturating_sub(1).clamp(0, 10)).unwrap_or(10);
    let backoff = BACKOFF_BASE_SECS
        .saturating_mul(2_i64.saturating_pow(exponent))
        .min(BACKOFF_MAX_SECS);
    retry_after.map_or(backoff, |secs| {
        secs.clamp(60, BACKOFF_MAX_SECS).max(backoff)
    })
}

async fn observation(state: &AppState, key: i64) -> Option<Observation> {
    let payload: Option<String> = state
        .sched
        .client()
        .get(format!("quota:ck:{key}"))
        .await
        .ok()?;
    let observation: Observation = serde_json::from_str(payload.as_deref()?).ok()?;
    (chrono::Utc::now()
        .timestamp()
        .saturating_sub(observation.snapshot.observed_at)
        <= OBSERVATION_MAX_AGE_SECS)
        .then_some(observation)
}

/// Routing already resolved the credential, so quota sorting needs no extra PG reads.
pub async fn read_for(
    state: &AppState,
    candidate: &okapi_store::ChannelCandidate,
) -> Option<okapi_providers::account::quota::Snapshot> {
    let hook = okapi_providers::registry::lookup(&candidate.provider)?
        .account
        .filter(|hook| hook.capabilities().quota)?;
    let mut observation = observation(state, candidate.channel_key_id).await?;
    hook.scope_quota(&mut observation.snapshot);
    let credential = stored_credential(&candidate.provider, &candidate.credential).ok()?;
    (identity(&candidate.provider, &credential) == observation.identity)
        .then_some(observation.snapshot)
}

pub async fn read(state: &AppState, key: i64) -> Option<okapi_providers::account::quota::Snapshot> {
    let mut observation = observation(state, key).await?;
    // A reauthorization to another account must never inherit the old quota state.
    let row = okapi_store::oauth_credentials::snapshot(&state.pg, key, state.master_key.as_deref())
        .await
        .ok()??;
    let provider: String = sqlx::query_scalar(
        "SELECT c.provider FROM channels c JOIN channel_keys k ON k.channel_id=c.id WHERE k.id=$1",
    )
    .bind(key)
    .fetch_one(&state.pg)
    .await
    .ok()?;
    let credential = stored_credential(&provider, &row.plaintext).ok()?;
    okapi_providers::registry::lookup(&provider)?
        .account?
        .scope_quota(&mut observation.snapshot);
    (identity(&provider, &credential) == observation.identity).then_some(observation.snapshot)
}

async fn observation_credential<'a>(
    state: &AppState,
    row: &okapi_store::oauth_credentials::KeyRow,
    plaintext: &'a str,
    refresh: bool,
    proxy_url: Option<&str>,
) -> Option<ResolvedCredential<'a>> {
    let key = super::super::credentials::oauth::OAuthKey {
        channel_key_id: row.id,
        provider: &row.provider,
        token_url: row
            .settings
            .get("oauth_token_url")
            .and_then(serde_json::Value::as_str),
        proxy_url,
    };
    let stored = stored_credential(&row.provider, plaintext).ok()?;
    if stored.oauth().is_some() && refresh {
        super::super::credentials::oauth::fresh_credential_for(state, &key, plaintext)
            .await
            .ok()
            .map(ResolvedCredential::OAuth)
    } else {
        Some(stored)
    }
}

/// 额度探测与推理走同一出口（§11.41）；出口不可用就不探测，绝不改走直连。
/// 外层 None = 本轮不探测（已记日志）；内层是代理地址（None = 直连出口）。
async fn probe_proxy(state: &AppState, key: i64) -> Option<Option<String>> {
    let egress = match okapi_store::egress::resolve_for_key(
        &state.pg,
        key,
        state.master_key.as_deref(),
    )
    .await
    {
        Ok(Some(egress)) => egress,
        Ok(None) => {
            tracing::warn!(key, "quota observation skipped: egress unavailable");
            return None;
        }
        Err(error) => {
            tracing::warn!(key, %error, "quota observation skipped: egress could not be resolved");
            return None;
        }
    };
    if let Ok(url) = egress.proxy_url() {
        Some(url)
    } else {
        tracing::warn!(key, "quota observation skipped: egress proxy URL invalid");
        None
    }
}

/// 本轮探测租约（跨副本只探一次）。
async fn acquire_lease(state: &AppState, key: i64) -> bool {
    let lease: Result<Option<String>, _> = state
        .sched
        .client()
        .set(
            format!("quota:poll:ck:{key}"),
            "1",
            Some(Expiration::EX(POLL_INTERVAL_SECS)),
            Some(SetOptions::NX),
            false,
        )
        .await;
    matches!(lease, Ok(Some(_)))
}

pub async fn poll(state: &AppState, row: &okapi_store::oauth_credentials::KeyRow, plaintext: &str) {
    let policy = match policy(state, row.channel_id).await {
        Ok(policy) => policy,
        Err(error) => {
            tracing::warn!(key = row.id, %error, "quota observation skipped: channel policy unavailable");
            return;
        }
    };
    let Some(adapter) = okapi_providers::registry::lookup(&row.provider) else {
        return;
    };
    let Some(hook) = adapter.account.filter(|hook| hook.capabilities().quota) else {
        return;
    };
    if !acquire_lease(state, row.id).await {
        return;
    }
    // 以下早退都发生在拿到本轮探测租约之后：这把 key 本轮观测就此缺席，要留痕
    let Some(proxy_url) = probe_proxy(state, row.id).await else {
        return;
    };
    let Some(credential) = observation_credential(
        state,
        row,
        plaintext,
        policy.refresh_mode == RefreshMode::Managed && hook.capabilities().refresh,
        proxy_url.as_deref(),
    )
    .await
    else {
        tracing::warn!(
            key = row.id,
            "quota observation skipped: credential unavailable"
        );
        return;
    };
    let Some(base) = api_base(state, row).await else {
        tracing::warn!(
            key = row.id,
            "quota observation skipped: channel api_base unavailable"
        );
        return;
    };
    let outbound = okapi_providers::Outbound::from_settings(&row.settings, proxy_url);
    let context = || okapi_providers::account::QuotaContext {
        http: state.upstream.http(),
        api_base: &base,
        access_token: credential.material(),
        account_id: credential
            .oauth()
            .and_then(|oauth| oauth.account_id.as_deref()),
        outbound: &outbound,
    };
    match hook.quota(context()).await {
        Ok(mut snapshot) => {
            let reported_plan = snapshot.plan.take();
            let observation = Observation {
                identity: identity(&row.provider, &credential),
                snapshot,
            };
            // 路由靠额度观测：先落库，档位（只供展示，可能还要再请求一次上游）放在后面
            if let Ok(payload) = serde_json::to_string(&observation) {
                let result: Result<(), _> = state
                    .sched
                    .client()
                    .set(
                        format!("quota:ck:{}", row.id),
                        payload,
                        Some(Expiration::EX(OBSERVATION_MAX_AGE_SECS)),
                        None,
                        false,
                    )
                    .await;
                let _: Result<i64, _> = state
                    .sched
                    .client()
                    .del(format!("quota:fail:ck:{}", row.id))
                    .await;
                if let Err(error) = result {
                    tracing::warn!(key=row.id,%error,"quota observation persistence failed");
                }
            }
            refresh_plan(
                state,
                row.id,
                hook,
                observation.identity,
                reported_plan,
                context(),
            )
            .await;
        }
        Err(error) => back_off(state, row.id, &error).await,
    }
}

/// 订阅档位很少变：额度探测成功后顺带查，每把 key 每天最多一次（失败或中途退出一小时后再试）。
/// 额度应答里已带档位的（Codex）每次探测直接记下，不另发请求。
const PLAN_POLL_SECS: i64 = 86_400;
const PLAN_RETRY_SECS: i64 = 3_600;
/// 观测保留两个周期，一次失败不会让列表上的档位消失。
const PLAN_MAX_AGE_SECS: i64 = 2 * PLAN_POLL_SECS;

#[derive(Deserialize, Serialize)]
struct PlanObservation {
    identity: String,
    plan: String,
}

async fn refresh_plan(
    state: &AppState,
    key: i64,
    hook: &dyn okapi_providers::account::AccountHooks,
    identity: String,
    reported: Option<String>,
    context: okapi_providers::account::QuotaContext<'_>,
) {
    let plan = match reported {
        Some(plan) => plan,
        None => match probe_plan(state, key, hook, context).await {
            Some(plan) => plan,
            None => return,
        },
    };
    let observation = PlanObservation { identity, plan };
    if let Ok(payload) = serde_json::to_string(&observation) {
        let _: Result<(), _> = state
            .sched
            .client()
            .set(
                format!("plan:ck:{key}"),
                payload,
                Some(Expiration::EX(PLAN_MAX_AGE_SECS)),
                None,
                false,
            )
            .await;
    }
}

/// 单独查档位（Claude 走 profile），受每天一次的租约约束。
async fn probe_plan(
    state: &AppState,
    key: i64,
    hook: &dyn okapi_providers::account::AccountHooks,
    context: okapi_providers::account::QuotaContext<'_>,
) -> Option<String> {
    let lease: Result<Option<String>, _> = state
        .sched
        .client()
        .set(
            format!("plan:poll:ck:{key}"),
            "1",
            // 先占短租约：探测到一半进程退出，最多一小时后就会再试，不会整天没有档位
            Some(Expiration::EX(PLAN_RETRY_SECS)),
            Some(SetOptions::NX),
            false,
        )
        .await;
    if !matches!(lease, Ok(Some(_))) {
        return None;
    }
    match hook.plan(context).await {
        Ok(plan) => {
            // 有应答（认得出或认不出档位）才把租约延到一天；失败就留着短租约，一小时后重试
            let _: Result<bool, _> = state
                .sched
                .client()
                .expire(format!("plan:poll:ck:{key}"), PLAN_POLL_SECS, None)
                .await;
            plan
        }
        Err(error) => {
            tracing::debug!(key, %error, "subscription plan probe failed");
            None
        }
    }
}

/// 列表展示用：最近一次查到的档位；换了账号（凭证身份不符）就不显示旧档位。
pub async fn plan(state: &AppState, key: i64, provider: &str, plaintext: &str) -> Option<String> {
    let payload: Option<String> = state
        .sched
        .client()
        .get(format!("plan:ck:{key}"))
        .await
        .ok()?;
    let observation: PlanObservation = serde_json::from_str(payload.as_deref()?).ok()?;
    let credential = stored_credential(provider, plaintext).ok()?;
    (identity(provider, &credential) == observation.identity).then_some(observation.plan)
}

/// Probe failure is not inference failure; keep status intact and back off polling.
async fn back_off(state: &AppState, key: i64, error: &okapi_providers::UpstreamError) {
    // INCR 与 EXPIRE 同一脚本原子完成：分两步时 EXPIRE 失败会留下永不过期的计数，退避从此卡在封顶
    let failures: i64 = state
        .sched
        .incr_by_with_ttl(&format!("quota:fail:ck:{key}"), 1, BACKOFF_MAX_SECS * 2)
        .await
        .unwrap_or(1);
    let delay = backoff_secs(failures, error.retry_after_secs());
    let _: Result<bool, _> = state
        .sched
        .client()
        .expire(format!("quota:poll:ck:{key}"), delay, None)
        .await;
    let status = match error {
        okapi_providers::UpstreamError::Status { status, .. } => Some(*status),
        _ => None,
    };
    tracing::warn!(
        key,
        code = error.error_code(),
        status,
        failures,
        retry_in_secs = delay,
        "quota observation unavailable"
    );
}

async fn api_base(
    state: &AppState,
    row: &okapi_store::oauth_credentials::KeyRow,
) -> Option<String> {
    let base: Option<String> = sqlx::query_scalar("SELECT api_base FROM channels WHERE id=$1")
        .bind(row.channel_id)
        .fetch_one(&state.pg)
        .await
        .ok()?;
    Some(
        base.filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                okapi_providers::registry::lookup(&row.provider)
                    .and_then(|adapter| adapter.default_base)
                    .unwrap_or_default()
                    .into()
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::backoff_secs;

    #[test]
    fn failures_back_off_exponentially_and_honor_longer_retry_after() {
        assert_eq!(
            [1, 2, 3, 4, 5, 6, 20].map(|n| backoff_secs(n, None)),
            [120, 240, 480, 960, 1920, 3600, 3600]
        );
        assert_eq!(backoff_secs(1, Some(900)), 900, "longer server hint wins");
        assert_eq!(
            backoff_secs(3, Some(30)),
            480,
            "a short hint never shortens backoff"
        );
        assert_eq!(backoff_secs(1, Some(99_999)), 3600);
    }
}
