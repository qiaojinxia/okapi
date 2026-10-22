use super::error::AppError;
use super::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// GET /v1/models（OpenAI 兼容形状）。
pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let key = super::auth::authenticate_data_plane(&state, &headers).await?;
    let names = okapi_store::pricing::list_active_models(&state.pg).await?;
    let data: Vec<serde_json::Value> = names
        .into_iter()
        .filter(|name| key.allows_model(name))
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
/// 错误一律回 Gemini 错误壳（含查询串解析失败，如重复的 `?key=`），与 generateContent 一致。
pub async fn list_models_gemini(
    State(state): State<AppState>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    match gemini_models(&state, &uri, headers).await {
        Ok(json) => json.into_response(),
        Err(error) => error.into_gemini_response_with(None),
    }
}

async fn gemini_models(
    state: &AppState,
    uri: &axum::http::Uri,
    mut headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let query: GeminiModelsQuery = super::extract::query_from_uri(uri)?;
    if !headers.contains_key("x-goog-api-key")
        && !headers.contains_key(axum::http::header::AUTHORIZATION)
        && let Some(token) = query.key
    {
        headers.insert(
            "x-goog-api-key",
            token.parse().map_err(|_| AppError::bad_request())?,
        );
    }
    let key = super::auth::authenticate_data_plane(state, &headers).await?;
    let names = okapi_store::pricing::list_active_models(&state.pg).await?;
    let models: Vec<serde_json::Value> = names
        .into_iter()
        .filter(|name| key.allows_model(name))
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

#[derive(serde::Deserialize)]
pub struct GeminiModelsQuery {
    pub key: Option<String>,
}
