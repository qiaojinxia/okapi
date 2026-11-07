//! 模型解析与候选渠道（各入口共用）：60 秒的模型缓存与 5 秒的候选缓存。
use super::state::AppState;
use okapi_kernel::error::AppError;
use okapi_store::ChannelCandidate;
use std::sync::Arc;

/// 模型解析（60s 进程缓存；miss 回源 PG）。
pub(crate) async fn resolve_model_cached(
    state: &AppState,
    requested: &str,
) -> Result<Arc<Option<okapi_store::channels::ResolvedModel>>, AppError> {
    if let Some(hit) = state.routing.model_cache.get(requested).await {
        return Ok(hit);
    }
    let resolved = Arc::new(okapi_store::channels::resolve_model(&state.pg, requested).await?);
    state
        .routing
        .model_cache
        .insert(requested.to_owned(), Arc::clone(&resolved))
        .await;
    Ok(resolved)
}

/// 模型在池链上的候选渠道。5s 进程缓存（热路径零 PG 读；console 写路径主动失效，多副本靠 TTL
/// 收敛）；缓存键含池链：不同池的候选集合不同，混用会把别的池的渠道发给用户。
pub(crate) async fn cached_candidates(
    state: &AppState,
    model: &str,
    pool_chain: &[&str],
    fresh: bool,
) -> Result<Arc<Vec<ChannelCandidate>>, okapi_store::StoreError> {
    let cache_key = format!("{model}|{}", pool_chain.join(">"));
    if !fresh && let Some(hit) = state.routing.cand_cache.get(&cache_key).await {
        return Ok(hit);
    }
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
    Ok(rows)
}
