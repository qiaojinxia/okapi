use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use okapi_api::{ErrorBody, codes};
use okapi_ledger::LedgerError;
use okapi_pricing::PricingError;
use okapi_store::StoreError;
use uuid::Uuid;

pub(crate) fn stream_error_event(
    ingress: super::ingress::Ingress,
    error: &AppError,
    request_id: Uuid,
    sequence: Option<u64>,
) -> axum::response::sse::Event {
    axum::response::sse::Event::default()
        .event("error")
        .data(stream_error_payload(ingress, error, request_id, sequence).to_string())
}

fn stream_error_payload(
    ingress: super::ingress::Ingress,
    error: &AppError,
    request_id: Uuid,
    sequence: Option<u64>,
) -> serde_json::Value {
    use super::ingress::Ingress;
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
    use crate::gateway::ingress::Ingress;

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

/// gateway 统一错误：只携带 error_code（i18n 红线），状态码映射集中于此。
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub code: String,
    pub param: Option<String>,
}

impl AppError {
    #[must_use]
    pub fn new(status: StatusCode, code: &str) -> Self {
        Self {
            status,
            code: code.to_owned(),
            param: None,
        }
    }

    #[must_use]
    pub fn with_param(mut self, param: impl Into<String>) -> Self {
        self.param = Some(param.into());
        self
    }

    #[must_use]
    pub fn unauthorized(code: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code)
    }

    #[must_use]
    pub fn bad_request() -> Self {
        Self::new(StatusCode::BAD_REQUEST, codes::BAD_REQUEST)
    }

    #[must_use]
    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, codes::INTERNAL_ERROR)
    }

    /// 组装响应（body 与响应头均携带 request_id，便于用户报障关联）。
    #[must_use]
    pub fn into_response_with(self, request_id: Option<Uuid>) -> Response {
        let body = ErrorBody::new(
            &self.code,
            self.param.clone(),
            request_id.map(|id| id.to_string()),
        );
        let mut resp = (self.status, axum::Json(body)).into_response();
        self.attach_retry_after(&mut resp);
        if let Some(id) = request_id
            && let Ok(value) = axum::http::HeaderValue::from_str(&id.to_string())
        {
            resp.headers_mut().insert("x-okapi-request-id", value);
        }
        resp
    }

    fn attach_retry_after(&self, response: &mut Response) {
        if self.status != StatusCode::TOO_MANY_REQUESTS || self.code != codes::RATE_LIMITED {
            return;
        }
        // Only fixed-window limits have a known reset. Quota and concurrency
        // failures cannot promise that they will clear after an arbitrary delay.
        let period = match self.param.as_deref() {
            Some(
                "rpm" | "tpm" | "group_rpm" | "model_rpm" | "token_count_rpm" | "invalid_api_key",
            ) => 60,
            Some("group_rph") => 3600,
            Some("rpd" | "token_count_rpd") => 86_400,
            _ => return,
        };
        let remaining = period - chrono::Utc::now().timestamp().rem_euclid(period);
        if let Ok(value) = axum::http::HeaderValue::from_str(&remaining.to_string()) {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
    }
}

impl AppError {
    /// Anthropic 协议入口的错误壳（`{"type":"error","error":{...}}`）；
    /// message 仍只放 error_code（i18n 红线），param 以空格拼接供排障。
    #[must_use]
    pub fn into_anthropic_response_with(self, request_id: Option<Uuid>) -> Response {
        let message = match &self.param {
            Some(p) => format!("{} {p}", self.code),
            None => self.code.clone(),
        };
        let body = serde_json::json!({
            "type": "error",
            "error": {"type": self.code, "message": message},
            "request_id": request_id.map(|id| id.to_string()),
        });
        let mut resp = (self.status, axum::Json(body)).into_response();
        self.attach_retry_after(&mut resp);
        if let Some(id) = request_id
            && let Ok(value) = axum::http::HeaderValue::from_str(&id.to_string())
        {
            resp.headers_mut().insert("x-okapi-request-id", value);
        }
        resp
    }
}

impl AppError {
    /// Gemini 协议入口的错误壳（`{"error":{"code","message","status"}}`，google.rpc.Status 形状）；
    /// message 仍只放 error_code（i18n 红线），status 按 HTTP 状态映射到 gRPC 状态名。
    #[must_use]
    pub fn into_gemini_response_with(self, request_id: Option<Uuid>) -> Response {
        let message = match &self.param {
            Some(p) => format!("{} {p}", self.code),
            None => self.code.clone(),
        };
        let body = serde_json::json!({
            "error": {
                "code": self.status.as_u16(),
                "message": message,
                "status": gemini_status_name(self.status),
            },
            "request_id": request_id.map(|id| id.to_string()),
        });
        let mut resp = (self.status, axum::Json(body)).into_response();
        self.attach_retry_after(&mut resp);
        if let Some(id) = request_id
            && let Ok(value) = axum::http::HeaderValue::from_str(&id.to_string())
        {
            resp.headers_mut().insert("x-okapi-request-id", value);
        }
        resp
    }
}

