//! 价目目录建议（docs/pricing-catalog.md）：模型表单的输入辅助，只读。
//!
//! 目录是内置的 LiteLLM 社区价目表快照（`okapi_pricing::catalog`）。这里只把查询结果交给表单：
//! 不写库、不碰价簿——管理员点了"填入"、保存、发布，价格才会生效，发布后的价簿快照才是计费依据。

use super::admin::guard;
use super::query::Query;
use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use serde::Deserialize;
use serde_json::{Value, json};

/// 与模型表单的名称上限一致。
const MAX_NAME: usize = 128;
const DEFAULT_LIMIT: usize = 20;

#[derive(Deserialize)]
pub struct LookupQuery {
    pub model: String,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn catalog() -> Result<&'static okapi_pricing::catalog::Catalog, AppError> {
    okapi_pricing::catalog::embedded().map_err(|error| {
        tracing::error!(%error, "内置价目目录无法加载");
        AppError::internal()
    })
}

/// GET /admin/pricing/catalog/lookup?model=gpt-4o：这个名字在目录里的建议；目录没有就是 `null`。
pub async fn lookup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LookupQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_READ).await?;
    let name = query.model.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return Err(AppError::bad_request().with_param("model"));
    }
    let catalog = catalog()?;
    Ok(Json(json!({
        "suggestion": catalog.suggest(name),
        "snapshot": catalog.meta(),
    })))
}

/// GET /admin/pricing/catalog/search?q=claude-opus&limit=20：按名字联想，每个 id 一条。
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, AppError> {
    guard(&state, &headers, permissions::PRICING_READ).await?;
    let text = query.q.trim();
    if text.chars().count() > MAX_NAME {
        return Err(AppError::bad_request().with_param("q"));
    }
    let hits = catalog()?.search(text, query.limit.unwrap_or(DEFAULT_LIMIT));
    Ok(Json(json!({ "data": hits })))
}
