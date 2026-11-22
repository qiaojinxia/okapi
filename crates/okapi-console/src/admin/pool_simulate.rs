//! 渠道池调度策略模拟（Dry-Run）：
//! `POST /admin/pools/{code}/simulate` 复用与生产热路径完全一致的 `SchedulingPipeline`，
//! 对已保存策略（以及可选未保存草稿 `draft_policy`）返回完整决策追踪。

use super::guard;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_domain::pool_policy::{PoolPolicy, RequestFeatures};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::{Json as ExtractJson, Path};
use okapi_store::pool_policy::StoredPoolPolicy;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct SimulatePoolReq {
    pub model: String,
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub has_tools: bool,
    #[serde(default)]
    pub session_key: Option<String>,
    /// 可选未保存草稿策略：传入时先校验合法性与无环，再并行模拟返回 `draft` 决策追踪。
    #[serde(default)]
    pub draft_policy: Option<PoolPolicy>,
}

/// 策略模拟入口：只读不写库、不改缓存、不扣额。
pub async fn simulate_pool_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    ExtractJson(req): ExtractJson<SimulatePoolReq>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::CHANNEL_READ).await?;
    let requested = req.model.trim();
    if requested.is_empty() || requested.len() > 128 {
        return Err(AppError::bad_request().with_param("model"));
    }
    let saved = okapi_store::pool_policy::load_pool_policy(&state.pg, &code)
        .await?
        .ok_or_else(|| {
            AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("pool_code")
        })?;

    // 与真实请求同一套模型别名归一；未入模型的名字允许原样试跑，便于上线前预检。
    let canonical = okapi_engine::candidates::resolve_model_cached(&state, requested)
        .await?
        .as_ref()
        .as_ref()
        .map_or_else(|| requested.to_owned(), |m| m.canonical.clone());

    let features = RequestFeatures {
        model: &canonical,
        prompt_tokens: req.prompt_tokens,
        has_tools: req.has_tools,
        has_warm_cache: false,
        session_key: req.session_key.as_deref(),
    };

    let saved_trace = okapi_engine::policy_pipeline::simulate(&state, &saved, &features).await?;

    let draft_trace = if let Some(draft_policy) = req.draft_policy {
        draft_policy
            .validate(&code)
            .map_err(|param| AppError::bad_request().with_param(param))?;
        let graph = okapi_store::pool_policy::list_pool_graph(&state.pg).await?;
        for target in draft_policy.referenced_pools() {
            if target != code && !graph.iter().any(|n| n.pool_code == target) {
                return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
                    .with_param(target.to_owned()));
            }
        }
        if okapi_store::pool_policy::has_fallback_cycle(
            &graph,
            &code,
            saved.fallback_pool_code.as_deref(),
            &draft_policy,
        ) {
            return Err(AppError::bad_request().with_param("policy.fallback_pools.cycle"));
        }
        let draft_stored = StoredPoolPolicy {
            policy: draft_policy,
            policy_version: saved.policy_version + 1,
            ..saved
        };
        Some(okapi_engine::policy_pipeline::simulate(&state, &draft_stored, &features).await?)
    } else {
        None
    };

    Ok(Json(json!({
        "pool_code": code,
        "requested_model": requested,
        "canonical_model": canonical,
        "saved": saved_trace,
        "draft": draft_trace,
    })))
}
