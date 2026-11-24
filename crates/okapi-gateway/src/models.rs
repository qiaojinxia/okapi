use super::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use okapi_kernel::error::AppError;
use serde_json::json;

/// GET /v1/models：OpenAI 列表形状，同时带上 Anthropic `models.list` 的字段，一个响应两边都能读。
/// OpenAI 客户端只认 `id` / `object` / `owned_by`，其余字段会被忽略；Anthropic SDK 与 Claude Code 的网关模型发现
/// 读 `id` / `display_name` / `description`（实测 Claude Code 2.1.311 会请求 `/v1/models?limit=1000`，
/// 只保留 id 含 claude 或 anthropic 的条目，选单标签取 `display_name`，缺了就显示 id）。
/// 上下文窗口、最大输出和能力来自模型配置里管理员明确填过的字段，没填就不出现，不按名字猜。
pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let key = super::auth::authenticate_data_plane(&state, &headers).await?;
    let cards = state.active_model_cards().await?;
    let data: Vec<serde_json::Value> = cards
        .iter()
        .filter(|card| key.allows_model(&card.model_name))
        .map(model_entry)
        .collect();
    let first = data.first().map(|entry| entry["id"].clone());
    let last = data.last().map(|entry| entry["id"].clone());
    Ok(Json(json!({
        "object": "list",
        "data": data,
        "has_more": false,
        "first_id": first,
        "last_id": last,
    })))
}

/// `/v1/models` 的一行：OpenAI 的 `id` / `object` / `created` / `owned_by`，Anthropic 的 `type` / `display_name` /
/// `created_at`（RFC 3339）/ `max_input_tokens` / `max_tokens`，以及本站自己的规格。厂商、类型、模态和能力白名单放进
/// `okapi` 扩展对象，不占用别家协议的字段名（Anthropic 的 `capabilities` 是对象，我们的是布尔表，混用会让严格的解析失败）。
fn model_entry(card: &okapi_store::pricing::ActiveModelCard) -> serde_json::Value {
    use serde_json::{Map, Value};
    let name = card.model_name.as_str();
    let display_name = card
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(name);
    let mut entry = Map::new();
    entry.insert("id".into(), json!(name));
    entry.insert("object".into(), json!("model"));
    entry.insert("type".into(), json!("model"));
    entry.insert("created".into(), json!(card.created_at.timestamp()));
    entry.insert(
        "created_at".into(),
        json!(
            card.created_at
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
    );
    entry.insert("owned_by".into(), json!("okapi"));
    entry.insert("display_name".into(), json!(display_name));
    if let Some(description) = card.catalog_config["description"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        entry.insert("description".into(), json!(description));
    }
    if let Some(window) = card.context_window.filter(|v| *v > 0) {
        entry.insert("context_window".into(), json!(window));
        entry.insert("max_input_tokens".into(), json!(window));
    }
    if let Some(output) = card.max_output.filter(|v| *v > 0) {
        entry.insert("max_output_tokens".into(), json!(output));
        entry.insert("max_tokens".into(), json!(output));
    }
    let mut ext = Map::new();
    if let Some(vendor) = card
        .vendor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        ext.insert("vendor".into(), json!(vendor));
    }
    for field in ["kind", "input_modalities", "output_modalities"] {
        let value = &card.catalog_config[field];
        let present = match value {
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            _ => false,
        };
        if present {
            ext.insert(field.into(), value.clone());
        }
    }
    if let Some(flags) = card.capabilities.as_object() {
        let mut on: Vec<&str> = flags
            .iter()
            .filter(|(_, v)| v.as_bool() == Some(true))
            .map(|(k, _)| k.as_str())
            .collect();
        on.sort_unstable();
        if !on.is_empty() {
            ext.insert("capabilities".into(), json!(on));
        }
    }
    if !ext.is_empty() {
        entry.insert("okapi".into(), Value::Object(ext));
    }
    Value::Object(entry)
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
    let query: GeminiModelsQuery = okapi_kernel::extract::query_from_uri(uri)?;
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
    let cards = state.active_model_cards().await?;
    let models: Vec<serde_json::Value> = cards
        .iter()
        .map(|card| card.model_name.as_str())
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

#[cfg(test)]
mod tests {
    use super::model_entry;
    use okapi_store::pricing::ActiveModelCard;
    use serde_json::json;

    fn card() -> ActiveModelCard {
        ActiveModelCard {
            model_name: "claude-sonnet-5-5".into(),
            display_name: Some(" Sonnet 5.5 ".into()),
            vendor: Some("anthropic".into()),
            capabilities: json!({"vision": true, "tools": true, "audio": false}),
            context_window: Some(1_000_000),
            max_output: Some(128_000),
            catalog_config: json!({"kind": "chat", "description": "Efficient", "input_modalities": ["text", "image"], "output_modalities": ["text"]}),
            created_at: chrono::DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn one_entry_serves_openai_and_anthropic_readers() {
        let entry = model_entry(&card());
        assert_eq!(entry["id"], "claude-sonnet-5-5");
        assert_eq!(entry["object"], "model");
        assert_eq!(entry["owned_by"], "okapi");
        assert_eq!(entry["created"], 1_760_000_000);
        assert_eq!(entry["type"], "model");
        assert_eq!(entry["display_name"], "Sonnet 5.5", "trimmed");
        assert_eq!(entry["created_at"], "2025-10-09T08:53:20Z");
        assert_eq!(entry["description"], "Efficient");
        assert_eq!(entry["context_window"], 1_000_000);
        assert_eq!(entry["max_input_tokens"], 1_000_000);
        assert_eq!(entry["max_output_tokens"], 128_000);
        assert_eq!(entry["max_tokens"], 128_000);
        assert_eq!(
            entry["okapi"],
            json!({
                "vendor": "anthropic",
                "kind": "chat",
                "input_modalities": ["text", "image"],
                "output_modalities": ["text"],
                "capabilities": ["tools", "vision"],
            }),
            "only capabilities that are explicitly true, sorted"
        );
        assert!(
            entry.get("capabilities").is_none(),
            "Anthropic's `capabilities` is an object; ours must not collide with it"
        );
    }

    #[test]
    fn unconfigured_fields_are_absent_not_guessed() {
        let bare = ActiveModelCard {
            model_name: "plain-model".into(),
            display_name: None,
            vendor: None,
            capabilities: json!({}),
            context_window: None,
            max_output: Some(0),
            catalog_config: json!({}),
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        };
        let entry = model_entry(&bare);
        assert_eq!(entry["display_name"], "plain-model", "falls back to the id");
        for absent in [
            "description",
            "context_window",
            "max_input_tokens",
            "max_output_tokens",
            "max_tokens",
            "okapi",
        ] {
            assert!(entry.get(absent).is_none(), "{absent} must not be invented");
        }
    }
}
