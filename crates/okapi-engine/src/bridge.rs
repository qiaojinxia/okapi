//! Control-plane to data-plane bridge trait and optional remote HTTP implementation.
//!
//! - 单机 / `okapi all` 与缺省部署：由 `okapi-gateway` 注册进程内 `GatewayDataPlane`；
//! - 独立 `okapi console`（配置 `OKAPI_GATEWAY_INTERNAL_URL`）：使用 [`HttpDataPlaneBridge`]
//!   经内网流式 HTTP 中继 `/v1/chat/completions` 与 `/v1/responses`，跳过本地 tokenizer 预热，
//!   使控制面与数据面可独立扩缩容与滚动升级。

use crate::channel_test;
use crate::ingress::Ingress;
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use okapi_api::codes;
use okapi_domain::{GroupCode, ModelCode};
use okapi_kernel::error::AppError;
use okapi_providers::model_parameters::{self, ParameterProfile};
use okapi_providers::reasoning::ReasoningDirective;
use std::sync::Arc;
use std::time::Duration;

/// Data-plane entrypoints invoked by control-plane relay endpoints.
#[async_trait::async_trait]
pub trait DataPlaneBridge: Send + Sync {
    async fn chat_completions(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response;

    async fn responses(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response;

    async fn messages(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response;

    async fn playground_parameters(
        &self,
        state: &AppState,
        headers: &HeaderMap,
        model: &str,
    ) -> Result<ParameterProfile, AppError>;
}

/// 进程内 60s 缓存的模型解析（`gateway` 与 `console` 的 `playground_parameters` 共用）。
pub async fn resolve_model_cached(
    state: &AppState,
    name: &str,
) -> Result<Arc<Option<okapi_store::channels::ResolvedModel>>, AppError> {
    if let Some(hit) = state.routing.model_cache.get(name).await {
        return Ok(hit);
    }
    let resolved = Arc::new(okapi_store::channels::resolve_model(&state.pg, name).await?);
    state
        .routing
        .model_cache
        .insert(name.to_owned(), Arc::clone(&resolved))
        .await;
    Ok(resolved)
}

/// 模型解析 + 修饰符（§4.4 / §11.25）：全名（含别名）直命中优先，未命中再按修饰符语法剥基名重试。
pub async fn resolve_with_directive(
    state: &AppState,
    requested: &str,
) -> Result<
    Option<(
        okapi_store::channels::ResolvedModel,
        Option<ReasoningDirective>,
        Option<String>,
    )>,
    AppError,
> {
    let has_modifier_sep = requested.contains('@');
    if !has_modifier_sep && let Some(meta) = resolve_model_cached(state, requested).await?.as_ref()
    {
        return Ok(Some((meta.clone(), None, None)));
    }
    if let Some(m) = okapi_providers::modifiers::split_model_modifiers(requested)
        && let Some(meta) = resolve_model_cached(state, m.base()).await?.as_ref()
    {
        let variant = m.canonical_name();
        let variant = variant
            .split_once('@')
            .map_or(variant.clone(), |(_, rest)| {
                format!("{}@{rest}", meta.canonical)
            });
        return Ok(Some((meta.clone(), m.reasoning(), Some(variant))));
    }
    Ok(None)
}

/// 只读 Playground 参数契约计算：与真实请求同口径（身份 / 规范模型 / 分组 / 池链 / 负毛利熔断），
/// 无预扣、无限流计数、无上游调用；进程内与远程 HTTP 桥接共用。
pub async fn playground_parameters(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
) -> Result<ParameterProfile, AppError> {
    let key = okapi_kernel::auth::authenticate(state, headers).await?;
    let Some((meta, _, _)) = resolve_with_directive(state, model).await? else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND));
    };
    if !key.allows_model(&meta.canonical) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::MODEL_NOT_ALLOWED,
        ));
    }
    let book = state.pricebook.load();
    if !book.has_model(&ModelCode::from(meta.canonical.as_str()))
        || !book.has_group(&GroupCode::from(key.group_code.as_str()))
    {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND));
    }
    let mut candidates = okapi_store::channels::candidates_for_model(
        &state.pg,
        &meta.canonical,
        &key.pool_chain(),
        state.master_key.as_deref(),
    )
    .await?;
    candidates.retain(|c| Ingress::OpenAi.accepts(c, &meta.canonical));
    state
        .retain_margin_ok(&key.group_code, &mut candidates)
        .await;
    let mut common: Option<ParameterProfile> = None;
    for c in &candidates {
        let upstream = c.upstream_model(&meta.canonical);
        let dialect = crate::dialect::upstream_dialect(&c.provider, upstream);
        let mut p = model_parameters::profile(dialect, upstream, c.api_base.as_deref());
        let controlled = |field: &str| {
            c.strip_request_fields.iter().any(|f| f == field)
                || c.inject_request_fields.contains_key(field)
        };
        if controlled("temperature")
            || c.capabilities
                .get("temperature")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
        {
            p.temperature_max = None;
        }
        if controlled("top_p") {
            p.top_p = false;
        }
        if controlled("reasoning_effort")
            || controlled("reasoning")
            || controlled("thinking")
            || controlled("output_config")
            || controlled("generationConfig")
            || c.capabilities
                .get("reasoning")
                .and_then(serde_json::Value::as_bool)
                == Some(false)
        {
            p.efforts.clear();
            p.budget_min = None;
            p.budget_max = None;
        }
        match &mut common {
            Some(profile) => profile.intersect(&p),
            None => common = Some(p),
        }
    }
    common
        .ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL))
}

