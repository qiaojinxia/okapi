//! count_tokens：本地估算或转给上游 tokenizer（不计费）。
use super::super::estimate::estimate_prompt_tokens;
use super::super::ingress::Ingress;
use super::super::scheduler::order_candidates;
use super::super::state::AppState;
use super::{
    DEFAULT_ANTHROPIC_BASE, non_json_failure, non_json_kind, parse_checked, resolve_model_cached,
};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use okapi_api::{MessagesRequestProbe, codes};
use okapi_kernel::error::AppError;
use okapi_providers::rewrite_model;
use okapi_store::ChannelCandidate;
use std::time::Duration;
use uuid::Uuid;

/// Anthropic `POST /v1/messages/count_tokens`：鉴权 + 模型可见，**不计费**。
/// 有 anthropic 候选则代理上游 tokenizer（一个被拒就换下一个）；没有候选才本地估算（Claude Code 缺此端点会退本地）。
pub async fn messages_count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4();
    match tokio::time::timeout(
        Duration::from_mins(1),
        count_tokens_inner(&state, &headers, body),
    )
    .await
    .unwrap_or_else(|_| {
        Err(AppError::new(
            StatusCode::GATEWAY_TIMEOUT,
            codes::UPSTREAM_TIMEOUT,
        ))
    }) {
        Ok(resp) => resp,
        Err(err) => err.into_anthropic_response_with(Some(request_id)),
    }
}

/// count_tokens 不带 metadata：仅限真机的渠道只认 UA，不收就退本地估算。
pub(super) fn counts_for(c: &ChannelCandidate, headers: &HeaderMap) -> bool {
    !okapi_providers::profiles::native_only(&c.extensions)
        || user_agent(headers).is_some_and(|ua| ua.starts_with("claude-cli/"))
}

pub(super) fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
}

pub(super) async fn count_tokens_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = super::super::auth::authenticate_data_plane(state, headers).await?;
    let _permit =
        okapi_kernel::sched_redis::token_count::CountPermit::acquire(&state.sched, &key).await?;
    // 不计费，但有 anthropic 候选时会拿渠道凭证打上游 tokenizer：按分组窗限速（§11.32）。
    // 不过 check_member_limit——那是月度消费上限，不该挡住不花钱的调用
    super::super::auth::check_group_rate(state, &key).await?;
    let probe: MessagesRequestProbe = parse_checked(Ingress::Anthropic, &body)?;
    let meta = resolve_model_cached(state, &probe.model).await?;
    let Some(meta) = meta.as_ref() else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND));
    };
    let canonical = meta.canonical.clone();
    if !key.allows_model(&canonical) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::MODEL_NOT_ALLOWED,
        ));
    }
    let rows = okapi_store::channels::candidates_for_model(
        &state.pg,
        &canonical,
        &key.pool_chain(),
        state.master_key.as_deref(),
    )
    .await
    .map_err(AppError::from)?;
    // 被挡住的候选（账号准入：本地 token 上限、上游额度窗，与主链路同一套；key RPM；渠道并发）
    // 换下一个——不计费的旁路同样占用账号，同模型下别的 key 空闲时不该整体 429；
    // 能用的候选都挡在限速上才报限速，全被账号准入挡住（或根本没有 anthropic 候选）就退本地估算
    let mut limited = None;
    // 上游拒绝（账号所在组织不许 OAuth 的 403、beta 被拒的 400……）就换下一个候选，最多试三个；
    // 全部失败且都是「没有这个端点」（404 / 405 / 501）才回 501：真机 Claude Code 对网关的 501 会改用一次 1 token 的采样来数
    let mut failed = false;
    let mut unsupported = true;
    let mut attempts = 0;
    let candidates = order_candidates(rows)
        .into_iter()
        .filter(|c| matches!(c.provider.as_str(), "anthropic" | "anthropic_max"))
        .filter(|c| counts_for(c, headers));
    for cand in candidates {
        if attempts == MAX_COUNT_ATTEMPTS {
            break;
        }
        if super::super::account_control::admit(state, cand.channel_id, Some(cand.channel_key_id))
            .await
            .is_err()
        {
            continue;
        }
        if !super::super::key_gate::rpm_ok(state, &cand).await {
            limited = Some(super::super::key_gate::Closed::Rpm.param());
            continue;
        }
        let Some(_slot) =
            okapi_kernel::sched_redis::token_count::ChannelPermit::acquire(&state.sched, &cand)
                .await
                .map_err(|error| super::super::account_control::attempt_error(&error))?
        else {
            limited = Some("channel_concurrency");
            continue;
        };
        attempts += 1;
        match count_via_upstream(state, headers, &key, &cand, &canonical, &probe.model, &body).await
        {
            Ok(response) => return Ok(response),
            Err(CountFailure::Request(error)) => return Err(error),
            Err(CountFailure::Upstream {
                unsupported: is_unsupported,
            }) => {
                failed = true;
                unsupported &= is_unsupported;
            }
        }
    }
    if failed {
        return Err(if unsupported {
            AppError::new(StatusCode::NOT_IMPLEMENTED, codes::UPSTREAM_ERROR)
                .with_param("input_tokens_unsupported")
        } else {
            AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
        });
    }
    if let Some(param) = limited {
        return Err(
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(param),
        );
    }
    let tokens =
        estimate_prompt_tokens(&probe.model, &probe.prompt_segments(), probe.messages.len());
    Ok(axum::Json(serde_json::json!({ "input_tokens": tokens })).into_response())
}

