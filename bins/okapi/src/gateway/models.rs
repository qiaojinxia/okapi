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

/// GET /v1beta/models（Gemini `models.list` 形状；Gemini SDK / CLI 探测可用模型用）。
pub async fn list_models_gemini(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let names = okapi_store::pricing::list_active_models(&state.pg).await?;
    let models: Vec<serde_json::Value> = names
        .into_iter()
        .map(|name| {
            json!({
                "name": format!("models/{name}"),
                "displayName": name,
                "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
            })
        })
        .collect();
    Ok(Json(json!({ "models": models })))
}
