//! failover 主循环：逐个候选尝试、401 强刷、429 冷却、耗尽后的回复与致命错误分类。
use super::super::error::AppError;
use super::super::execution_plan::{ExecutionPlan, Requirements};
use super::super::ingress::Ingress;
use super::{
    AttemptError, FailureReply, ForwardFailure, MAX_ATTEMPTS, ProbeInfo, RequestBilling,
    attempt_json, attempt_stream, build_upstream_body, eligible_candidates, failure_kind_of,
    lost_tools, recovery_secs, upstream_endpoint,
};
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use okapi_api::codes;
use okapi_providers::UpstreamError;
use okapi_providers::convert::tool_loss::ToolLossPolicy;
use okapi_store::channels::KeyFailure;
use std::time::Duration;

// failover 主循环：候选过滤/粘性/信号量/状态机联动的完整语义在同一视野内更可读
#[allow(clippy::too_many_lines)]
pub(super) async fn try_model(
    bill: &RequestBilling,
    info: &ProbeInfo,
    body: &Bytes,
) -> Result<Response, ForwardFailure> {
    let (candidates, sticky_key) = eligible_candidates(bill, info, false).await?;

    let mut failover: i16 = 0;
    let mut attempted = 0usize;
    let mut last_code: &'static str = codes::UPSTREAM_ERROR;
    let mut last_status: Option<i16> = None;
    let mut last_channel: Option<(i64, i64)> = None;
    let mut last_upstream = None;
    let mut last_kind: Option<KeyFailure> = None;
    // 次数用尽或请求不许改投：候选里可能还有好的，与「整个池都试过了」是两回事（见 `exhausted_reply`）
    let mut stopped_early = false;

    for mut cand in candidates {
        if attempted >= MAX_ATTEMPTS {
            stopped_early = true;
            break;
        }
        // key 级 RPM / 当日消费闸：挡下的候选不算一次尝试
        if let Err(closed) = super::super::key_gate::check(&bill.state, &cand).await {
            tracing::debug!(
                channel_key = cand.channel_key_id,
                gate = closed.param(),
                "key 闸已关，跳过候选"
            );
            continue;
        }
        attempted += 1;

        let upstream_model = cand.upstream_model(&bill.model).to_owned();
        let mut body_up = match build_upstream_body(bill, info, &cand, body, &upstream_model) {
            Ok(body_up) => body_up,
            Err(err) => {
                let rejection = match &err {
                    UpstreamError::Build(reason) => build_rejection(reason),
                    _ => AppError::bad_request(),
                };
                return Err(ForwardFailure::app(
                    rejection,
                    failover,
                    Some((cand.channel_id, cand.channel_key_id)),
                ));
            }
        };
        let base = cand.api_base.clone().unwrap_or_else(|| {
            okapi_providers::registry::lookup(&cand.provider)
                .and_then(|adapter| adapter.default_base)
                .unwrap_or_default()
                .to_owned()
        });
        // 被并发 / 准入挡下的候选不算一次真实尝试，届时恢复成上一次真实失败的渠道与上游
        let previous_attempt = (last_channel, last_upstream.clone());
        last_channel = Some((cand.channel_id, cand.channel_key_id));
        last_upstream = Some((
            upstream_model.clone(),
            upstream_endpoint(&cand, bill.is_stream, bill.ingress).to_owned(),
        ));
        let sticky_layer: i16 = if bill.response_parent.is_some() {
            1
        } else if sticky_key == Some(cand.channel_key_id) {
            2
        } else {
            3
        };

        // §3.6：连接/超时/5xx 允许同 key 先重试；次数按渠道配（缺省 1，空回复直接换渠道）
        let same_key_retries = cand.same_key_retries;
        let mut retry: i16 = 0;
        let oauth = super::super::oauth_cred::is_oauth_provider(&cand.provider);
        let mut refreshed_401 = false;
        let mut dropped_reasoning = false;
        let attempt = loop {
            // Keep the candidate equal to the token actually sent; a forced refresh can
            // then distinguish a rejected token from one another request just rotated.
            if oauth {
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(bill.started + Duration::from_mins(8)),
                    super::super::oauth_cred::fresh_credential(&bill.state, &cand),
                )
                .await
                .unwrap_or(Err(UpstreamError::Timeout))
                {
                    Ok(credential) => cand.credential = credential.to_plaintext(),
                    Err(err) => {
                        break Err(classify_fatal(
                            super::super::oauth_cred::unavailable(err),
                            failover,
                            (cand.channel_id, cand.channel_key_id),
                        ));
                    }
                }
            }

            bill.trace.begin(
                &cand,
                &upstream_model,
                upstream_endpoint(&cand, bill.is_stream, bill.ingress),
            );
            let attempt = async {
                if info.stream {
                    attempt_stream(
                        bill,
                        &cand,
                        &base,
                        body_up.clone(),
                        failover,
                        sticky_layer,
                        retry,
                    )
                    .await
                } else {
                    attempt_json(
                        bill,
                        &cand,
                        &base,
                        body_up.clone(),
                        failover,
                        sticky_layer,
                        retry,
                    )
                    .await
                }
            };
            let mut result = tokio::time::timeout_at(
                tokio::time::Instant::from_std(bill.started + Duration::from_mins(8)),
                attempt,
            )
            .await
            .unwrap_or_else(|_| {
                Err(AttemptError::Retriable {
                    code: codes::UPSTREAM_TIMEOUT,
                    upstream_status: None,
                    failure_kind: KeyFailure::Request,
                })
            });
            if oauth
                && matches!(
                    &result,
                    Err(AttemptError::Retriable {
                        upstream_status: Some(401),
                        ..
                    })
                )
            {
                if !refreshed_401 {
                    refreshed_401 = true;
                    let refreshed = tokio::time::timeout_at(
                        tokio::time::Instant::from_std(bill.started + Duration::from_mins(8)),
                        super::super::oauth_cred::refresh_rejected_credential(&bill.state, &cand),
                    )
                    .await
                    .unwrap_or(Err(UpstreamError::Timeout));
                    match refreshed {
                        Ok(credential) => {
                            cand.credential = credential.to_plaintext();
                            retry = retry.saturating_add(1);
                            continue;
                        }
                        Err(err) => bill.trace.failure(&err),
                    }
                }
                if let Err(AttemptError::Retriable { failure_kind, .. }) = &mut result {
                    // invalid_grant already persisted status=6; store-side status guards
                    // preserve it. A recoverable OAuth 401 only needs a short cooldown.
                    *failure_kind = KeyFailure::RateLimited {
                        retry_after_secs: Some(30),
                    };
                }
            }
            if oauth
                && let Err(AttemptError::Retriable {
                    failure_kind: KeyFailure::RateLimited { retry_after_secs },
                    upstream_status: Some(429),
                    ..
                }) = &mut result
            {
                *retry_after_secs = Some(retry_after_secs.unwrap_or(5));
            }
            // 会话换了账号（额度切换、并发满、粘性过期）：历史里上个账号签发的加密推理项新账号
            // 验不过。去掉推理项在同一候选上重发一次——不计 failover、不标 key 失败；换别的
            // 账号也一样验不过。压缩项去不掉，仍被拒就把上游的 400 回给客户端。
            if bill.ingress == Ingress::Responses
                && !dropped_reasoning
                && matches!(
                    &result,
                    Err(AttemptError::Fatal(ForwardFailure {
                        reply: FailureReply::Upstream { status, body },
                        ..
                    })) if okapi_providers::responses::rejects_foreign_encrypted_content(*status, body)
                )
                && let Some(stripped) =
                    okapi_providers::responses::without_reasoning_items(&body_up)
            {
                dropped_reasoning = true;
                tracing::info!(
                    request_id = %bill.request_id,
                    channel_key = cand.channel_key_id,
                    "加密推理项不属于本账号，去掉推理项重发"
                );
                bill.trace
                    .set("dropped_reasoning", serde_json::Value::Bool(true));
                body_up = stripped;
                continue;
            }
            // Responses 直转撞上 404/405 = 这个上游根本没有 /responses（"openai" 渠道指着
            // 只实现了 chat 的第三方地址是常态）。同一候选就地改走降级链再来一次：
            // 不计 failover、不计 retry、不标 key 失败——渠道没坏，是方言不对。降级会丢工具而渠道的
            // tool_loss_policy 不接受时不降：宁可把上游的 404 报出去，也不静默丢掉客户端工具。
            if bill.ingress == Ingress::Responses
                && bill.response_parent.is_none()
                && cand.responses_native
                && ToolLossPolicy::parse(cand.tool_loss_policy.as_deref())
                    .admits(&lost_tools(info, bill))
                && ExecutionPlan::compile(bill.ingress, &cand, &bill.model, Requirements::default())
                    .is_ok_and(ExecutionPlan::can_fallback_to_chat)
                && matches!(
                    &result,
                    Err(AttemptError::Fatal(f)) if matches!(f.upstream_status, Some(404 | 405))
                )
            {
                tracing::info!(
                    request_id = %bill.request_id,
                    channel = cand.channel_id,
                    "上游无 /responses，本次改走降级链（可在渠道设置关闭 responses_native）"
                );
                cand.responses_native = false;
                match build_upstream_body(bill, info, &cand, body, &upstream_model) {
                    Ok(b) => {
                        body_up = b;
                        last_upstream = Some((
                            upstream_model.clone(),
                            upstream_endpoint(&cand, bill.is_stream, bill.ingress).to_owned(),
                        ));
                        continue;
                    }
                    Err(_) => break result,
                }
            }
            let transient = matches!(
                &result,
                Err(AttemptError::Retriable {
                    failure_kind: KeyFailure::Transient | KeyFailure::Request | KeyFailure::Unreachable { .. },
                    code,
                    upstream_status,
                }) if *code != codes::EMPTY_COMPLETION && *code != codes::NO_AVAILABLE_CHANNEL && matches!(upstream_status, None | Some(408 | 500..=528 | 530..=599))
            );
            if retry < same_key_retries && transient {
                retry += 1;
                tracing::debug!(
                    channel_key = cand.channel_key_id,
                    retry,
                    same_key_retries,
                    "瞬态失败，同 key 重试"
                );
                continue;
            }
            break result;
        };

        match attempt {
            Ok(resp) => {
                // 成功建立输出：刷新会话粘性映射；响应持有并发许可直到 EOF/drop。
                // 账号测试是钉住的，不该改写管理员真实会话的粘性。
                if let Some(session) = bill.session.as_ref().filter(|_| bill.pin.is_none()) {
                    bill.state
                        .sched
                        .sticky_set(bill.user_id, session, cand.channel_key_id)
                        .await;
                }
                super::super::key_health::success(&bill.state, &cand).await;
                return Ok(resp);
            }
            Err(AttemptError::Retriable {
                code,
                upstream_status,
                failure_kind,
            }) => {
                if code == codes::NO_AVAILABLE_CHANNEL {
                    // Admission denied before the wire attempt. Busy candidates
                    // neither consume retry budget nor change credential health.
                    attempted = attempted.saturating_sub(1);
                    if attempted > 0 {
                        // 之前已有真实的上游失败：最终仍报它的错误码 / 状态 / 渠道。后面的候选全忙只说明
                        // 没法再改投，不是失败原因（曾被覆盖成 503 no_available_channel，盖住真实的 5xx）。
                        (last_channel, last_upstream) = previous_attempt;
                    } else {
                        last_code = code;
                    }
                    continue;
                }
                tracing::warn!(
                    request_id = %bill.request_id,
                    channel_key = cand.channel_key_id,
                    code,
                    "首字前失败，failover 下一候选"
                );
                super::super::key_health::failure(&bill.state, &cand, code, failure_kind).await;
                last_code = code;
                last_status = upstream_status;
                last_kind = Some(failure_kind);
                // allow_fallbacks:false（§11.24）——失败即返回，不改投其它渠道。
                //
                // 先判再自增：`failover_count` 记的是"这次请求换了几回渠道"，我们**拒绝**
                // 改投时一次也没换，记成 1 会把分析面的 failover 指标虚高一截。
                // key 状态机照常登记（上面的 mark_key_failure 已做）：这条渠道确实出过
                // 问题，不能因为调用方不要 failover 就当没发生。
                if !bill.prefs.allow_fallbacks || bill.response_parent.is_some() {
                    tracing::debug!(
                        request_id = %bill.request_id,
                        "请求声明 allow_fallbacks=false，不再改投"
                    );
                    stopped_early = true;
                    break;
                }
                failover = failover.saturating_add(1);
            }
            Err(AttemptError::Fatal(mut failure)) => {
                failure.failover_count = failover;
                failure.upstream = last_upstream.clone().map(Box::new);
                return Err(failure);
            }
        }
    }

    if attempted == 0 {
        // 全部候选并发满：忙碌但非故障
        return Err(ForwardFailure::app(
            AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL),
            0,
            last_channel,
        ));
    }

    let err_code = last_code;
    let reply = exhausted_reply(bill, err_code, last_status, last_kind, stopped_early).await;
    let mut failure = ForwardFailure::app(reply, failover, last_channel);
    // 请求日志里仍记真实的上游失败原因；客户端看到的状态与它可以不同（见 `exhausted_reply`）
    err_code.clone_into(&mut failure.error_code);
    failure.upstream_status = last_status;
    failure.upstream = last_upstream.map(Box::new);
    Err(failure)
}

