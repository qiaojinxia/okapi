//! 渠道 key 健康反馈（各入口共用）：瞬态失败按「连续」计数，同一把 key 的下一次成功清零。
//!
//! 失败计数与冷却在 PG（`channels::mark_key_failure`）；成功清零只在本进程见过该 key
//! 瞬态失败时才回 PG，健康 key 的成功请求不多一次往返。其它副本各清自己见过的；
//! 遗漏的旧计数由存储层的半开窗口衰减，不会长期累积。
//!
//! 出口代理（IMPLEMENTATION §11.41）同一套做法：连接阶段失败（`KeyFailure::Unreachable`）且
//! 这次尝试走了代理，记到代理的被动熔断上而不是 key 上——请求没送到上游，凭证无从判断。
use super::state::AppState;
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
    if matches!(kind, KeyFailure::Unreachable)
        && let Some(proxy) = cand.egress_proxy_id
    {
        let reason = if code == okapi_api::codes::UPSTREAM_TIMEOUT {
            "connect_timeout"
        } else {
            "connect_failed"
        };
        let _ = okapi_store::egress::mark_failure(&state.pg, proxy, reason).await;
        state.egress_failures.insert(proxy, ()).await;
    }
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