/// 独立 `console` 角色使用的流式 HTTP 数据面桥接器：将中继请求转发至内网 `gateway` 服务。
#[derive(Clone)]
pub struct HttpDataPlaneBridge {
    base_url: Arc<str>,
    client: reqwest::Client,
}

const FORWARD_REQ_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "x-goog-api-key",
    "cookie",
    "content-type",
    "accept",
    "user-agent",
    "traceparent",
    "session-id",
    "session_id",
    "x-session-id",
    "x-forwarded-for",
    "x-real-ip",
    "x-okapi-edge-key",
];

const FORWARD_RESP_HEADERS: &[&str] = &[
    "content-type",
    "cache-control",
    "x-accel-buffering",
    "x-okapi-request-id",
    "traceparent",
    "retry-after",
];

impl HttpDataPlaneBridge {
    pub fn new(base_url: &str) -> anyhow::Result<Self> {
        let trimmed = base_url.trim().trim_end_matches('/');
        let parsed = reqwest::Url::parse(trimmed)
            .map_err(|err| anyhow::anyhow!("invalid gateway_internal_url: {err}"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
            "gateway_internal_url must be an http/https URL with a host"
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(32)
            .build()
            .map_err(|err| anyhow::anyhow!("http data-plane bridge client: {err}"))?;
        Ok(Self {
            base_url: Arc::from(trimmed),
            client,
        })
    }

    async fn relay_post(
        &self,
        state: &AppState,
        path: &str,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Response {
        let url = format!("{}{path}", self.base_url);
        let mut req = self.client.post(&url);
        let mut has_content_type = false;
        for &name in FORWARD_REQ_HEADERS {
            if let Some(val) = headers.get(name) {
                if name == "content-type" {
                    has_content_type = true;
                }
                req = req.header(name, val.clone());
            }
        }
        if !has_content_type {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        if let Some(pin) = channel_test::current() {
            let Some(secret) = channel_test::signing_secret(state) else {
                return AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::OVERLOADED)
                    .with_param("channel_pin_secret_required")
                    .into_response_with(None);
            };
            let signed = channel_test::encode_signed_header(&pin, &secret);
            req = req.header(channel_test::INTERNAL_PIN_HEADER, signed);
        }
        let upstream = match req.body(body).send().await {
            Ok(resp) => resp,
            Err(error) => {
                tracing::warn!(%error, url, "remote data-plane bridge request failed");
                return AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::OVERLOADED)
                    .with_param("data_plane_unreachable")
                    .into_response_with(None);
            }
        };
        let status = upstream.status();
        let mut out_headers = HeaderMap::new();
        for &name in FORWARD_RESP_HEADERS {
            if let Some(val) = upstream.headers().get(name)
                && let Ok(hname) = HeaderName::from_bytes(name.as_bytes())
                && let Ok(hval) = HeaderValue::from_bytes(val.as_bytes())
            {
                out_headers.insert(hname, hval);
            }
        }
        let body = Body::from_stream(upstream.bytes_stream());
        let mut resp = (status, body).into_response();
        resp.headers_mut().extend(out_headers);
        resp
    }
}

#[async_trait::async_trait]
impl DataPlaneBridge for HttpDataPlaneBridge {
    async fn chat_completions(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response {
        self.relay_post(&state, "/v1/chat/completions", &headers, body)
            .await
    }

    async fn responses(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response {
        self.relay_post(&state, "/v1/responses", &headers, body)
            .await
    }

    async fn messages(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response {
        self.relay_post(&state, "/v1/messages", &headers, body)
            .await
    }

    async fn playground_parameters(
        &self,
        state: &AppState,
        headers: &HeaderMap,
        model: &str,
    ) -> Result<ParameterProfile, AppError> {
        playground_parameters(state, headers, model).await
    }
}
