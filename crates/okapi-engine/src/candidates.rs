//! 模型解析与候选渠道（各入口共用）：60 秒的模型缓存、5 秒的候选缓存、按池的选路策略排 failover 序，
//! 以及媒体端点按注册表选渠道。
//!
//! 媒体端点只选注册表里声明了这一面的 provider（`registry::MediaSurface`），渠道还能用
//! `capabilities.<面> = false` 单独关掉。此前 embeddings / audio / images / videos / realtime 各写一份
//! provider 黑名单，口径互不相同，名单之外的 provider（含 custom_pass）一律放行。
use super::account_control::QuotaOrder;
use super::policy_pipeline;
use super::scheduler::{CandidateQueue, CandidateSet, LayerOptions, Strategy};
use super::state::AppState;
use axum::http::StatusCode;
use okapi_domain::pool_policy::RequestFeatures;
use okapi_kernel::error::AppError;
use okapi_providers::registry::{self, MediaSurface};
use okapi_store::{AuthedKey, ChannelCandidate};
use std::sync::Arc;

/// 模型解析（60s 进程缓存；miss 回源 PG，同键 singleflight 合并并发请求）。
pub async fn resolve_model_cached(
    state: &AppState,
    requested: &str,
) -> Result<Arc<Option<okapi_store::channels::ResolvedModel>>, AppError> {
    if let Some(hit) = state.routing.model_cache.get(requested).await {
        return Ok(hit);
    }
    Box::pin(
        state
            .routing
            .model_cache
            .try_get_with(requested.to_owned(), async {
                okapi_store::channels::resolve_model(&state.pg, requested)
                    .await
                    .map(Arc::new)
                    .map_err(AppError::from)
            }),
    )
    .await
    .map_err(|err| (*err).clone())
}

/// 模型在池链上的候选渠道。5s 进程缓存（热路径零 PG 读，同键 singleflight 防击穿；
/// console 写路径主动失效，多副本靠广播 + TTL 收敛）；缓存键含池链：不同池的候选集合不同，
/// 混用会把别的池的渠道发给用户。
pub async fn cached_candidates(
    state: &AppState,
    model: &str,
    pool_chain: &[&str],
    fresh: bool,
) -> Result<Arc<Vec<ChannelCandidate>>, AppError> {
    let cache_key = format!("{model}|{}", pool_chain.join(">"));
    if !fresh && let Some(hit) = state.routing.cand_cache.get(&cache_key).await {
        return Ok(hit);
    }
    if fresh {
        let rows = Arc::new(
            okapi_store::channels::candidates_for_model(
                &state.pg,
                model,
                pool_chain,
                state.master_key.as_deref(),
            )
            .await?,
        );
        state
            .routing
            .cand_cache
            .insert(cache_key, Arc::clone(&rows))
            .await;
        return Ok(rows);
    }
    Box::pin(state.routing.cand_cache.try_get_with(cache_key, async {
        okapi_store::channels::candidates_for_model(
            &state.pg,
            model,
            pool_chain,
            state.master_key.as_deref(),
        )
        .await
        .map(Arc::new)
        .map_err(AppError::from)
    }))
    .await
    .map_err(|err| (*err).clone())
}

/// 按池的选路策略排出 failover 序（只在同一优先级层内排）：时延 / 用量 / 在途数从 Redis 读，
/// 其余加权随机；`least_concurrency` 池发出的候选要登记在途。soonest_reset 的先后要读额度观测，
/// 由准入筛选排（`QuotaOrder::SoonestReset`），这里的加权随机只管同一时刻的并列。
pub async fn queue(
    state: &AppState,
    raw: Arc<Vec<ChannelCandidate>>,
    strategy: Strategy,
) -> CandidateQueue {
    queue_with_options(state, raw, strategy, LayerOptions::default()).await
}

/// 与 [`queue`] 相同，但按池策略指定的 [`LayerOptions`] 控制角色层级、priority 分层与成本权重开关。
pub async fn queue_with_options(
    state: &AppState,
    raw: Arc<Vec<ChannelCandidate>>,
    strategy: Strategy,
    opts: LayerOptions,
) -> CandidateQueue {
    let keys = || raw.iter().map(|c| c.channel_key_id);
    match strategy {
        Strategy::PriorityWeighted | Strategy::SoonestReset | Strategy::ExpiryUrgency => {
            CandidateQueue::weighted_with(raw, opts)
        }
        Strategy::LeastLatency => {
            let latency = state.sched.channel_key_latencies(keys()).await;
            CandidateQueue::by_latency_with(raw, &latency, opts)
        }
        Strategy::LeastUsage => {
            let spend = state.sched.channel_key_spends(keys()).await;
            CandidateQueue::by_load_with(raw, &spend, opts)
        }
        Strategy::LeastConcurrency => {
            let in_flight = state.sched.channel_key_in_flight(keys()).await;
            CandidateQueue::by_load_with(raw, &in_flight, opts).counting_in_flight()
        }
    }
}

/// 媒体端点的候选：加载渠道池策略文档并解析生效策略（多跳降级链、护栏、灰度、角色分层与选路策略），
/// 只留能接 `surface` 的，再过额度与毛利护栏。一个都不剩时回 503（`no_candidates_code` 区分是不是
/// 护栏筛掉的）。取出时每个候选重抽一次出口轮换组，`least_concurrency` 池的候选带上在途登记。
pub async fn media(
    state: &AppState,
    key: &AuthedKey,
    model: &str,
    surface: MediaSurface,
) -> Result<Vec<ChannelCandidate>, AppError> {
    Box::pin(media_inner(state, key, model, surface)).await
}

