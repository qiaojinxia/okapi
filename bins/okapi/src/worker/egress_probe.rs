//! 出口代理后台探测（IMPLEMENTATION §11.41 二期）：定时经每个启用中的代理请求探测地址，
//! 刷新出口 IP / 国家 / 延迟。出口 IP 变了发 `egress_ip_changed`——「静态」代理悄悄换了 IP，
//! 等于固定分配在它上面的账号全都换了 IP；有渠道在用的代理不可达发 `egress_down`。
//!
//! 只记事实、不碰熔断：探测地址不是上游，「能到 Cloudflare」证明不了「能到上游」，
//! 熔断只认真实请求的连接失败（`key_health`）。多副本时每一轮由 Redis 租约选出一个副本执行。

use crate::gateway::state::AppState;
use crate::worker::notify::Notifier;
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

/// `settings.egress_probe_policy`；缺省开启、10 分钟一轮。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProbePolicy {
    pub enabled: bool,
    pub interval_secs: u64,
    /// 探测地址；None = Cloudflare trace（一次拿到出口 IP 与国家）。
    pub target: Option<String>,
    pub concurrency: u8,
}

impl Default for ProbePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 600,
            target: None,
            concurrency: 4,
        }
    }
}

impl ProbePolicy {
    /// 形状与取值域校验（设置写入与 worker 读取共用）。探测地址只认 http(s) 绝对 URL；
    /// SSRF 闸由写入端（console）另行检查。
    pub fn parse(value: &Value) -> Option<Self> {
        let policy = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value::<Self>(value.clone()).ok()?
        };
        let target_ok = policy.target.as_deref().is_none_or(|target| {
            reqwest::Url::parse(target)
                .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some())
        });
        ((60..=86_400).contains(&policy.interval_secs)
            && (1..=16).contains(&policy.concurrency)
            && target_ok)
            .then_some(policy)
    }

    fn target(&self) -> &str {
        self.target
            .as_deref()
            .unwrap_or(okapi_providers::egress_probe::DEFAULT_TARGET)
    }
}

const SETTING: &str = "egress_probe_policy";

pub async fn run(
    state: AppState,
    notifier: Notifier,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *stop.borrow() {
            return;
        }
        let setting = state.setting_cached(SETTING).await;
        let policy = setting
            .as_ref()
            .as_ref()
            .map_or_else(|| Some(ProbePolicy::default()), ProbePolicy::parse)
            .unwrap_or_else(|| {
                tracing::warn!("invalid egress_probe_policy; background egress probing disabled");
                ProbePolicy {
                    enabled: false,
                    ..ProbePolicy::default()
                }
            });
        // 租约时长略短于间隔：下一轮醒来时已过期，可以重新争
        let lease = i64::try_from(policy.interval_secs.saturating_sub(5).max(30)).unwrap_or(600);
        if policy.enabled
            && state
                .sched
                .egress_probe_claim(state.node.as_ref(), lease)
                .await
        {
            tokio::select! {
                _ = stop.changed() => return,
                result = probe_round(&state, &policy, &notifier) => {
                    if let Err(error) = result {
                        tracing::warn!(error = %error, "egress probe round failed");
                    }
                }
            }
        }
        // 关闭时也按缺省间隔回来看一眼设置；租约让多副本里每轮只有一个在探
        let wait = if policy.enabled {
            policy.interval_secs
        } else {
            ProbePolicy::default().interval_secs
        };
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(Duration::from_secs(wait)) => {}
        }
    }
}

