use super::error::AppError;
use super::state::AppState;
use axum::http::HeaderMap;
use okapi_api::codes;
use okapi_store::AuthedKey;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Bearer 鉴权：SHA-256(key) → Redis 缓存（auth:key:*，60s + 版本失效）→ PG 回源。
/// 缓存在 Redis 而非进程内：console 的角色/分组变更经 auth:ver 跨进程立即失效
/// （docs/database.md §2.3/§2.4）。
pub async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<AuthedKey>, AppError> {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    // Anthropic 协议客户端（Claude Code 等）用 x-api-key 头
    let token = bearer
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| AppError::unauthorized(codes::INVALID_API_KEY))?;

    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let authed = if let Some(hit) = state.sched.auth_get(&key_hash).await {
        Arc::new(hit)
    } else {
        let found = okapi_store::auth::find_key_by_hash(&state.pg, &key_hash)
            .await?
            .ok_or_else(|| AppError::unauthorized(codes::INVALID_API_KEY))?;
        state.sched.auth_set(&key_hash, &found).await;
        Arc::new(found)
    };

    if !authed.is_usable(chrono::Utc::now()) {
        return Err(AppError::unauthorized(codes::KEY_DISABLED));
    }
    Ok(authed)
}

/// 数据面鉴权 = `authenticate` + key 级 IP 白名单（§11.17）。
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
    let authed = authenticate(state, headers).await?;
    if authed
        .ip_allowlist
        .as_deref()
        .is_some_and(|l| !l.is_empty())
    {
        let ip = super::clients::client_ip(headers);
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

/// 渠道 key 的选路反馈：时延 EWMA（least_latency 池排序输入）+ 当日消费（上限闸输入）。
///
/// 只在结算后调用，故只有成功请求进入时延样本——失败请求的耗时多半是超时或握手失败，
/// 混进去会把一个刚恢复的 key 长时间压在队尾。
pub async fn record_channel_key_feedback(
    state: &AppState,
    channel_key_id: i64,
    latency_ms: i32,
    amount_micro: i64,
) {
    if let Ok(sample) = u32::try_from(latency_ms) {
        state
            .sched
            .channel_key_latency_record(channel_key_id, sample)
            .await;
    }
    if amount_micro > 0 {
        state
            .sched
            .channel_key_spend_add(channel_key_id, amount_micro)
            .await;
    }
}
