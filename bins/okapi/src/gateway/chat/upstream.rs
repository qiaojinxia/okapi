//! 上游请求体：按入口 × 渠道方言构造与改写，并分派到注册的上游实现。
use super::super::execution_plan::{ExecutionPlan, Requirements};
use super::super::ingress::Ingress;
use super::super::sched_redis::response_affinity::{ResponseBinding, ResponseWriter};
use super::{ProbeInfo, RequestBilling, UnifiedReasoning, bound_default_output, lost_tools};
use bytes::Bytes;
use okapi_providers::convert::{
    anthropic_to_openai as conv_a2o, gemini_to_openai as conv_g2o, openai_to_anthropic as convert,
    openai_to_gemini as conv_gem, responses_to_chat as conv_resp,
};
use okapi_providers::reasoning;
use okapi_providers::{
    ChatResponse, StreamHandle, UpstreamError, ensure_stream_usage, rewrite_model,
};
use okapi_store::ChannelCandidate;

pub(super) fn build_upstream_body(
    bill: &RequestBilling,
    info: &ProbeInfo,
    cand: &ChannelCandidate,
    source: &Bytes,
    upstream_model: &str,
) -> Result<Bytes, UpstreamError> {
    let plan = ExecutionPlan::compile(
        bill.ingress,
        cand,
        &bill.model,
        Requirements {
            tools: info.needs_tools,
            vision: info.needs_vision,
            server_tools: bill.server_tools.has_tools(),
        },
    )?;
    // 渠道允许丢的工具：这次转换确实丢了，记进诊断，日志里看得到少了什么
    let lost = lost_tools(info, bill);
    if plan.converts(bill.ingress) && !lost.is_empty() {
        bill.trace
            .set("dropped_tools", serde_json::json!(lost.all()));
    }
    let native_responses = plan.native_responses();
    let dialect = plan.dialect.as_str();
    // 缺省输出上限写进入口方言的原文，由下面的出向转换器翻译成上游自己的字段：写在转换之后，
    // 跨方言时就落成上游不认的键（Gemini 上游的顶层 max_tokens，Anthropic / chat 上游的
    // max_output_tokens 与 generationConfig），推理模型的 chat 上游还会在 completion_cap
    // 改名之后收到被拒的 max_tokens。`source` 仍是客户端的原文：后面判断"客户端自己写没写
    // 上限"要看它，补上的缺省不能被当成客户端的硬上限。
    let body = &bound_default_output(bill.ingress, bill.default_output_cap, source.clone());
    let built = match (bill.ingress, dialect) {
        // Responses 同方言：只改 model，其余字段（previous_response_id/store/include/
        // 内置工具/reasoning）一律原样——这正是直转相对降级链的全部价值。
        // usage 随 response.completed 必带，无需 stream_options。
        (Ingress::Responses, _) if native_responses => {
            rewrite_model(body, &info.requested_model, upstream_model)
        }
        (Ingress::OpenAi, "anthropic") => {
            convert::request_openai_to_anthropic(body, upstream_model, bill.completion_cap)
        }
        (Ingress::OpenAi, "gemini") => conv_gem::request_openai_to_gemini(body),
        // Compact 同方言只重写 model，不注入 stream_options。
        (Ingress::ResponsesCompact, _) => {
            rewrite_model(body, &info.requested_model, upstream_model)
        }
        (Ingress::Anthropic, "anthropic") => {
            rewrite_model(body, &info.requested_model, upstream_model)
                .and_then(|body| with_anthropic_default_cap(body, bill.completion_cap))
        }
        // OpenAI 同方言：透传 + 流式补 include_usage。跨方言的三条路各自的
        // 转换器早已强制注入，唯独这条最常用的路曾漏掉——客户端不主动开
        // stream_options 时上游不返 usage，结算落字符估算，实测漏收约七成。
        (Ingress::OpenAi, _) => rewrite_model(body, &info.requested_model, upstream_model)
            .and_then(|b| {
                if info.stream {
                    ensure_stream_usage(&b)
                } else {
                    Ok(b)
                }
            }),
        (Ingress::Anthropic, _) => conv_a2o::request_anthropic_to_openai(body, upstream_model),
        (Ingress::Responses, dialect) => conv_resp::request_responses_to_chat(body, upstream_model)
            .and_then(|chat_body| match dialect {
                "anthropic" => convert::request_openai_to_anthropic(
                    &chat_body,
                    upstream_model,
                    bill.completion_cap,
                ),
                "gemini" => conv_gem::request_openai_to_gemini(&chat_body),
                _ => Ok(chat_body),
            }),
        // Gemini 同方言：原样透传（模型名在 URL 上，body 里没有可重写的 model）
        (Ingress::Gemini, "gemini") => Ok(body.clone()),
        // Gemini 客户端 + anthropic 上游：gemini→chat→anthropic 两跳
        (Ingress::Gemini, "anthropic") => conv_g2o::request_gemini_to_openai(
            body,
            upstream_model,
            info.stream,
        )
        .and_then(|chat_body| {
            convert::request_openai_to_anthropic(&chat_body, upstream_model, bill.completion_cap)
        }),
        // Gemini 客户端 + OpenAI(兼容) 上游：转换器已按 stream 注入 stream_options
        (Ingress::Gemini, _) => {
            conv_g2o::request_gemini_to_openai(body, upstream_model, info.stream)
        }
    }?;
    let built = bill.server_tools.restore(built, dialect)?;
    let profile = okapi_providers::model_parameters::profile(
        dialect,
        upstream_model,
        cand.api_base.as_deref(),
    );
    let built = okapi_providers::model_parameters::completion_cap(
        &profile,
        dialect,
        built,
        native_responses,
    )?;
    if bill.ingress != Ingress::ResponsesCompact
        && let Some(result) = okapi_providers::model_parameters::apply_effort(
            &profile,
            dialect,
            &built,
            source,
            bill.directive,
            native_responses,
        )
    {
        return result;
    }
    match bill.directive {
        _ if bill.ingress == Ingress::ResponsesCompact => Ok(built),
        Some(d) if native_responses => reasoning::apply_responses(&built, d),
        Some(d) => match dialect {
            "anthropic" => reasoning::apply_anthropic(&built, d),
            "gemini" => reasoning::apply_gemini(&built, d),
            _ => reasoning::apply_openai(&built, d),
        },
        None => Ok(built),
    }
}