/// 所有尝试都在首字前失败后给客户端的回复。上游限流 / 过载的语义要带到客户端：429 让 SDK 按
/// `Retry-After` 退避，503 表示服务暂时不可用——一律回 502 会让客户端把它当成网关故障。
/// 其余失败仍是 502 加上一次失败的错误码；日志里的错误码与上游状态不受这里影响。
///
/// `Retry-After` 只在整个池都试过了（不是次数用尽或请求不许改投）时给：此时没有候选可试，
/// 最早有 key 回来的时刻就是客户端该等的时间；候选里可能还有好的，就不替客户端编一个等待时间。
pub(super) async fn exhausted_reply(
    bill: &RequestBilling,
    code: &'static str,
    status: Option<i16>,
    kind: Option<KeyFailure>,
    stopped_early: bool,
) -> AppError {
    let rate_limited = status == Some(429) && matches!(kind, Some(KeyFailure::RateLimited { .. }));
    let reply = if code == codes::NO_AVAILABLE_CHANNEL || status == Some(529) {
        AppError::new(StatusCode::SERVICE_UNAVAILABLE, code)
    } else if rate_limited {
        AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param("upstream")
    } else {
        return AppError::new(StatusCode::BAD_GATEWAY, code);
    };
    if stopped_early {
        return reply;
    }
    match recovery_secs(bill, None).await {
        Some(secs) => reply.with_retry_after(secs),
        None => reply,
    }
}

