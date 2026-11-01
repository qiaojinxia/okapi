//! 订阅额度预判：把「5 分钟一次的整数百分点轮询」补成实时估计，并据此在冷启动前判断一个请求
//! 会不会把某个窗口推过线。设计与线上取证见 docs/quota-forecast.md。
//!
//! - 观测：5 分钟轮询（整数、向上取整）+ 每个上游响应带回的 `anthropic-ratelimit-unified-*` 头
//!   （实时，零额外请求）。两路读数都带上当时这把 key 的累计烧速计数。
//! - 估计：û = u_obs + θ·（观测后又花的上游成本），θ 由相邻读数在线标定（[`model`]）。
//! - 判据：û + z·σ + 本请求增量 > 线。冷请求（新会话 / 缓存已过期）按整段 prompt 写缓存估增量，
//!   热请求（会话粘性命中且距上次不到 [`WARM_WINDOW_SECS`]）按几乎全读缓存估。
//!
//! 缺省 `shadow`：只把评估写进请求诊断，不拦任何请求；`enforce` 才据此排除候选。
#![allow(clippy::cast_precision_loss)] // 秒数 / micro-USD / token 数远小于 2^53

pub mod model;
mod store;
#[cfg(test)]
mod tests;

use super::policy::{ForecastMode, Policy};
use crate::gateway::{diagnostics::Trace, state::AppState};
use model::{
    Nowcast, Observation, Reading, Source, Z_BLOCK, debias, nowcast, prior_theta, would_exceed,
};
use okapi_providers::account::quota::Snapshot;
use okapi_providers::unified_limits::{ResponseMeta, Sink, UnifiedLimits};
use okapi_store::ChannelCandidate;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

pub(super) use store::KeyState;

/// 缓存还活着的判据：距这个会话上一次成功的请求不超过这么久（5 分钟 TTL 留 30 秒余量）。
pub const WARM_WINDOW_SECS: i64 = 270;

/// 一个请求的形状：预判据此估增量、区分冷 / 热。
#[derive(Clone, Debug)]
pub struct RequestShape {
    pub prompt_tokens: u32,
    /// 冷启动：整段 prompt 都写缓存，按价簿折成的 API 等价美元（渠道相对成本系数另乘）。
    pub cold_usd: f64,
    /// 热：几乎全读缓存，只有一小段新内容要写。
    pub warm_usd: f64,
    /// 会话粘性命中且缓存大概率还活着的 channel_key_id。
    pub warm_key: Option<i64>,
}

/// 对一个候选的评估。
pub struct Assessment {
    /// 写进请求诊断的条目（紧凑）。
    pub entry: Value,
    /// 若按 `enforce` 应当排除：与其它额度排除同形状的原因（带 `resets_at`）。
    pub block: Option<Value>,
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// 小请求的增量只有千分之几个百分点，两位小数会把它抹成 0。
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

fn window_name(window_secs: i64) -> &'static str {
    match window_secs {
        model::FIVE_HOURS => "five_hour",
        model::SEVEN_DAYS => "seven_day",
        _ => "other",
    }
}

/// 同一个窗口的两路观测里取更新的（并列取响应头）。
fn freshest(
    window_secs: i64,
    poll: Option<&(Snapshot, Option<i64>)>,
    state: Option<&KeyState>,
) -> Option<Observation> {
    let from_poll = poll.and_then(|(snapshot, burn_micro)| {
        let window = snapshot
            .windows
            .iter()
            .find(|w| w.window_secs == Some(window_secs))?;
        Some(Observation {
            window_secs,
            used: debias(window.used_percent),
            resets_at: window.resets_at,
            at: snapshot.observed_at,
            burn_micro: *burn_micro,
            source: Source::Poll,
        })
    });
    let from_header = state.and_then(|s| s.header.as_ref()).and_then(|header| {
        let window = header
            .windows
            .iter()
            .find(|w| w.window_secs == window_secs)?;
        Some(Observation {
            window_secs,
            used: window.used,
            resets_at: window.resets_at,
            at: header.at,
            burn_micro: Some(header.burn_micro),
            source: Source::Header,
        })
    });
    match (from_poll, from_header) {
        (Some(poll), Some(header)) => Some(if header.at >= poll.at { header } else { poll }),
        (poll, header) => poll.or(header),
    }
}