/// Native Messages requires the same admitted default cap as converted requests.
pub(super) fn with_anthropic_default_cap(body: Bytes, cap: u32) -> Result<Bytes, UpstreamError> {
    let mut value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| UpstreamError::Build("request_body".into()))?;
    if value.get("max_tokens").is_some_and(|v| !v.is_null()) {
        return Ok(body);
    }
    let object = value
        .as_object_mut()
        .ok_or_else(|| UpstreamError::Build("request_body".into()))?;
    object.insert("max_tokens".into(), serde_json::json!(cap));
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|_| UpstreamError::Build("request_body".into()))
}

/// thinking-to-content 渠道开关：OpenAI 方言出口的 reasoning 转 <think> 正文。
pub(super) fn wrap_thinking_to_content(resp: ChatResponse) -> ChatResponse {
    use futures::StreamExt as _;
    use okapi_providers::convert::thinking;
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => ChatResponse::Json {
            status,
            upstream_request_id,
            body: thinking::rewrite_json(&body),
            usage,
        },
        ChatResponse::Stream(h) => {
            let mut st = thinking::ThinkingToContent::new();
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(st.step(item)));
            ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            })
        }
    }
}

/// 入口协议 × 渠道协议 分派：返回的事件流/JSON 一律已是**客户端方言**形状，
/// 泵送与结算无感。
// 四象限线性分派，拆分破坏矩阵完整视野
#[allow(clippy::too_many_lines)]
/// 渠道字段透传控制（new-api rc.23 #6847 对齐）：先剥除配置的顶层字段，再浅合并强制注入的字段。
/// 方言无关（对入口原文生效，转换路径自然继承）；受保护键两边都不动（防误配打断主链）。
/// 两项都没配时不解析（缺省零开销）；配了也只解析一次。
pub(super) fn adjust_request_fields(
    body: &Bytes,
    strip: &[String],
    inject: &serde_json::Map<String, serde_json::Value>,
) -> Option<Bytes> {
    const STRIP_PROTECTED: [&str; 5] = [
        "model",
        "messages",
        "stream",
        "previous_response_id",
        "conversation",
    ];
    const INJECT_PROTECTED: [&str; 6] = [
        "model",
        "messages",
        "stream",
        "provider",
        "previous_response_id",
        "conversation",
    ];
    if strip.is_empty() && inject.is_empty() {
        return None;
    }
    let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = value.as_object_mut()?;
    let mut changed = false;
    for field in strip {
        if !STRIP_PROTECTED.contains(&field.as_str()) && obj.remove(field).is_some() {
            changed = true;
        }
    }
    for (key, val) in inject {
        if !INJECT_PROTECTED.contains(&key.as_str()) {
            obj.insert(key.clone(), val.clone());
            changed = true;
        }
    }
    changed.then(|| Bytes::from(serde_json::to_vec(&value).unwrap_or_default()))
}