/// 按 入口协议 × 渠道协议 构造上游请求体：同方言重写 model 透传，跨方言走出向转换；
/// Responses 对说 Responses 方言的渠道直转，否则先降级 chat，再按渠道协议二段转换。
/// 末尾做 reasoning 后缀注入（按上游方言；显式字段不覆盖）。
/// 上游请求体构造失败（`UpstreamError::Build` 的原因）→ 400。协议转换表达不了的 content part
/// 回显 part 类型，调用方才知道是哪块附件不被支持；其它构造失败不带 param。
pub(super) fn build_rejection(reason: &str) -> AppError {
    if reason.starts_with(okapi_providers::convert::UNSUPPORTED_CONTENT_PREFIX) {
        AppError::bad_request().with_param(reason)
    } else {
        AppError::bad_request()
    }
}

pub(super) fn classify_fatal(
    err: UpstreamError,
    failover: i16,
    channel: (i64, i64),
) -> AttemptError {
    if let Some(trace) = super::super::diagnostics::Trace::current() {
        trace.failure(&err);
    }
    if err.retriable_before_first_token() {
        return AttemptError::Retriable {
            code: err.error_code(),
            upstream_status: err.upstream_status(),
            failure_kind: failure_kind_of(&err),
        };
    }
    match err {
        UpstreamError::Status { status, body, .. } => {
            let mut failure = ForwardFailure {
                reply: FailureReply::Upstream { status, body },
                error_code: codes::UPSTREAM_ERROR.to_owned(),
                upstream_status: i16::try_from(status).ok(),
                failover_count: failover,
                channel: Some(channel),
                upstream: None,
            };
            failure.error_code = format!("upstream_status_{status}");
            AttemptError::Fatal(failure)
        }
        UpstreamError::Build(reason) if reason.starts_with("tools.") => {
            AttemptError::Fatal(ForwardFailure::app(
                AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
                failover,
                Some(channel),
            ))
        }
        UpstreamError::Build(reason) => AttemptError::Fatal(ForwardFailure::app(
            build_rejection(&reason),
            failover,
            Some(channel),
        )),
        UpstreamError::Session { timed_out, .. } => AttemptError::Fatal(ForwardFailure::app(
            AppError::new(
                if timed_out {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                },
                if timed_out {
                    codes::UPSTREAM_TIMEOUT
                } else {
                    codes::UPSTREAM_ERROR
                },
            ),
            failover,
            Some(channel),
        )),
        UpstreamError::Connect(_)
        | UpstreamError::Unreachable { .. }
        | UpstreamError::Timeout
        | UpstreamError::Stream(_) => AttemptError::Fatal(ForwardFailure::app(
            AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
            failover,
            Some(channel),
        )),
    }
}

#[cfg(test)]
mod session_failure_tests {
    use super::*;

    #[test]
    fn uncertain_session_execution_is_fatal_even_before_first_output() {
        for (timed_out, status, code) in [
            (false, StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
            (true, StatusCode::GATEWAY_TIMEOUT, codes::UPSTREAM_TIMEOUT),
        ] {
            let error = UpstreamError::Session {
                reason: "responses_ws_closed",
                timed_out,
            };
            assert!(!error.retriable_before_first_token());
            let AttemptError::Fatal(failure) = classify_fatal(error, 2, (10, 20)) else {
                panic!("an uncertain session turn must not be replayed");
            };
            assert_eq!(failure.channel, Some((10, 20)));
            assert_eq!(failure.failover_count, 2);
            assert_eq!(failure.error_code, code);
            let FailureReply::App(reply) = failure.reply else {
                panic!("stable error envelope required");
            };
            assert_eq!(reply.status, status);
        }
    }
}
