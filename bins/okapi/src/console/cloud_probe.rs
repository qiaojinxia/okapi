//! bedrock / vertex 渠道的测活与模型发现（IMPLEMENTATION §11.35）。
//!
//! 这两家的请求要签名 / 换 token，走不了 `PassUpstream` 那条"拼 URL + 一个鉴权头"的通用探测，
//! 直接用各自的上游客户端。结果形状与 `admin::probe_channel` 完全一致，前端不感知。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::http::StatusCode;
use bytes::Bytes;
use okapi_providers::UpstreamError;
use okapi_providers::anthropic::MessagesResponse;
use okapi_providers::gemini::GeminiResponse;
use serde_json::{Value, json};

/// 探测补全的输出上限（与通用探测同值）。
const PROBE_MAX_TOKENS: u32 = 16;

pub(super) fn is_cloud(provider: &str) -> bool {
    matches!(provider, "bedrock" | "vertex" | "anthropic_max" | "codex")
}

/// 测活：`model = None` 只验凭证（bedrock：SigV4 列基础模型 / API key 列兼容模型；vertex：换 token；
/// anthropic_max / codex：token 未过期或能成功刷新），`model = Some` 真发一次 16 token 补全。
/// 返回与通用探测同形状的 JSON。
// 四家 × 两种探测范围的分派放同一视野
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) async fn probe(
    state: &AppState,
    provider: &str,
    base: &str,
    settings: &Value,
    credential: &str,
    channel_key_id: i64,
    upstream_model: Option<&str>,
) -> Value {
    let outbound = okapi_providers::Outbound::from_settings(settings);
    let region = settings
        .get("aws_region")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let oauth_key = crate::gateway::oauth_cred::OAuthKey {
        channel_key_id,
        provider,
        token_url: settings
            .get("oauth_token_url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty()),
        proxy_url: outbound.proxy_url.as_deref(),
    };
    let started = std::time::Instant::now();
    let outcome: Result<u16, UpstreamError> = match (provider, upstream_model) {
        ("anthropic_max" | "codex", None) => {
            crate::gateway::oauth_cred::fresh_credential_for(state, &oauth_key, credential)
                .await
                .map(|_| 200)
        }
        ("anthropic_max", Some(m)) => {
            match crate::gateway::oauth_cred::fresh_credential_for(state, &oauth_key, credential)
                .await
            {
                Ok(cred) => {
                    let body = Bytes::from(
                        json!({"model": m, "max_tokens": PROBE_MAX_TOKENS,
                               "messages": [{"role": "user", "content": "ping"}]})
                        .to_string(),
                    );
                    okapi_providers::oauth::anthropic_max::messages(
                        state.anthropic.http(),
                        base,
                        &cred.access_token,
                        body,
                        false,
                        &outbound,
                    )
                    .await
                    .map(|r| messages_status(&r))
                }
                Err(err) => Err(err),
            }
        }
        ("codex", Some(m)) => {
            match crate::gateway::oauth_cred::fresh_credential_for(state, &oauth_key, credential)
                .await
            {
                Ok(cred) => {
                    let body = Bytes::from(
                        json!({"model": m, "input": "ping", "max_output_tokens": PROBE_MAX_TOKENS})
                            .to_string(),
                    );
                    okapi_providers::oauth::codex::responses(
                        state.upstream.http(),
                        base,
                        &cred.access_token,
                        cred.account_id.as_deref(),
                        body,
                        false,
                        &outbound,
                    )
                    .await
                    .map(|r| match r {
                        okapi_providers::ChatResponse::Json { status, .. } => status,
                        okapi_providers::ChatResponse::Stream(_) => 200,
                    })
                }
                Err(err) => Err(err),
            }
        }
        ("bedrock", None) => {
            if okapi_providers::aws_sigv4::AwsCredentials::parse(credential).is_some() {
                state
                    .bedrock
                    .list_foundation_models(base, region, credential, &outbound)
                    .await
                    .map(|_| 200)
            } else {
                state
                    .bedrock
                    .list_openai_models(base, credential, &outbound)
                    .await
            }
        }
        ("bedrock", Some(m)) => state
            .bedrock
            .messages(
                base,
                region,
                credential,
                m,
                anthropic_ping(),
                false,
                &outbound,
            )
            .await
            .map(|r| messages_status(&r)),
        ("vertex", None) => state
            .vertex
            .access_token(credential, &outbound)
            .await
            .map(|_| 200),
        ("vertex", Some(m)) if okapi_providers::vertex::is_anthropic_model(m) => state
            .vertex
            .messages(base, credential, m, anthropic_ping(), false, &outbound)
            .await
            .map(|r| messages_status(&r)),
        ("vertex", Some(m)) => state
            .vertex
            .generate(base, credential, m, gemini_ping(), false, &outbound)
            .await
            .map(|r| match r {
                GeminiResponse::Json { status, .. } => status,
                GeminiResponse::Stream(_) => 200,
            }),
        _ => Err(UpstreamError::Build("provider".to_owned())),
    };
    let latency_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let at = chrono::Utc::now().to_rfc3339();
    let scope = if upstream_model.is_some() {
        "model"
    } else {
        "credential"
    };
    match outcome {
        Ok(status) => json!({"ok": true, "http_status": status, "latency_ms": latency_ms, "at": at,
                              "scope": scope, "model": upstream_model}),
        Err(UpstreamError::Status { status, body, .. }) => {
            let detail: String = String::from_utf8_lossy(&body).chars().take(300).collect();
            json!({"ok": false, "http_status": status, "latency_ms": latency_ms, "at": at,
                   "scope": scope, "model": upstream_model, "upstream_body": detail})
        }
        Err(err) => json!({"ok": false, "error_code": err.error_code(), "latency_ms": latency_ms,
                           "at": at, "scope": scope, "model": upstream_model}),
    }
}

/// 模型发现：bedrock 走控制面 ListFoundationModels（只有 SigV4 凭证能列）；vertex 没有稳定的
/// 公开列表接口，明确回不支持。
pub(super) async fn fetch_models(
    state: &AppState,
    provider: &str,
    base: &str,
    settings: &Value,
    credential: &str,
) -> Result<Vec<String>, AppError> {
    // vertex 没有稳定的公开列表接口；订阅登录（anthropic_max / codex）没有模型列表面
    if provider != "bedrock" {
        return Err(AppError::bad_request().with_param("fetch_models_unsupported"));
    }
    if okapi_providers::aws_sigv4::AwsCredentials::parse(credential).is_none() {
        return Err(AppError::bad_request().with_param("fetch_models_requires_sigv4"));
    }
    let outbound = okapi_providers::Outbound::from_settings(settings);
    let region = settings
        .get("aws_region")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut models = state
        .bedrock
        .list_foundation_models(base, region, credential, &outbound)
        .await
        .map_err(|err| match err {
            UpstreamError::Status { status, .. } => {
                AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                    .with_param(format!("status_{status}"))
            }
            other => AppError::new(StatusCode::BAD_GATEWAY, okapi_api::codes::UPSTREAM_ERROR)
                .with_param(other.error_code()),
        })?;
    models.sort();
    models.dedup();
    Ok(models)
}

fn messages_status(resp: &MessagesResponse) -> u16 {
    match resp {
        MessagesResponse::Json { status, .. } => *status,
        MessagesResponse::Stream(_) => 200,
    }
}

fn anthropic_ping() -> Bytes {
    Bytes::from(
        json!({"max_tokens": PROBE_MAX_TOKENS,
               "messages": [{"role": "user", "content": "ping"}]})
        .to_string(),
    )
}

fn gemini_ping() -> Bytes {
    Bytes::from(
        json!({"contents": [{"parts": [{"text": "ping"}]}],
               "generationConfig": {"maxOutputTokens": PROBE_MAX_TOKENS}})
        .to_string(),
    )
}