/// 探一轮：逐个代理测试（有界并发），落库，汇总告警。
pub async fn probe_round(
    state: &AppState,
    policy: &ProbePolicy,
    notifier: &Notifier,
) -> anyhow::Result<RoundReport> {
    let targets = okapi_store::egress::probe_targets(&state.pg).await?;
    let target = policy.target();
    let outcomes = stream::iter(targets)
        .map(|proxy| async move {
            let url = match okapi_store::credential::open(
                state.master_key.as_deref(),
                &proxy.url_ciphertext,
            ) {
                Ok(url) => url,
                Err(error) => {
                    tracing::warn!(proxy = proxy.id, %error, "egress probe skipped: address unreadable");
                    return None;
                }
            };
            let result =
                okapi_providers::egress_probe::probe(state.upstream.http(), Some(&url), target)
                    .await;
            let (record, ok) = match &result {
                Ok(r) => (
                    okapi_store::egress::ProbeRecord {
                        ok: true,
                        exit_ip: r.exit_ip.as_deref(),
                        exit_country: r.country.as_deref(),
                        latency_ms: i32::try_from(r.latency_ms).ok(),
                        error: None,
                    },
                    true,
                ),
                Err(e) => (
                    okapi_store::egress::ProbeRecord {
                        ok: false,
                        exit_ip: None,
                        exit_country: None,
                        latency_ms: None,
                        error: Some(e.code),
                    },
                    false,
                ),
            };
            let change = match okapi_store::egress::record_probe(&state.pg, proxy.id, record, false)
                .await
            {
                Ok(change) => change,
                Err(error) => {
                    tracing::warn!(proxy = proxy.id, %error, "egress probe result not recorded");
                    None
                }
            };
            Some(Outcome {
                id: proxy.id,
                name: proxy.name,
                in_use: proxy.in_use,
                channel_count: proxy.channel_count,
                assigned_keys: proxy.assigned_keys,
                ok,
                error: result.err().map(|e| e.code),
                change,
            })
        })
        .buffer_unordered(usize::from(policy.concurrency.max(1)))
        .filter_map(|outcome| async move { outcome })
        .collect::<Vec<_>>()
        .await;
    let report = RoundReport::from(&outcomes);
    let changed: Vec<Value> = outcomes
        .iter()
        .filter_map(|o| {
            o.change.as_ref().map(|c| {
                json!({"proxy_id": o.id, "name": o.name, "previous_ip": c.previous,
                       "current_ip": c.current, "channels": o.channel_count,
                       "assigned_keys": o.assigned_keys})
            })
        })
        .collect();
    if !changed.is_empty() {
        tracing::warn!(count = changed.len(), "egress proxy exit IP changed");
        notifier
            .dispatch("egress_ip_changed", &json!({ "proxies": changed }))
            .await;
    }
    let down: Vec<Value> = outcomes
        .iter()
        .filter(|o| !o.ok && o.in_use)
        .map(|o| {
            json!({"proxy_id": o.id, "name": o.name, "error": o.error,
                   "channels": o.channel_count, "assigned_keys": o.assigned_keys})
        })
        .collect();
    if !down.is_empty() {
        tracing::warn!(count = down.len(), "egress proxies in use are unreachable");
        notifier
            .dispatch("egress_down", &json!({ "proxies": down }))
            .await;
    }
    Ok(report)
}

struct Outcome {
    id: i64,
    name: String,
    in_use: bool,
    channel_count: i64,
    assigned_keys: i64,
    ok: bool,
    error: Option<&'static str>,
    change: Option<okapi_store::egress::ExitChange>,
}

/// 一轮的统计（日志与测试用）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoundReport {
    pub probed: usize,
    pub failed: usize,
    pub changed: usize,
}

impl From<&Vec<Outcome>> for RoundReport {
    fn from(outcomes: &Vec<Outcome>) -> Self {
        Self {
            probed: outcomes.len(),
            failed: outcomes.iter().filter(|o| !o.ok).count(),
            changed: outcomes.iter().filter(|o| o.change.is_some()).count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_and_bounds() {
        assert_eq!(
            ProbePolicy::parse(&Value::Null),
            Some(ProbePolicy::default())
        );
        assert!(ProbePolicy::parse(&json!({"enabled": false})).is_some_and(|p| !p.enabled));
        assert_eq!(
            ProbePolicy::parse(&json!({"target": "https://ip.example.com/json"}))
                .unwrap()
                .target(),
            "https://ip.example.com/json"
        );
        for bad in [
            json!({"interval_secs": 59}),
            json!({"interval_secs": 86_401}),
            json!({"concurrency": 0}),
            json!({"concurrency": 17}),
            json!({"target": "ftp://x"}),
            json!({"target": "not a url"}),
            json!({"intervalsecs": 600}),
        ] {
            assert!(ProbePolicy::parse(&bad).is_none(), "{bad}");
        }
    }
}