pub(super) fn shape_upstream_body(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: Bytes,
) -> Result<Bytes, UpstreamError> {
    // okapi 自己的路由指令必须先剥掉：上游不认识 `provider`，会 400。
    // 与渠道级 strip_request_fields 分开做——那是管理员配置，这是协议要求，不可关。
    // 请求有没有这个键，入口解析偏好时已经知道：没有就别再把整份请求体解析一遍去确认
    // （键名粗筛对带反斜杠的请求体一律放行，真实请求几乎都带，每次尝试都白做一次整份 DOM 解析）。
    let body = if bill.prefs.directive_present {
        super::super::routing_prefs::strip(&body).unwrap_or(body)
    } else {
        body
    };
    let native_responses =
        ExecutionPlan::compile(bill.ingress, cand, &bill.model, Requirements::default())?
            .native_responses();
    // 统一 `reasoning` 对象同理：意图已翻译进各方言的原生字段，原对象上游不认识（§11.26）。
    // Responses 直转是唯一的例外——`reasoning.effort` 就是上游的原生键，只摘非原生键。
    // 顶层没有这个对象时什么也摘不到（入口解析时已经看过）：跳过，别为一句「没有」再解析整份请求体。
    let body = if bill.unified_reasoning == UnifiedReasoning::Absent {
        body
    } else if native_responses {
        reasoning::strip_unified_for_responses(&body).unwrap_or(body)
    } else {
        reasoning::strip_unified(&body).unwrap_or(body)
    };
    let body = adjust_request_fields(
        &body,
        &cand.strip_request_fields,
        &cand.inject_request_fields,
    )
    .unwrap_or(body);
    if let Some(parent) = &bill.response_parent {
        let parsed: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| UpstreamError::Build(e.to_string()))?;
        if parsed
            .get("previous_response_id")
            .and_then(serde_json::Value::as_str)
            != Some(parent.id.as_str())
        {
            return Err(UpstreamError::Build(
                "previous_response_id_changed".to_owned(),
            ));
        }
    }
    Ok(body)
}