/// 评估一个候选：每个跟踪的窗口给出 û、σ、θ、本请求增量与对应的线。
/// 没有任何可用观测（非订阅渠道、Redis 无数据、观测过期）返回 None。
#[must_use]
pub fn assess(
    policy: &Policy,
    candidate: &ChannelCandidate,
    poll: Option<&(Snapshot, Option<i64>)>,
    state: Option<&KeyState>,
    shape: &RequestShape,
    now: i64,
) -> Option<Assessment> {
    let warm = shape.warm_key == Some(candidate.channel_key_id);
    // 渠道相对成本系数乘在官方价上得上游成本（与结算同口径）
    let usd =
        (if warm { shape.warm_usd } else { shape.cold_usd }) * candidate.cost_milli as f64 / 1000.0;
    let plan = state.and_then(|s| s.plan.as_deref());
    let burn_now = state.map(|s| s.burn_micro.unwrap_or(0));
    let mut windows = Vec::new();
    let mut worst: Option<(f64, Value)> = None;
    for window_secs in store::TRACKED_WINDOWS {
        let Some(observation) = freshest(window_secs, poll, state) else {
            continue;
        };
        let prior = prior_theta(plan, window_secs);
        let theta = state
            .and_then(|s| s.theta.get(&window_secs))
            .map_or(prior, |estimate| estimate.theta(prior));
        let Some(estimate): Option<Nowcast> = nowcast(&observation, burn_now, theta, now) else {
            continue;
        };
        let delta = theta * usd;
        let place = policy
            .quota_limits
            .get(&window_secs)
            .map_or(100.0, |cap| f64::from(*cap));
        let keep = policy
            .quota_keep_limits
            .get(&window_secs)
            .map_or(place, |cap| f64::from(*cap))
            .max(place);
        let line = if warm { keep } else { place };
        let hit = would_exceed(&estimate, delta, line, Z_BLOCK);
        windows.push(json!({
            "n": window_name(window_secs),
            "src": observation.source,
            "age": estimate.age_secs,
            "obs": round2(observation.used),
            "u": round2(estimate.used),
            "s": round2(estimate.sigma),
            "th": round2(theta),
            "d": round3(delta),
            "line": line,
            "hit": hit,
        }));
        if hit {
            let overshoot = estimate.used + Z_BLOCK * estimate.sigma + delta - line;
            if worst.as_ref().is_none_or(|(seen, _)| overshoot > *seen) {
                worst = Some((
                    overshoot,
                    json!({
                        "rule": "forecast",
                        "window": window_name(window_secs),
                        "window_secs": window_secs,
                        "used_percent": round2(estimate.used),
                        "sigma": round2(estimate.sigma),
                        "delta_percent": round2(delta),
                        "limit_percent": line,
                        "resets_at": observation.resets_at,
                    }),
                ));
            }
        }
    }
    if windows.is_empty() {
        return None;
    }
    let block = worst.map(|(_, detail)| detail);
    Some(Assessment {
        entry: json!({
            "k": candidate.channel_key_id,
            "warm": warm,
            "w": windows,
            "v": if block.is_some() { "block" } else { "ok" },
        }),
        block,
    })
}

/// 把整份评估写进请求诊断（管理端日志可见；个人日志的白名单不含它）。
pub fn trace_assessments(mode: ForecastMode, shape: &RequestShape, entries: &[Value]) {
    if entries.is_empty() {
        return;
    }
    if let Some(trace) = Trace::current() {
        trace.set(
            "forecast",
            json!({
                "mode": mode,
                "tokens": shape.prompt_tokens,
                "cold_usd": round2(shape.cold_usd),
                "warm_usd": round2(shape.warm_usd),
                "cands": entries,
            }),
        );
    }
}

/// 读一批候选 key 的预判输入（一次管道）。
pub async fn load(state: &AppState, keys: &[i64]) -> HashMap<i64, KeyState> {
    store::load(state, keys).await
}

