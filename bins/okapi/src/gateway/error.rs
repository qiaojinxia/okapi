use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use okapi_api::{ErrorBody, codes};
use okapi_ledger::LedgerError;
use okapi_pricing::PricingError;
use okapi_store::StoreError;
use uuid::Uuid;

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
        if let Some(id) = request_id
            && let Ok(value) = axum::http::HeaderValue::from_str(&id.to_string())
        {
            resp.headers_mut().insert("x-okapi-request-id", value);
        }
        resp
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
        if let Some(id) = request_id
            && let Ok(value) = axum::http::HeaderValue::from_str(&id.to_string())
        {
            resp.headers_mut().insert("x-okapi-request-id", value);
        }
        resp
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        self.into_response_with(None)
    }
}

impl From<StoreError> for AppError {
    fn from(err: StoreError) -> Self {
        tracing::error!(error = %err, "store error");
        Self::internal()
    }
}

impl From<LedgerError> for AppError {
    fn from(err: LedgerError) -> Self {
        // 账本故障 fail-closed：宁停不错账（IMPLEMENTATION §12.2）
        tracing::error!(error = %err, "ledger error (fail-closed)");
        Self::internal()
    }
}

impl From<PricingError> for AppError {
    fn from(err: PricingError) -> Self {
        match err {
            PricingError::UnknownModel(_) => {
                Self::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND)
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
