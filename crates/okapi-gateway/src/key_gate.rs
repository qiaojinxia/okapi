//! 渠道 key 的选路闸（各入口共用）：key 级 RPM 与当日消费上限（`channel_keys.rpm_limit` /
//! `daily_spend_cap_micro`）。
//!
//! 闸要在真要用这个候选时才判：RPM 是固定窗口计数，判一次就占一格，提前把整张候选表
//! 过一遍会替没用上的 key 记账。消费闸是软实时的（结算后才累加，见
//! `AppState::record_key_spend`），所以可能略超上限。
use super::state::AppState;
use okapi_store::ChannelCandidate;

/// 候选被哪道闸挡下（`with_param` 用的名字，与 count_tokens 的 `channel_rpm` 同名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Closed {
    Rpm,
    DailySpend,
}

impl Closed {
    pub(crate) fn param(self) -> &'static str {
        match self {
            Self::Rpm => "channel_rpm",
            Self::DailySpend => "channel_daily_spend",
        }
    }
}

/// key 级 RPM：未配置上限时不产生 Redis 往返。
pub(crate) async fn rpm_ok(state: &AppState, cand: &ChannelCandidate) -> bool {
    match cand.rpm_limit.filter(|n| *n > 0) {
        Some(limit) => {
            state
                .sched
                .channel_key_rate_ok(cand.channel_key_id, i64::from(limit))
                .await
        }
        None => true,
    }
}

/// 会产生费用的请求用的整套闸：RPM + 当日消费。
pub(crate) async fn check(state: &AppState, cand: &ChannelCandidate) -> Result<(), Closed> {
    if !rpm_ok(state, cand).await {
        return Err(Closed::Rpm);
    }
    if let Some(cap) = cand.daily_spend_cap_micro
        && state.sched.channel_key_spend_get(cand.channel_key_id).await >= cap
    {
        return Err(Closed::DailySpend);
    }
    Ok(())
}