// 入口方言 × 上游协议矩阵的收敛点，拆分损害路由全貌可读性
#[allow(clippy::too_many_lines)]
pub(super) async fn dispatch_chat(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    stream: bool,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    let plan = ExecutionPlan::compile(bill.ingress, cand, &bill.model, Requirements::default())?;
    let native_responses = plan.native_responses();
    let body = shape_upstream_body(bill, cand, body)?;
    bill.server_tools.verify_outbound(&body)?;
    let upstream_model = cand.upstream_model(&bill.model).to_owned();
    // 订阅 provider 额外带上客户端身份头（真实 Claude Code / Codex CLI 经网关出去时上游看到它自己）
    let outbound = super::super::oauth_cred::outbound_with_client(
        cand,
        &bill.client_headers,
        Some(bill.user_id),
    );
    // 方言臂内部再按 provider 选传输（直连 / bedrock / vertex），见 dialect.rs
    let dialect = plan.dialect.as_str();
    let resp = match (bill.ingress, dialect) {
        (Ingress::ResponsesCompact, _) => {
            bill.state
                .responses_via(cand, base, body, false, true, &outbound)
                .await
        }
        // Responses 客户端 + 说 Responses 方言的上游：直转，事件原样透出
        (Ingress::Responses, _) if native_responses => {
            bill.state
                .responses_via(cand, base, body, stream, false, &outbound)
                .await
        }
        // Gemini 客户端 + gemini 方言上游：透传 + 计费元数据扫描
        (Ingress::Gemini, "gemini") => {
            match bill
                .state
                .generate_via(cand, base, &upstream_model, body, stream)
                .await?
            {
                okapi_providers::gemini::GeminiResponse::Json {
                    status,
                    upstream_request_id,
                    body,
                } => {
                    let usage = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| conv_gem::usage_from_gemini(v.get("usageMetadata")));
                    Ok(ChatResponse::Json {
                        status,
                        upstream_request_id,
                        body,
                        usage,
                    })
                }
                okapi_providers::gemini::GeminiResponse::Stream(h) => {
                    let mut scanner = okapi_providers::gemini::MetaScanner::new();
                    let events = h
                        .events
                        .flat_map(move |item| futures::stream::iter(scanner.scan(item)));
                    Ok(ChatResponse::Stream(StreamHandle {
                        upstream_request_id: h.upstream_request_id,
                        events: Box::pin(events),
                    }))
                }
            }
        }
        // Gemini 客户端 + anthropic 方言上游：providers 内转回 OpenAI 形状，再回 Gemini 形状
        (Ingress::Gemini, "anthropic") => bill
            .state
            .messages_via(cand, base, &upstream_model, body, stream, &outbound)
            .await
            .and_then(|resp| convert::wrap_messages(resp, &upstream_model))
            .and_then(|resp| conv_g2o::wrap_chat_as_gemini(resp, &upstream_model)),
        // Gemini 客户端 + OpenAI(兼容 / Azure) 上游：chat 形状 → Gemini 形状
        (Ingress::Gemini, _) => bill
            .state
            .openai_chat(cand, &upstream_model, body, stream)
            .await
            .and_then(|resp| conv_g2o::wrap_chat_as_gemini(resp, &upstream_model)),
        // OpenAI/Responses 客户端 + gemini 方言上游：providers 内转回 OpenAI 形状
        (Ingress::OpenAi | Ingress::Responses, "gemini") => bill
            .state
            .generate_via(cand, base, &upstream_model, body, stream)
            .await
            .and_then(|resp| conv_gem::wrap_generate(resp, &upstream_model)),
        // OpenAI/Responses 客户端 + anthropic 方言上游：providers 内转回 OpenAI 形状
        (Ingress::OpenAi | Ingress::Responses, "anthropic") => bill
            .state
            .messages_via(cand, base, &upstream_model, body, stream, &outbound)
            .await
            .and_then(|resp| convert::wrap_messages(resp, &upstream_model)),
        // 同方言 OpenAI（官方 / 兼容 / Azure）：原样，仅 URL 与鉴权头按 provider 分派
        (Ingress::OpenAi | Ingress::Responses, _) => {
            bill.state
                .openai_chat(cand, &upstream_model, body, stream)
                .await
        }
        // Anthropic 客户端 + anthropic 方言上游：透传 + 计费元数据扫描
        (Ingress::Anthropic, "anthropic") => {
            match bill
                .state
                .messages_via(cand, base, &upstream_model, body, stream, &outbound)
                .await?
            {
                okapi_providers::anthropic::MessagesResponse::Json {
                    status,
                    upstream_request_id,
                    body,
                } => {
                    let usage = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| convert::usage_from_anthropic(v.get("usage")));
                    Ok(ChatResponse::Json {
                        status,
                        upstream_request_id,
                        body,
                        usage,
                    })
                }
                okapi_providers::anthropic::MessagesResponse::Stream(h) => {
                    let mut scanner = okapi_providers::anthropic::MetaScanner::new();
                    let events = h
                        .events
                        .flat_map(move |item| futures::stream::iter(scanner.scan(item)));
                    Ok(ChatResponse::Stream(StreamHandle {
                        upstream_request_id: h.upstream_request_id,
                        events: Box::pin(events),
                    }))
                }
            }
        }
        // Anthropic 客户端 + OpenAI(兼容 / Azure) 上游：回向转换为 Anthropic 事件/JSON
        (Ingress::Anthropic, _) => {
            match bill
                .state
                .openai_chat(cand, &upstream_model, body, stream)
                .await?
            {
                ChatResponse::Json {
                    status,
                    upstream_request_id,
                    body,
                    usage,
                } => {
                    let (body, parsed_usage) = conv_a2o::response_openai_to_anthropic(&body)?;
                    Ok(ChatResponse::Json {
                        status,
                        upstream_request_id,
                        body,
                        usage: usage.or(parsed_usage),
                    })
                }
                ChatResponse::Stream(h) => {
                    let mut st = conv_a2o::OaiStreamToAnthropic::new(&upstream_model);
                    let events = h
                        .events
                        .flat_map(move |item| futures::stream::iter(st.step(item)));
                    Ok(ChatResponse::Stream(StreamHandle {
                        upstream_request_id: h.upstream_request_id,
                        events: Box::pin(events),
                    }))
                }
            }
        }
    };
    let mut resp = resp?;
    if native_responses {
        bill.trace.finish(None, None);
        // 直转：上游已是 Responses 形状，reasoning 以原生 reasoning item 呈现，
        // 既无需合成事件骨架，也不做 thinking_to_content（那是 chat 形状的补丁）
        return Ok(resp);
    }
    if matches!(bill.ingress, Ingress::OpenAi | Ingress::Responses) && cand.thinking_to_content {
        resp = wrap_thinking_to_content(resp);
    }
    // Responses 出口：chat 形状 → Responses 事件/对象（降级链的回程半跳）
    if bill.ingress == Ingress::Responses {
        resp = wrap_responses_egress(resp, &upstream_model)?;
    }
    Ok(resp)
}

/// chat 形状 → Responses 方言（response.created/.output_text.delta/.completed 事件骨架）。
pub(super) fn wrap_responses_egress(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => {
            let (body, parsed_usage) = conv_resp::response_chat_to_responses(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                // Protocol JSON cannot carry the internal invalid-usage marker.
                // Preserve the upstream probe across every conversion hop.
                usage: usage.or(parsed_usage),
            })
        }
        ChatResponse::Stream(h) => {
            let mut st = conv_resp::ChatStreamToResponses::new(upstream_model);
            let events = h
                .events
                .flat_map(move |item| futures::stream::iter(st.step(item)));
            Ok(ChatResponse::Stream(StreamHandle {
                upstream_request_id: h.upstream_request_id,
                events: Box::pin(events),
            }))
        }
    }
}

pub(super) fn response_writer(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
) -> Option<ResponseWriter> {
    (bill.ingress == Ingress::Responses && cand.responses_native)
        .then(|| ResponseWriter::new(ResponseBinding::from_candidate(cand)))
}