async fn media_inner(
    state: &AppState,
    key: &AuthedKey,
    model: &str,
    surface: MediaSurface,
) -> Result<Vec<ChannelCandidate>, AppError> {
    let primary_pool = key
        .pool_code
        .as_deref()
        .unwrap_or(okapi_store::channels::DEFAULT_POOL);
    let stored = state.load_pool_policy(primary_pool).await;
    let features = RequestFeatures {
        model,
        ..Default::default()
    };
    let effective = stored.resolve(&features);
    let strategy = Strategy::parse(Some(&effective.strategy));
    let chain: Vec<&str> = effective.pool_chain.iter().map(String::as_str).collect();
    let raw = cached_candidates(state, model, &chain, false).await?;
    let (mut candidates, _) =
        policy_pipeline::build_queue(state, raw, model, &effective, None).await;
    candidates.retain(|c| serves(c, surface));
    let margin_removed = state
        .retain_margin_ok_ordered(
            &key.group_code,
            &mut candidates,
            QuotaOrder::for_policy(strategy, &effective, 0),
            None,
        )
        .await
        .margin_removed;
    if candidates.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            super::state::no_candidates_code(margin_removed),
        ));
    }
    Ok(candidates.into_iter().collect())
}

/// 渠道能不能接这一面：provider 在注册表里声明了它，渠道的 `capabilities` 也没把它关掉。
pub fn serves(c: &ChannelCandidate, surface: MediaSurface) -> bool {
    registry::lookup(&c.provider).is_some_and(|adapter| adapter.serves(surface))
        && !switches(surface)
            .iter()
            .any(|name| c.capabilities.get(*name) == Some(&serde_json::Value::Bool(false)))
}

/// 渠道 `capabilities` 里关掉某一面的键（值为 `false` 即关）。图片批量也认 `images`。
fn switches(surface: MediaSurface) -> &'static [&'static str] {
    match surface {
        MediaSurface::Embeddings => &["embeddings"],
        MediaSurface::Audio => &["audio"],
        MediaSurface::Images => &["images"],
        MediaSurface::Videos => &["videos"],
        MediaSurface::Realtime => &["realtime"],
        MediaSurface::ImageBatches => &["batch_images", "images"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::tests::cand;
    use serde_json::json;

    fn channel(provider: &str, capabilities: serde_json::Value) -> ChannelCandidate {
        let mut c = cand(0, 1, 1000);
        provider.clone_into(&mut c.provider);
        c.capabilities = capabilities;
        c
    }

    /// 只认注册表的声明：黑名单时代放行的 gemini embeddings、订阅渠道的 realtime、custom_pass
    /// 与注册表里没有的 provider 都不再选。
    #[test]
    fn media_candidates_follow_the_registry() {
        let served = |provider, surface| serves(&channel(provider, json!({})), surface);
        assert!(served("openai_compat", MediaSurface::Realtime));
        assert!(served("azure", MediaSurface::Embeddings));
        assert!(!served("azure", MediaSurface::Videos));
        assert!(!served("gemini", MediaSurface::Embeddings));
        assert!(!served("codex", MediaSurface::Realtime));
        assert!(!served("anthropic_max", MediaSurface::Realtime));
        assert!(!served("custom_pass", MediaSurface::Images));
        assert!(!served("not-registered", MediaSurface::Audio));
        assert!(served("vertex", MediaSurface::ImageBatches));
        assert!(!served("openai", MediaSurface::ImageBatches));
    }

    /// 渠道用 `capabilities.<面> = false` 关掉一面，别的面照常；只有布尔 false 算关。
    #[test]
    fn a_channel_switches_a_surface_off_with_an_explicit_false() {
        let openai = |capabilities| channel("openai", capabilities);
        for (name, surface) in [
            ("embeddings", MediaSurface::Embeddings),
            ("audio", MediaSurface::Audio),
            ("images", MediaSurface::Images),
            ("videos", MediaSurface::Videos),
            ("realtime", MediaSurface::Realtime),
        ] {
            let mut off = serde_json::Map::new();
            off.insert(name.to_owned(), json!(false));
            let c = openai(serde_json::Value::Object(off));
            assert!(!serves(&c, surface), "{name}");
            let others = [MediaSurface::Embeddings, MediaSurface::Videos]
                .into_iter()
                .filter(|other| *other != surface);
            for other in others {
                assert!(serves(&c, other), "{name} only switches its own surface");
            }
        }
        assert!(serves(
            &openai(json!({"images": true})),
            MediaSurface::Images
        ));
        assert!(serves(
            &openai(json!({"images": "false"})),
            MediaSurface::Images
        ));
        // 图片批量另有自己的开关，同时也认 `images`
        let gemini = |capabilities| channel("gemini", capabilities);
        assert!(!serves(
            &gemini(json!({"batch_images": false})),
            MediaSurface::ImageBatches
        ));
        assert!(!serves(
            &gemini(json!({"images": false})),
            MediaSurface::ImageBatches
        ));
        assert!(serves(
            &gemini(json!({"realtime": false})),
            MediaSurface::ImageBatches
        ));
    }
}
