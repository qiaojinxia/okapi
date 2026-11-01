//! Shared chat endpoint compatibility for routing, diagnostics and connection examples.
use super::error::{AppError, gemini_status_name};
use super::execution_plan::{ExecutionPlan, Requirements};
use okapi_api::ErrorBody;
use okapi_store::ChannelCandidate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) enum Ingress {
    #[default]
    #[serde(rename = "chat_completions")]
    OpenAi,
    #[serde(rename = "messages")]
    Anthropic,
    #[serde(rename = "responses")]
    Responses,
    #[serde(rename = "responses_compact")]
    ResponsesCompact,
    #[serde(rename = "gemini")]
    Gemini,
}

impl Ingress {
    pub const ALL: [Self; 5] = [
        Self::OpenAi,
        Self::Responses,
        Self::Anthropic,
        Self::Gemini,
        Self::ResponsesCompact,
    ];

    pub fn endpoint(self) -> &'static str {
        match self {
            Self::OpenAi => "/v1/chat/completions",
            Self::Anthropic => "/v1/messages",
            Self::Responses => "/v1/responses",
            Self::ResponsesCompact => "/v1/responses/compact",
            Self::Gemini => "/v1beta/models/{model}:generateContent",
        }
    }

    pub fn supports(
        self,
        provider: &str,
        upstream_model: &str,
        native: bool,
        caps: &Value,
    ) -> bool {
        ExecutionPlan::compile_target(
            self,
            provider,
            upstream_model,
            native,
            caps,
            Requirements::default(),
        )
        .is_ok()
    }

    pub fn accepts(self, channel: &ChannelCandidate, model: &str) -> bool {
        self.supports(
            &channel.provider,
            channel.upstream_model(model),
            channel.responses_native,
            &channel.capabilities,
        )
    }

    /// Only suggest endpoints reachable through the caller's current pool and active keys.
    pub fn available_endpoints(channels: &[ChannelCandidate], model: &str) -> Vec<&'static str> {
        Self::ALL
            .into_iter()
            .filter(|ingress| channels.iter().any(|c| ingress.accepts(c, model)))
            .map(Self::endpoint)
            .collect()
    }
}

/// 流内错误事件：错误发生在已经开始写 SSE 之后，按客户端所用的入口协议给出对应形状的 `error` 事件
/// （Responses 带 sequence_number，Anthropic / Gemini / OpenAI 各用自己的错误壳）。
pub(crate) fn stream_error_event(
    ingress: Ingress,
    error: &AppError,
    request_id: Uuid,
    sequence: Option<u64>,
) -> axum::response::sse::Event {
    axum::response::sse::Event::default()
        .event("error")
        .data(stream_error_payload(ingress, error, request_id, sequence).to_string())
}

fn stream_error_payload(
    ingress: Ingress,
    error: &AppError,
    request_id: Uuid,
    sequence: Option<u64>,
) -> serde_json::Value {
    use serde_json::json;
    match ingress {
        Ingress::Responses | Ingress::ResponsesCompact => json!({
            "type":"error", "code":error.code, "message":error.code, "param":error.param,
            "request_id":request_id, "sequence_number":sequence.unwrap_or(0),
        }),
        Ingress::Anthropic => json!({
            "type":"error", "error":{"type":error.code,"message":error.code,"param":error.param},
            "request_id":request_id,
        }),
        Ingress::Gemini => json!({
            "error":{"code":error.status.as_u16(),"message":error.code,
                "status":gemini_status_name(error.status),"param":error.param},
            "request_id":request_id,
        }),
        Ingress::OpenAi => {
            let body = ErrorBody::new(
                &error.code,
                error.param.clone(),
                Some(request_id.to_string()),
            );
            json!({"error":body.error,"request_id":request_id})
        }
    }
}

#[cfg(test)]
mod stream_error_tests {
    use super::*;
    use axum::http::StatusCode;
    use okapi_api::codes;

    #[test]
    fn stream_errors_follow_each_ingress_protocol() {
        let id = Uuid::new_v4();
        let error =
            AppError::new(StatusCode::GATEWAY_TIMEOUT, codes::UPSTREAM_TIMEOUT).with_param("idle");
        for ingress in Ingress::ALL {
            let payload = stream_error_payload(ingress, &error, id, Some(17));
            assert_eq!(payload["request_id"], id.to_string());
            match ingress {
                Ingress::Responses | Ingress::ResponsesCompact => {
                    assert_eq!(payload["type"], "error");
                    assert_eq!(payload["code"], codes::UPSTREAM_TIMEOUT);
                    assert_eq!(payload["sequence_number"], 17);
                }
                Ingress::Anthropic => {
                    assert_eq!(payload["type"], "error");
                    assert_eq!(payload["error"]["type"], codes::UPSTREAM_TIMEOUT);
                }
                Ingress::Gemini => {
                    assert_eq!(payload["error"]["code"], 504);
                    assert_eq!(payload["error"]["status"], "DEADLINE_EXCEEDED");
                }
                Ingress::OpenAi => {
                    assert_eq!(payload["error"]["code"], codes::UPSTREAM_TIMEOUT);
                    assert_eq!(payload["error"]["type"], "okapi_error");
                    assert_eq!(payload["error"]["request_id"], id.to_string());
                }
            }
        }
    }
}
