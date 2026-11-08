//! 渠道 key 的后台探测与自动恢复（IMPLEMENTATION §11.73；对照 new-api 的定时测渠道 + 自动启用）。
//!
//! 被动的 key 状态机只在真实请求踩到时才变：凭证失效（6）要人手动复活，配额耗尽（4）要等到次日 0 点，
//! 长冷却（2）要等到期。这里定时对 key 发一次 16 token 的最小补全（`channel_probe::probe_model`，和控制台「测试」同一套），
//! 探通就放回轮转；`all` 模式下连正常的 key 也探，探不通按真实失败登记，不必等客户端先踩一脚。
//!
//! 范围刻意收窄：只探静态 key 的直连渠道（`PROBEABLE_PROVIDERS`）。订阅 OAuth 有自己的刷新与额度观测，云厂商要签名，
//! `custom_pass` 没有通用的最小补全。人工停用的 key（冷却期为空的 2）、停用的渠道、不在任何池里的渠道不探；
//! 渠道 `settings.health_probe = "off"` 可单独退出。默认关，要站长在 `channel_probe_policy` 里打开。
//!
//! 多副本：每把 key 的探测由 Redis 占位（SET NX EX，TTL = 探测间隔）选出一个副本，占位同时是「最近探过」的标记。

use futures::{StreamExt, stream};
use okapi_gateway::channel_probe::{PROBEABLE_PROVIDERS, ProbeTarget, failure_kind, probe_model};
use okapi_gateway::state::AppState;
use okapi_kernel::policies::{CHANNEL_PROBE_SETTING, ChannelProbeMode, ChannelProbePolicy};
use okapi_store::channels::{self, KeyFailure, ProbeKey};
use serde_json::{Value, json};
use std::time::Duration;

/// 开着时隔多久扫一轮（每把 key 的最小间隔由占位的 TTL 保证，不靠这个）。
const TICK: Duration = Duration::from_secs(30);
/// 关着时隔多久回来看一眼设置。
const IDLE: Duration = Duration::from_secs(60);
/// 一次从库里取多少把候选 key。
const PAGE: i64 = 100;

pub async fn run(state: AppState, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut after = 0;
    loop {
        if *stop.borrow() {
            return;
        }
        let policy = read_policy(&state).await;
        if policy.enabled {
            tokio::select! {
                _ = stop.changed() => return,
                result = probe_round(&state, &policy, &mut after) => match result {
                    Ok(report) if report.probed > 0 => tracing::info!(
                        probed = report.probed,
                        restored = report.restored,
                        failed = report.failed,
                        skipped = report.skipped,
                        "channel key probe round"
                    ),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "channel key probe round failed"),
                },
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(if policy.enabled { TICK } else { IDLE }) => {}
        }
    }
}