/// HTTP 状态 → google.rpc.Code 名（Gemini 错误壳的 `status` 字段）。
#[must_use]
pub fn gemini_status_name(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "INVALID_ARGUMENT",
        StatusCode::UNAUTHORIZED => "UNAUTHENTICATED",
        StatusCode::PAYMENT_REQUIRED | StatusCode::FORBIDDEN => "PERMISSION_DENIED",
        StatusCode::NOT_FOUND => "NOT_FOUND",
        StatusCode::CONFLICT => "ABORTED",
        StatusCode::TOO_MANY_REQUESTS => "RESOURCE_EXHAUSTED",
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => "DEADLINE_EXCEEDED",
        StatusCode::NOT_IMPLEMENTED => "UNIMPLEMENTED",
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE => "UNAVAILABLE",
        _ => "INTERNAL",
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        self.into_response_with(None)
    }
}

impl From<StoreError> for AppError {
    fn from(err: StoreError) -> Self {
        // 资源占用冲突是可预期的管理端结果（非故障）：回 409 + error_code 供前端渲染，
        // 且不打 error 日志以免污染告警
        if let StoreError::Conflict(code) = err {
            return Self::new(StatusCode::CONFLICT, code);
        }
        // 超出 varchar 长度（22001）是输入问题：回 400、不打 error 日志。漏校验的字段也不该让
        // 任何人借超长值把告警日志刷满
        if let StoreError::Sqlx(sqlx::Error::Database(db)) = &err
            && db.code().as_deref() == Some("22001")
        {
            return Self::bad_request().with_param("value_too_long");
        }
        if let StoreError::InvalidData(
            code @ ("statistics_calendar_history_incomplete"
            | "statistics_request_history_incomplete"),
        ) = err
        {
            return Self::internal().with_param(code);
        }
        tracing::error!(error = %err, "store error");
        Self::internal()
    }
}

impl From<LedgerError> for AppError {
    fn from(err: LedgerError) -> Self {
        match err {
            LedgerError::KeyQuotaExceeded => {
                Self::new(StatusCode::TOO_MANY_REQUESTS, codes::KEY_QUOTA_EXCEEDED)
            }
            // 同一账户的账本操作排队过长：未预扣、未改账，客户端退避重试即可
            LedgerError::UserBusy => Self::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("account_busy"),
            // 业务冲突（§11.28 激活期内换套餐）：409 + 当前套餐码
            LedgerError::SubscriptionActive(plan_code) => {
                Self::new(StatusCode::CONFLICT, "subscription_active").with_param(plan_code)
            }
            LedgerError::Store(err) => Self::from(err),
            LedgerError::UserNotFound => {
                Self::new(StatusCode::NOT_FOUND, codes::NOT_FOUND).with_param("user_id")
            }
            // 账本故障 fail-closed：宁停不错账（IMPLEMENTATION §12.2）
            err => {
                tracing::error!(error = %err, "ledger error (fail-closed)");
                Self::internal()
            }
        }
    }
}

impl From<PricingError> for AppError {
    fn from(err: PricingError) -> Self {
        match err {
            PricingError::UnknownModel(_) => {
                Self::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND)
            }
            PricingError::MissingServerToolUsage | PricingError::InvalidServerToolAdmission(_) => {
                Self::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
            }
            PricingError::UnpricedServerTool => {
                Self::new(StatusCode::BAD_REQUEST, codes::SERVER_TOOL_UNPRICED).with_param("tools")
            }
            PricingError::UnknownGroup(_) => {
                tracing::error!(error = %err, "pricing group missing");
                Self::internal()
            }
            PricingError::Overflow | PricingError::Internal(_) | PricingError::InvalidUsage(_) => {
                tracing::error!(error = %err, "pricing error (fail-closed)");
                Self::internal()
            }
        }
    }
}

/// 成功响应挂 `x-okapi-request-id`。
///
/// 错误路径由 `into_response_with` 负责（本文件上方三处）；成功路径此前各端点各写
/// 一份私有副本，images / videos 干脆没写——于是这两个**计费**端点扣了钱却不给
/// request_id，用户看到账单对不回是哪次调用。收成一份共用的。
#[must_use]
pub fn with_request_id(mut resp: Response, request_id: uuid::Uuid) -> Response {
    if let Ok(value) = axum::http::HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut().insert("x-okapi-request-id", value);
    }
    resp
}

#[cfg(test)]
mod retry_after_tests {
    use super::*;

    #[test]
    fn all_protocols_report_the_fixed_window_reset_and_do_not_guess_quota_resets() {
        for axis in [
            "rpm",
            "tpm",
            "model_rpm",
            "group_rpm",
            "group_rph",
            "rpd",
            "token_count_rpd",
        ] {
            let error = || {
                AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(axis)
            };
            for response in [
                error().into_response_with(None),
                error().into_anthropic_response_with(None),
                error().into_gemini_response_with(None),
            ] {
                let seconds: i64 = response.headers()["retry-after"]
                    .to_str()
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!((1..=86_400).contains(&seconds));
            }
        }
        for (code, axis) in [
            (codes::RATE_LIMITED, "concurrency"),
            (codes::KEY_QUOTA_EXCEEDED, "rpm"),
        ] {
            let response = AppError::new(StatusCode::TOO_MANY_REQUESTS, code)
                .with_param(axis)
                .into_response_with(None);
            assert!(!response.headers().contains_key("retry-after"));
        }
    }
}
