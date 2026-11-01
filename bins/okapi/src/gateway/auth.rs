use super::state::AppState;
use crate::kernel::auth::authenticate;
use crate::kernel::error::AppError;
use axum::http::HeaderMap;
use okapi_api::codes;
use okapi_store::AuthedKey;
use std::sync::Arc;

/// 数据面鉴权 = 过载准入 + `authenticate` + key 级 IP 白名单（§11.17）。
///
/// 过载准入在最前面：本进程后台结算积压超过上界时直接 503 `overloaded`（§12.2），
/// 不预扣、不碰上游，也不再产生新的结算——这是积压的唯一减法。
///
/// 白名单只约束 `/v1/*` 数据面：门户用同一把 key 登录，若把"只许我服务器的 IP 调用"
/// 也套到门户上，用户在自己笔记本上就再也进不了门户查账——那是把 new-api 令牌
/// IP 白名单（只作用于中继请求）的语义做错。来源 IP 由 `clients::client_ip` 按 §14.2
/// 信任闸判定（不可信来源不认转发头，只认 socket 对端）；配了名单却拿不到 IP 按不在
/// 名单上处理（fail-closed）。
pub async fn authenticate_data_plane(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<AuthedKey>, AppError> {
    state.check_settle_backlog()?;
    let authed = authenticate(state, headers).await?;
    if authed
        .ip_allowlist
        .as_deref()
        .is_some_and(|l| !l.is_empty())
    {
        let ip = crate::kernel::clients::client_ip(headers);
        if !authed.allows_ip(ip) {
            return Err(
                AppError::new(axum::http::StatusCode::FORBIDDEN, codes::IP_NOT_ALLOWED)
                    .with_param(ip.map(|i| i.to_string()).unwrap_or_default()),
            );
        }
    }
    Ok(authed)
}

/// 团成员月度限额检查（软实时：结算后计数、预扣前比较，§6.1）。
/// 各计费端点在 reserve 前调用；非团 key / 未配限额直接放行。
pub async fn check_member_limit(state: &AppState, key: &AuthedKey) -> Result<(), AppError> {
    let (Some(member), Some(limit)) = (key.member_user_id, key.member_monthly_limit_micro) else {
        return Ok(());
    };
    let spent = state.sched.member_spend_get(key.user_id, member).await;
    if spent >= limit {
        return Err(AppError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            codes::MEMBER_LIMIT_EXCEEDED,
        ));
    }
    Ok(())
}

/// 分组级限流（§11.32）：生效分组的每用户分钟 / 小时上限，随鉴权缓存下发。
/// 各计费端点紧接 `check_member_limit` 调用；分组未配限额时零 Redis 往返。
pub async fn check_group_rate(state: &AppState, key: &AuthedKey) -> Result<(), AppError> {
    let cap = |v: Option<i32>| v.filter(|n| *n > 0).map(i64::from);
    let (rpm, rph) = (cap(key.group_rpm_limit), cap(key.group_rph_limit));
    if rpm.is_none() && rph.is_none() {
        return Ok(());
    }
    if let Some(which) = state
        .sched
        .group_rate_check(key.user_id, &key.group_code, rpm, rph)
        .await
    {
        return Err(AppError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            codes::RATE_LIMITED,
        )
        .with_param(which));
    }
    Ok(())
}

/// 结算后累计软实时计数：团成员消费（团 key 才计）与用户本月 token/消费
/// （volume 规则的两个阈值轴输入，各自仅在价簿含对应规则时才写）。
pub async fn record_settlement_counters(
    state: &AppState,
    key_user_id: i64,
    member: Option<i64>,
    amount_micro: i64,
    tokens: u64,
) {
    if let Some(member) = member {
        state
            .sched
            .member_spend_add(key_user_id, member, amount_micro)
            .await;
    }
    super::rule_inputs::record_tokens(state, key_user_id, tokens, amount_micro).await;
}

/// 渠道 key 的时延反馈（least_latency 池排序输入）。
///
/// 只在结算后调用，故只有成功请求进入时延样本——失败请求的耗时多半是超时或握手失败，
/// 混进去会把一个刚恢复的 key 长时间压在队尾。当日消费在落账成功处统一累加
/// （`AppState::record_key_spend`），不在这里。只有对话类请求调它：媒体请求的耗时
/// （出图、转写）与对话不可比，混进同一把 key 的样本会扭曲排序。
pub async fn record_channel_key_latency(state: &AppState, channel_key_id: i64, latency_ms: i32) {
    if let Ok(sample) = u32::try_from(latency_ms) {
        state
            .sched
            .channel_key_latency_record(channel_key_id, sample)
            .await;
    }
}