/// 没配 = 默认（关）；配错 = 关并留日志，不让一个写坏的值把探测悄悄变成别的样子。
async fn read_policy(state: &AppState) -> ChannelProbePolicy {
    let setting = state.setting_cached(CHANNEL_PROBE_SETTING).await;
    match setting.as_ref() {
        None => ChannelProbePolicy::default(),
        Some(value) => ChannelProbePolicy::parse(value).unwrap_or_else(|| {
            tracing::warn!("invalid channel_probe_policy; background channel probing disabled");
            ChannelProbePolicy::default()
        }),
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RoundReport {
    pub probed: usize,
    pub restored: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// 扫一轮：从游标往后分页取候选，逐把抢占位，抢到的凑满一批后有界并发地探。游标走到头就回到 0。
pub async fn probe_round(
    state: &AppState,
    policy: &ChannelProbePolicy,
    after: &mut i64,
) -> Result<RoundReport, okapi_store::StoreError> {
    let healthy = policy.mode == ChannelProbeMode::All;
    let lease = i64::try_from(policy.interval_secs).unwrap_or(900);
    let batch = usize::from(policy.batch_size);
    let mut claimed: Vec<ProbeKey> = Vec::new();
    'pages: loop {
        let page =
            channels::probe_keys(&state.pg, healthy, PROBEABLE_PROVIDERS, *after, PAGE).await?;
        if page.is_empty() {
            *after = 0;
            break;
        }
        for key in page {
            *after = key.channel_key_id;
            if state
                .sched
                .channel_probe_claim(key.channel_key_id, state.node.as_ref(), lease)
                .await
            {
                claimed.push(key);
                if claimed.len() >= batch {
                    break 'pages;
                }
            }
        }
    }
    let verdicts: Vec<Verdict> = stream::iter(claimed)
        .map(|key| probe_one(state, key))
        .buffer_unordered(usize::from(policy.concurrency.max(1)))
        .collect()
        .await;
    let mut report = RoundReport::default();
    for verdict in verdicts {
        match verdict {
            Verdict::Healthy => report.probed += 1,
            Verdict::Restored => {
                report.probed += 1;
                report.restored += 1;
            }
            Verdict::Failed => {
                report.probed += 1;
                report.failed += 1;
            }
            Verdict::Skipped(_) => report.skipped += 1,
        }
    }
    Ok(report)
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// 探通，key 本来就正常。
    Healthy,
    /// 探通，key 原先被摘出轮转，现在放回去了。
    Restored,
    /// 探不通。
    Failed,
    /// 没发请求（凭证读不出、出口不可用、协议不支持）：不是上游的错，不动 key。
    Skipped(&'static str),
}

async fn probe_one(state: &AppState, key: ProbeKey) -> Verdict {
    let key_id = key.channel_key_id;
    let credential = match okapi_store::credential::open(
        state.master_key.as_deref(),
        &key.credential_ciphertext,
    ) {
        Ok(credential) => credential,
        Err(error) => {
            tracing::warn!(key_id, %error, "channel key credential unreadable; probe skipped");
            return Verdict::Skipped("credential_unreadable");
        }
    };
    // 探测走的出口和真实流量是同一个；出口不可用就不探——绝不退回直连换一个 IP 去试
    let proxy_url =
        match okapi_store::egress::resolve_for_key(&state.pg, key_id, state.master_key.as_deref())
            .await
        {
            Ok(Some(resolved)) => match resolved.proxy_url() {
                Ok(proxy_url) => proxy_url,
                Err(_) => return Verdict::Skipped("egress_unavailable"),
            },
            Ok(None) => return Verdict::Skipped("key_gone"),
            Err(error) => {
                tracing::warn!(key_id, %error, "egress lookup failed; probe skipped");
                return Verdict::Skipped("egress_lookup_failed");
            }
        };
    let base = key
        .api_base
        .as_deref()
        .unwrap_or_default()
        .trim_end_matches('/');
    if base.is_empty() {
        return Verdict::Skipped("no_api_base");
    }
    // 探的是网关真正会发出去的名字：过一遍 model_mapping（azure 下映射值就是部署名）
    let upstream_model = key
        .model_mapping
        .get(&key.probe_model)
        .and_then(Value::as_str)
        .unwrap_or(&key.probe_model);
    let api_version = key
        .settings
        .get("api_version")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty());
    let Ok(outcome) = probe_model(
        state,
        ProbeTarget {
            provider: &key.provider,
            api_base: base,
            model: upstream_model,
            api_version,
            settings: &key.settings,
            credential,
            proxy_url,
        },
    )
    .await
    else {
        return Verdict::Skipped("probe_unsupported");
    };
    // 留痕供渠道列表「最近测试」列回填（和手动测试同一处）；auto 标出是后台探测
    let note = json!({
        "ok": outcome.failure.is_none(),
        "http_status": outcome.http_status,
        "error_code": outcome.failure.as_ref().map(okapi_providers::UpstreamError::error_code),
        "latency_ms": outcome.latency_ms,
        "at": chrono::Utc::now().to_rfc3339(),
        "scope": "model",
        "model": key.probe_model,
        "auto": true,
        "key_id": key_id,
    });
    state.sched.channel_test_record(key.channel_id, &note).await;

    let Some(error) = outcome.failure else {
        return restore_or_confirm(state, &key).await;
    };
    let kind = failure_kind(&error);
    // 正常的 key 探不通：和真实请求踩到一样登记；已经被摘出去的 key 本来就是坏的，不重复登记
    if key.key_status == 1 && !matches!(kind, KeyFailure::Request | KeyFailure::Unreachable { .. })
    {
        if let Err(store_error) =
            channels::mark_key_failure(&state.pg, key_id, error.error_code(), kind).await
        {
            tracing::warn!(key_id, %store_error, "probe failure could not be recorded");
        } else {
            tracing::warn!(
                key_id,
                channel_id = key.channel_id,
                code = error.error_code(),
                "channel key failed its probe"
            );
        }
    }
    Verdict::Failed
}

async fn restore_or_confirm(state: &AppState, key: &ProbeKey) -> Verdict {
    if key.key_status == 1 {
        if let Err(error) = channels::clear_key_failures(&state.pg, key.channel_key_id).await {
            tracing::warn!(key_id = key.channel_key_id, %error, "failure counter could not be cleared");
        }
        return Verdict::Healthy;
    }
    match channels::restore_key_after_probe(&state.pg, key.channel_key_id).await {
        Ok(true) => {
            tracing::info!(
                key_id = key.channel_key_id,
                channel_id = key.channel_id,
                was_status = key.key_status,
                "channel key passed its probe and is back in rotation"
            );
            Verdict::Restored
        }
        // 探的这一刻已经被别处恢复（或被管理员停用）：不算我们放回去的
        Ok(false) => Verdict::Healthy,
        Err(error) => {
            tracing::warn!(key_id = key.channel_key_id, %error, "probed key could not be restored");
            Verdict::Failed
        }
    }
}
