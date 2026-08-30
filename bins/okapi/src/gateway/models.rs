use super::error::AppError;
use super::state::AppState;
use axum::Json;
use axum::extract::State;
use serde_json::json;

/// GET /v1/models（OpenAI 兼容形状）。
pub async fn list_models(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let names = okapi_store::pricing::list_active_models(&state.pg).await?;
    let data: Vec<serde_json::Value> = names
        .into_iter()
        .map(|name| {
            json!({
                "id": name,
                "object": "model",
                "owned_by": "okapi",
            })
        })
        .collect();
    Ok(Json(json!({ "object": "list", "data": data })))
}