/// 一个候选没数成的原因：请求本身有问题（不必换候选）/ 上游拒绝（换下一个；`unsupported` = 上游说没有这个端点）。
pub(super) enum CountFailure {
    Request(AppError),
    Upstream { unsupported: bool },
}

/// 一次 count_tokens 最多试几个候选。
const MAX_COUNT_ATTEMPTS: usize = 3;

/// 用选中的 anthropic 候选的凭证打上游 tokenizer（调用方持有渠道并发许可）。
pub(super) async fn count_via_upstream(
    state: &AppState,
    headers: &HeaderMap,
    key: &okapi_store::AuthedKey,
    cand: &ChannelCandidate,
    canonical: &str,
    requested_model: &str,
    body: &Bytes,
) -> Result<Response, CountFailure> {
    let upstream_model = cand.upstream_model(canonical).to_owned();
    let body_up = rewrite_model(body, requested_model, &upstream_model)
        .map_err(|_| CountFailure::Request(AppError::bad_request()))?;
    let base = cand
        .api_base
        .clone()
        .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE.to_owned());
    let client = super::super::oauth_cred::client_headers(headers);
    let outbound = super::super::oauth_cred::outbound_with_client(cand, &client, Some(key.user_id));
    let counted = if cand.provider == "anthropic_max" {
        match super::super::oauth_cred::fresh_credential(state, cand).await {
            Ok(cred) => {
                okapi_providers::oauth::anthropic_max::count_tokens(
                    state.upstreams.anthropic.http(),
                    &base,
                    &cred.access_token,
                    body_up,
                    &outbound,
                    cred.account_id.as_deref(),
                )
                .await
            }
            Err(err) => Err(err),
        }
    } else {
        state
            .upstreams
            .anthropic
            .count_tokens(&base, &cand.credential, body_up, &outbound)
            .await
    };
    // 上游 tokenizer 回了网页之类不是 JSON 的体：别替它编一个 `input_tokens: 0` 回给客户端
    // （客户端会据此以为上下文是空的），按上游失败处理，客户端自己会退本地估算。
    let counted = counted.and_then(|up| match non_json_kind(&up) {
        Some(kind) => Err(non_json_failure(kind, up.len())),
        None => serde_json::from_slice::<serde_json::Value>(
            up.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&up),
        )
        .map_err(|_| non_json_failure("malformed_json", up.len())),
    });
    match counted {
        Ok(value) => Ok(axum::Json(value).into_response()),
        Err(error) => {
            log_count_upstream_failure(cand, &error);
            Err(CountFailure::Upstream {
                unsupported: matches!(error.upstream_status(), Some(404 | 405 | 501)),
            })
        }
    }
}

/// 上游的拒绝原因只在这里看得到（客户端只拿到 502）。
pub(super) fn log_count_upstream_failure(
    cand: &okapi_store::ChannelCandidate,
    error: &okapi_providers::UpstreamError,
) {
    let detail = match error {
        okapi_providers::UpstreamError::Status { body, .. } => {
            String::from_utf8_lossy(&body[..body.len().min(300)]).into_owned()
        }
        // 只认我们自己写的「体不是 JSON」说明（种类 + 长度）；别的 Stream 原因可能带 URL，不进日志
        okapi_providers::UpstreamError::Stream(reason)
            if reason.starts_with("upstream_not_json") =>
        {
            reason.clone()
        }
        _ => String::new(),
    };
    tracing::warn!(
        channel_id = cand.channel_id,
        channel_key_id = cand.channel_key_id,
        error = %error,
        detail,
        "count_tokens upstream failed"
    );
}
