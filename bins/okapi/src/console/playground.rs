//! Playground 试用台（IMPLEMENTATION §11.39）：同源流式中继 + 站点聊天预设。
//!
//! 数据面不开 CORS：gateway 与 console 分端口，浏览器直打 `/v1` 在所有单机形态下都是跨域。
//! 中继在本进程内**直接调用 `gateway::chat::chat_completions` 处理器**（同一 `AppState`、同一把
//! key），鉴权 / 限流 / 计费 / 日志与真实 SDK 调用逐字节一致——试用台看到的就是用户会看到的。
//! 中继只做两件事：强制 `stream: true`、请求体上限 1MB。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};

/// 试用台请求体上限：对话历史 + 系统提示词，1MB 已远超交互式用法。
pub const PLAYGROUND_BODY_LIMIT: usize = 1024 * 1024;

/// POST /api/me/playground/chat：改写 `stream: true` 后交给数据面处理器。
pub async fn chat(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if body.len() > PLAYGROUND_BODY_LIMIT {
        return AppError::new(StatusCode::PAYLOAD_TOO_LARGE, okapi_api::codes::BAD_REQUEST)
            .with_param("body_too_large")
            .into_response_with(None);
    }
    let Some(body) = force_stream(&body) else {
        return AppError::bad_request().into_response_with(None);
    };
    crate::gateway::chat::chat_completions(State(state), headers, body).await
}

/// 请求体必须是 JSON 对象；`stream` 置 true（非流式在同源长连接上没有意义，且流式才有首字体验）。
fn force_stream(body: &[u8]) -> Option<Bytes> {
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let obj = value.as_object_mut()?;
    obj.insert("stream".to_owned(), Value::Bool(true));
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

/// 站点预设一条（白名单字段、类型收口后的形状）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Preset {
    pub name: String,
    pub model: String,
    pub system: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f64>,
}

/// `settings.playground_presets` → 白名单收口：缺 name / model 的条目丢弃、文本截断、数值夹到合法区间。
/// settings 写入口是泛型 key/value，不能假设值形状（与 `site_notice` 同一立场）。
#[must_use]
pub fn sanitize_presets(raw: Option<&Value>) -> Vec<Preset> {
    let Some(items) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    let text = |v: &Value, key: &str, max: usize| -> String {
        v.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .chars()
            .take(max)
            .collect()
    };
    items
        .iter()
        .take(50)
        .filter_map(|item| {
            let name = text(item, "name", 64);
            let model = text(item, "model", 128);
            if name.is_empty() || model.is_empty() {
                return None;
            }
            Some(Preset {
                name,
                model,
                system: text(item, "system", 8000),
                temperature: item
                    .get("temperature")
                    .and_then(Value::as_f64)
                    .map(|t| t.clamp(0.0, 2.0)),
                max_tokens: item
                    .get("max_tokens")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0)
                    .map(|n| u32::try_from(n.min(1_000_000)).unwrap_or(u32::MAX)),
                top_p: item
                    .get("top_p")
                    .and_then(Value::as_f64)
                    .map(|p| p.clamp(0.0, 1.0)),
            })
        })
        .collect()
}

/// GET /api/playground/presets：公开只读（登录前的试用台占位页也要能列出）。
pub async fn presets(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let raw = state.setting_cached("playground_presets").await;
    Ok(Json(
        json!({ "data": sanitize_presets(raw.as_ref().as_ref()) }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_stream_rewrites_and_rejects_non_objects() {
        let out = force_stream(br#"{"model":"m","stream":false,"messages":[]}"#).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["stream"], true);
        assert_eq!(v["model"], "m");
        assert!(force_stream(b"[1,2]").is_none());
        assert!(force_stream(b"not json").is_none());
    }

    #[test]
    fn presets_are_whitelisted_and_clamped() {
        let raw = json!([
            {"name": " Writer ", "model": "gpt-4o", "system": "be brief", "temperature": 5,
             "max_tokens": 0, "top_p": -1, "secret": "x"},
            {"name": "", "model": "gpt-4o"},
            {"name": "no-model"},
            {"name": "Plain", "model": "claude", "temperature": "hot", "max_tokens": 512}
        ]);
        let out = sanitize_presets(Some(&raw));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "Writer");
        assert_eq!(out[0].temperature, Some(2.0));
        assert_eq!(out[0].max_tokens, None, "0 视为未设置");
        assert_eq!(out[0].top_p, Some(0.0));
        assert_eq!(out[1].temperature, None, "非数值丢弃");
        assert_eq!(out[1].max_tokens, Some(512));
        assert!(
            serde_json::to_value(&out[0])
                .unwrap()
                .get("secret")
                .is_none()
        );
        assert!(sanitize_presets(Some(&json!({"not": "array"}))).is_empty());
        assert!(sanitize_presets(None).is_empty());
    }
}