/// 结算后把这笔上游成本记进 key 的烧速计数（API 等价 micro-USD）。
pub async fn record_burn(state: &AppState, key: i64, upstream_cost_micro: i64) {
    store::burn_add(state, key, upstream_cost_micro).await;
}

/// 这把 key 当前的烧速计数；None = Redis 读失败（基线未知），Some(0) = 还没有计数。
pub async fn current_burn(state: &AppState, key: i64) -> Option<i64> {
    store::burn_get(state, key).await
}

/// 额度轮询成功后调用：喂 θ 估计并留一条样本。`burn_micro` 为 None（Redis 读失败）则整条跳过——
/// 没有烧速基线的读数没法配对，也不该留成样本。
pub async fn record_poll(state: &AppState, key: i64, snapshot: &Snapshot, burn_micro: Option<i64>) {
    let Some(burn_micro) = burn_micro else {
        return;
    };
    let mut samples = Vec::new();
    for window in &snapshot.windows {
        let Some(window_secs) = window.window_secs else {
            continue;
        };
        samples.push(json!([window_secs, window.used_percent, window.resets_at]));
        if store::TRACKED_WINDOWS.contains(&window_secs) {
            store::learn(
                state,
                key,
                window_secs,
                Reading {
                    at: snapshot.observed_at,
                    used: debias(window.used_percent),
                    resets_at: window.resets_at,
                    burn_micro,
                },
            )
            .await;
        }
    }
    if !samples.is_empty() {
        let sample = json!({"t": snapshot.observed_at, "s": "poll", "b": burn_micro, "w": samples});
        store::history_append(state, key, &sample, false).await;
    }
}

/// 响应头读数：窗口用量落成百分点，超出 105 的读数当单位不对丢弃（防止口径猜错时污染估计）。
pub async fn record_headers(state: &AppState, key: i64, limits: &UnifiedLimits, at: i64) {
    let Some(burn_micro) = store::burn_get(state, key).await else {
        return;
    };
    let windows: Vec<store::HeaderWindow> = limits
        .windows
        .iter()
        .filter_map(|window| {
            let used = window.utilization? * 100.0;
            (used <= 105.0 && store::TRACKED_WINDOWS.contains(&window.window_secs)).then_some(
                store::HeaderWindow {
                    window_secs: window.window_secs,
                    used,
                    resets_at: window.resets_at,
                },
            )
        })
        .collect();
    if windows.is_empty() {
        return;
    }
    store::header_save(
        state,
        key,
        &store::HeaderObservation {
            at,
            burn_micro,
            windows: windows.clone(),
        },
    )
    .await;
    for window in &windows {
        store::learn(
            state,
            key,
            window.window_secs,
            Reading {
                at,
                used: window.used,
                resets_at: window.resets_at,
                burn_micro,
            },
        )
        .await;
    }
    let sample = json!({
        "t": at, "s": "hdr", "b": burn_micro,
        "w": windows.iter().map(|w| json!([w.window_secs, round2(w.used), w.resets_at])).collect::<Vec<_>>(),
        "claim": limits.representative_claim, "st": limits.status,
    });
    store::history_append(state, key, &sample, true).await;
}

/// 挂在订阅渠道的上游调用上的响应观测钩子：把限额头记进当前尝试的诊断，
/// 并在后台把它喂给预判（同步部分只做一次诊断写入，不阻塞响应）。
pub fn header_sink(state: &AppState, candidate: &ChannelCandidate) -> Sink {
    let state = state.clone();
    let key = candidate.channel_key_id;
    Arc::new(move |meta: &ResponseMeta<'_>| {
        if let Some(trace) = Trace::current() {
            let mut note = serde_json::to_value(meta.limits).unwrap_or_default();
            note["http"] = json!(meta.status);
            trace.attempt_note("rate_limit", note);
        }
        let limits = meta.limits.clone();
        let at = chrono::Utc::now().timestamp();
        let state = state.clone();
        // detach 说明：喂预判只是统计，丢一次只少一条读数；不进结算的跟踪计数——
        // Redis 卡住时不该拖住优雅停机去等它
        tokio::spawn(async move {
            record_headers(&state, key, &limits, at).await;
        });
    })
}
