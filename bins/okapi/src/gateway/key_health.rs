//! 渠道 key 健康反馈（各入口共用）：瞬态失败按「连续」计数，同一把 key 的下一次成功清零。
//!
//! 失败计数与冷却在 PG（`channels::mark_key_failure`）；成功清零只在本进程见过该 key
//! 瞬态失败时才回 PG，健康 key 的成功请求不多一次往返。其它副本各清自己见过的；
//! 遗漏的旧计数由存储层的半开窗口衰减，不会长期累积。
//!
//! 出口代理（IMPLEMENTATION §11.41）同一套做法：连接阶段失败（`KeyFailure::Unreachable`）不动
//! key——请求没送到上游，凭证无从判断。这次尝试走了代理时：确定坏在代理这一跳的（连不上代理 /
//! 代理拒绝认证）记到代理的被动熔断上；分不清是代理还是目标的（隧道中断、目标 TLS 失败、超时）
//! 先在后台经同一代理探一次独立的探测地址，探不通才熔断代理——一个上游挂了，不能把同一代理上的
//! 其他渠道一起拖下水（全局默认出口是代理时就是整站）。
use super::state::AppState;
use crate::worker::egress_probe::{ProbePolicy, SETTING as PROBE_SETTING};
use okapi_store::ChannelCandidate;
use okapi_store::channels::{self, KeyFailure};

/// 登记一次上游失败。只有瞬态失败参与连续计数，也只有它留本地标记等成功来清。
pub(crate) async fn failure(
    state: &AppState,
    cand: &ChannelCandidate,
    code: &str,
    kind: KeyFailure,
) {
    let _ = channels::mark_key_failure(&state.pg, cand.channel_key_id, code, kind).await;
    if matches!(kind, KeyFailure::Transient) {
        state.key_failures.insert(cand.channel_key_id, ()).await;
    }
    if let KeyFailure::Unreachable { proxy_hop } = kind
        && let Some(proxy) = cand.egress_proxy_id
    {
        let timed_out = code == okapi_api::codes::UPSTREAM_TIMEOUT;
        // 少见分支装箱：别让每个调用方的 future 都背上归因与核实的状态
        Box::pin(proxy_unreachable(
            state,
            proxy,
            cand.proxy_url.as_deref(),
            proxy_hop,
            timed_out,
        ))
        .await;
    }
}

/// 一次经代理的连接阶段失败（HTTP 与 WebSocket 路径共用）。`proxy_hop` 见
/// [`okapi_providers::UpstreamError::proxy_hop_failed`]。
pub(crate) async fn proxy_unreachable(
    state: &AppState,
    proxy: i64,
    proxy_url: Option<&str>,
    proxy_hop: bool,
    timed_out: bool,
) {
    if proxy_hop {
        let reason = if timed_out {
            "connect_timeout"
        } else {
            "connect_failed"
        };
        record_proxy_failure(state, proxy, reason, false).await;
    } else if let Some(url) = proxy_url {
        verify_proxy(state, proxy, url.to_owned()).await;
    }
}

async fn record_proxy_failure(state: &AppState, proxy: i64, reason: &str, confirmed: bool) {
    state.egress_failures.insert(proxy, ()).await;
    let tripped = okapi_store::egress::mark_failure(&state.pg, proxy, reason, confirmed)
        .await
        .unwrap_or(false);
    if tripped {
        // 刚熔断：别让 5s 的候选缓存继续把请求送进这个代理
        state.invalidate_routing_caches();
    }
}

/// 后台核实：经这个代理请求后台探测用的地址，探不通即确认是代理坏了，直接熔断；探得通说明是
/// 这条渠道的目标连不上，代理不背锅。同一代理一分钟内只核实一次；后台探测关掉时也不核实
/// （管理员不想要任何探测流量），分不清的失败就不记到代理上。
async fn verify_proxy(state: &AppState, proxy: i64, proxy_url: String) {
    let setting = state.setting_cached(PROBE_SETTING).await;
    let policy = setting
        .as_ref()
        .as_ref()
        .map_or_else(|| Some(ProbePolicy::default()), ProbePolicy::parse)
        .unwrap_or_default();
    if !policy.enabled
        || !state
            .egress_verifying
            .entry(proxy)
            .or_insert(())
            .await
            .is_fresh()
    {
        return;
    }
    let state = state.clone();
    // detach 说明：后台核实，最长一次探测的时长；进程退出时丢掉只是少记一次熔断
    tokio::spawn(async move {
        let probe = okapi_providers::egress_probe::probe(
            state.upstream.http(),
            Some(&proxy_url),
            policy.target(),
        )
        .await;
        if let Err(error) = probe {
            record_proxy_failure(&state, proxy, error.code, true).await;
        }
    });
}

/// 上游给出了可用响应：这把 key 是好的，清掉它的连续失败计数；走的出口代理同理。
pub(crate) async fn success(state: &AppState, cand: &ChannelCandidate) {
    if let Some(proxy) = cand.egress_proxy_id
        && state.egress_failures.contains_key(&proxy)
    {
        state.egress_failures.invalidate(&proxy).await;
        let _ = okapi_store::egress::clear_failures(&state.pg, proxy).await;
    }
    if !state.key_failures.contains_key(&cand.channel_key_id) {
        return;
    }
    state.key_failures.invalidate(&cand.channel_key_id).await;
    let _ = channels::clear_key_failures(&state.pg, cand.channel_key_id).await;
}
