//! 上游请求体：按入口 × 渠道方言构造与改写，并分派到注册的上游实现。
use super::super::execution_plan::{ExecutionPlan, Operation, PlanError, Requirements};
use super::super::ingress::Ingress;
use super::{
    ProbeInfo, RequestBilling, UnifiedReasoning, bound_default_output_value, lost_tools,
    needs_default_output_bound,
};
use bytes::Bytes;
use okapi_kernel::sched_redis::response_affinity::{ResponseBinding, ResponseWriter};
use okapi_providers::convert::{
    anthropic_to_openai as conv_a2o, chat_to_responses as conv_c2r, gemini_to_openai as conv_g2o,
    openai_to_anthropic as convert, openai_to_gemini as conv_gem, responses_to_chat as conv_resp,
};
use okapi_providers::reasoning;
use okapi_providers::{
    ChatResponse, StreamHandle, UpstreamError, ensure_stream_usage_value, rewrite_model_value,
};
use okapi_store::ChannelCandidate;
use std::borrow::Cow;

/// 写时复制请求体：未发生任何改写时直接透传原始 `Bytes`，一旦需要改写才克隆/替换 `Value`，
/// 并在管线末端最多序列化一次，消除中间环节的多次 JSON 编解码与键重排。
struct WorkingBody<'a> {
    raw: Bytes,
    source: Cow<'a, serde_json::Value>,
    modified: Option<serde_json::Value>,
}

impl<'a> WorkingBody<'a> {
    fn new(raw: Bytes, source: Cow<'a, serde_json::Value>) -> Self {
        Self {
            raw,
            source,
            modified: None,
        }
    }

    fn as_value(&self) -> &serde_json::Value {
        self.modified.as_ref().unwrap_or(self.source.as_ref())
    }

    fn as_mut(&mut self) -> &mut serde_json::Value {
        if self.modified.is_none() {
            self.modified = Some(self.source.as_ref().clone());
        }
        self.modified
            .as_mut()
            .expect("modified initialized just above")
    }

    fn replace(&mut self, value: serde_json::Value) {
        self.modified = Some(value);
    }

    fn into_bytes(self) -> Result<Bytes, UpstreamError> {
        match self.modified {
            Some(value) => serde_json::to_vec(&value)
                .map(Bytes::from)
                .map_err(|e| UpstreamError::Build(e.to_string())),
            None => Ok(self.raw),
        }
    }
}

pub(super) fn build_upstream_body(
    bill: &RequestBilling,
    info: &ProbeInfo,
    cand: &ChannelCandidate,
    source: &Bytes,
    upstream_model: &str,
    source_value: Option<&serde_json::Value>,
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
    let native_responses = plan.native_responses();
    let dialect = plan.dialect.as_str();
    let parsed_source = match source_value {
        Some(v) => Cow::Borrowed(v),
        None => Cow::Owned(
            serde_json::from_slice::<serde_json::Value>(source)
                .map_err(|e| UpstreamError::Build(e.to_string()))?,
        ),
    };
    record_conversion_loss(bill, info, plan, parsed_source.as_ref());
    let mut working = WorkingBody::new(source.clone(), parsed_source);

    // 缺省输出上限写进入口方言的原文，由下面的出向转换器翻译成上游自己的字段：写在转换之后，
    // 跨方言时就落成上游不认的键（Gemini 上游的顶层 max_tokens，Anthropic / chat 上游的
    // max_output_tokens 与 generationConfig），推理模型的 chat 上游还会在 completion_cap
    // 改名之后收到被拒的 max_tokens。`working.source` 仍是客户端原文：后面判断"客户端自己写没写
    // 上限"要看它，补上的缺省不能被当成客户端的硬上限。
    if needs_default_output_bound(bill.ingress, bill.default_output_cap, working.as_value()) {
        bound_default_output_value(bill.ingress, bill.default_output_cap, working.as_mut());
    }
    convert_request_in_place(&mut working, bill, info, plan.operation, upstream_model)?;
    if bill
        .server_tools
        .needs_restore(working.as_value(), dialect)?
    {
        bill.server_tools.restore_value(working.as_mut(), dialect)?;
    }
    let profile = okapi_providers::model_parameters::profile(
        dialect,
        upstream_model,
        cand.api_base.as_deref(),
    );
    if okapi_providers::model_parameters::needs_completion_cap(
        &profile,
        dialect,
        working.as_value(),
        native_responses,
    ) {
        okapi_providers::model_parameters::completion_cap_value(
            &profile,
            dialect,
            working.as_mut(),
            native_responses,
        )?;
    }
    if bill.ingress != Ingress::ResponsesCompact {
        let effort_plan = okapi_providers::model_parameters::plan_effort(
            &profile,
            dialect,
            working.as_value(),
            working.source.as_ref(),
            bill.directive,
            native_responses,
        )?;
        match effort_plan {
            okapi_providers::model_parameters::EffortPlan::Fallback => {}
            okapi_providers::model_parameters::EffortPlan::Unchanged => {
                return working.into_bytes();
            }
            plan_to_apply => {
                // Separate borrows: take `working.as_mut()` cleanly against immutable `working.source`.
                if working.modified.is_none() {
                    working.modified = Some(working.source.as_ref().clone());
                }
                let target = working
                    .modified
                    .as_mut()
                    .expect("modified initialized just above");
                okapi_providers::model_parameters::apply_effort_plan(
                    plan_to_apply,
                    dialect,
                    target,
                    working.source.as_ref(),
                    native_responses,
                )?;
                return working.into_bytes();
            }
        }
    }
    if let Some(directive) = bill.directive {
        apply_fallback_reasoning(&mut working, plan.operation, directive)?;
    }
    working.into_bytes()
}

fn record_conversion_loss(
    bill: &RequestBilling,
    info: &ProbeInfo,
    plan: ExecutionPlan,
    source: &serde_json::Value,
) {
    if !plan.converts(bill.ingress) {
        bill.trace.remove("dropped_tools");
        bill.trace.remove("dropped_fields");
        return;
    }
    let lost = lost_tools(info, bill);
    if lost.is_empty() {
        bill.trace.remove("dropped_tools");
    } else {
        bill.trace
            .set("dropped_tools", serde_json::json!(lost.all()));
    }
    let dropped = okapi_providers::convert::field_loss::dropped_fields(
        super::super::execution_plan::format(bill.ingress),
        plan.operation.api_format(),
        source,
    );
    if dropped.is_empty() {
        bill.trace.remove("dropped_fields");
    } else {
        bill.trace
            .set("dropped_fields", serde_json::json!(dropped));
    }
}

fn apply_fallback_reasoning(
    working: &mut WorkingBody<'_>,
    operation: Operation,
    directive: okapi_providers::reasoning::ReasoningDirective,
) -> Result<(), UpstreamError> {
    match operation {
        // Compact 不带推理强度
        Operation::ResponsesCompact => Ok(()),
        Operation::Responses => {
            if reasoning::needs_apply_responses(working.as_value())? {
                reasoning::apply_responses_value(working.as_mut(), directive)?;
            }
            Ok(())
        }
        Operation::Messages => {
            if reasoning::needs_apply_anthropic(working.as_value())? {
                reasoning::apply_anthropic_value(working.as_mut(), directive)?;
            }
            Ok(())
        }
        Operation::GenerateContent => {
            if reasoning::needs_apply_gemini(working.as_value())? {
                reasoning::apply_gemini_value(working.as_mut(), directive)?;
            }
            Ok(())
        }
        Operation::ChatCompletions => {
            if reasoning::needs_apply_openai(working.as_value())? {
                reasoning::apply_openai_value(working.as_mut(), directive)?;
            }
            Ok(())
        }
    }
}

/// 请求体的「入口 × 操作」矩阵：同方言只按需原地改 model / 补丁，跨方言直接从内存 `Value` 构造。
fn convert_request_in_place(
    working: &mut WorkingBody<'_>,
    bill: &RequestBilling,
    info: &ProbeInfo,
    operation: Operation,
    upstream_model: &str,
) -> Result<(), UpstreamError> {
    match (bill.ingress, operation) {
        (Ingress::Responses, Operation::Responses)
        | (Ingress::ResponsesCompact, Operation::ResponsesCompact) => {
            if info.requested_model != upstream_model {
                rewrite_model_value(working.as_mut(), &info.requested_model, upstream_model)?;
            }
            Ok(())
        }
        (Ingress::OpenAi, Operation::ChatCompletions) => {
            if info.requested_model != upstream_model {
                rewrite_model_value(working.as_mut(), &info.requested_model, upstream_model)?;
            }
            if info.stream
                && working
                    .as_value()
                    .as_object()
                    .and_then(|o| o.get("stream_options"))
                    .and_then(|s| s.get("include_usage"))
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
            {
                ensure_stream_usage_value(working.as_mut())?;
            }
            Ok(())
        }
        (Ingress::Anthropic, Operation::Messages) => {
            if info.requested_model != upstream_model {
                rewrite_model_value(working.as_mut(), &info.requested_model, upstream_model)?;
            }
            if needs_anthropic_default_cap(working.as_value())? {
                with_anthropic_default_cap_value(working.as_mut(), bill.completion_cap)?;
            }
            Ok(())
        }
        (Ingress::Gemini, Operation::GenerateContent) => Ok(()),
        _ => {
            let converted = convert_cross_dialect(
                bill.ingress,
                operation,
                working.as_value(),
                upstream_model,
                bill.completion_cap,
                info.stream,
            )?;
            working.replace(converted);
            Ok(())
        }
    }
}

fn convert_cross_dialect(
    ingress: Ingress,
    operation: Operation,
    src: &serde_json::Value,
    upstream_model: &str,
    completion_cap: u32,
    stream: bool,
) -> Result<serde_json::Value, UpstreamError> {
    match (ingress, operation) {
        (Ingress::OpenAi, Operation::Messages) => {
            convert::request_openai_to_anthropic_value(src, upstream_model, completion_cap)
        }
        (Ingress::OpenAi, Operation::GenerateContent) => {
            conv_gem::request_openai_to_gemini_value(src)
        }
        (Ingress::Anthropic, Operation::ChatCompletions) => {
            conv_a2o::request_anthropic_to_openai_value(src, upstream_model)
        }
        (Ingress::Responses, Operation::ChatCompletions) => {
            conv_resp::request_responses_to_chat_value(src, upstream_model)
        }
        (Ingress::Responses, Operation::Messages) => {
            let chat = conv_resp::request_responses_to_chat_value(src, upstream_model)?;
            convert::request_openai_to_anthropic_value(&chat, upstream_model, completion_cap)
        }
        (Ingress::Responses, Operation::GenerateContent) => {
            let chat = conv_resp::request_responses_to_chat_value(src, upstream_model)?;
            conv_gem::request_openai_to_gemini_value(&chat)
        }
        (Ingress::Gemini, Operation::Messages) => {
            let chat = conv_g2o::request_gemini_to_openai_value(src, upstream_model, stream)?;
            convert::request_openai_to_anthropic_value(&chat, upstream_model, completion_cap)
        }
        (Ingress::Gemini, Operation::ChatCompletions) => {
            conv_g2o::request_gemini_to_openai_value(src, upstream_model, stream)
        }
        (Ingress::OpenAi, Operation::Responses) => {
            conv_c2r::request_chat_to_responses_value(src, upstream_model)
        }
        (Ingress::Anthropic, Operation::Responses) => {
            let chat = conv_a2o::request_anthropic_to_openai_value(src, upstream_model)?;
            conv_c2r::request_chat_to_responses_value(&chat, upstream_model)
        }
        (Ingress::Gemini, Operation::Responses) => {
            let chat = conv_g2o::request_gemini_to_openai_value(src, upstream_model, stream)?;
            conv_c2r::request_chat_to_responses_value(&chat, upstream_model)
        }
        (
            Ingress::OpenAi | Ingress::Anthropic | Ingress::Gemini | Ingress::Responses,
            Operation::ResponsesCompact,
        )
        | (Ingress::Responses, Operation::Responses)
        | (Ingress::Anthropic | Ingress::Gemini, Operation::GenerateContent)
        | (
            Ingress::ResponsesCompact,
            Operation::ChatCompletions
            | Operation::Messages
            | Operation::GenerateContent
            | Operation::Responses
            | Operation::ResponsesCompact,
        )
        | (Ingress::OpenAi, Operation::ChatCompletions)
        | (Ingress::Anthropic, Operation::Messages) => Err(PlanError::Endpoint.into()),
    }
}

fn needs_anthropic_default_cap(value: &serde_json::Value) -> Result<bool, UpstreamError> {
    let obj = value
        .as_object()
        .ok_or_else(|| UpstreamError::Build("request_body".into()))?;
    Ok(obj.get("max_tokens").is_none_or(serde_json::Value::is_null))
}

/// Native Messages requires the same admitted default cap as converted requests.
fn with_anthropic_default_cap_value(
    value: &mut serde_json::Value,
    cap: u32,
) -> Result<(), UpstreamError> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| UpstreamError::Build("request_body".into()))?;
    if object
        .get("max_tokens")
        .is_none_or(serde_json::Value::is_null)
    {
        object.insert("max_tokens".into(), serde_json::json!(cap));
    }
    Ok(())
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
pub(super) fn adjust_request_fields_value(
    value: &mut serde_json::Value,
    strip: &[String],
    inject: &serde_json::Map<String, serde_json::Value>,
) -> bool {
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
    let Some(obj) = value.as_object_mut() else {
        return false;
    };
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
    changed
}

pub(super) fn shape_upstream_body(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: Bytes,
) -> Result<Bytes, UpstreamError> {
    shape_and_maybe_verify(bill, cand, body, false)
}

fn shape_and_verify_outbound(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: Bytes,
) -> Result<Bytes, UpstreamError> {
    shape_and_maybe_verify(bill, cand, body, true)
}

/// 单次解析完成出站塑形（路由偏好摘除、统一 reasoning 摘除、渠道字段增删、`previous_response_id` 校验）
/// 与 `server_tools` 终检；没有任何塑形/检查需求时零解析、零拷贝直通。
fn shape_and_maybe_verify(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: Bytes,
    verify_tools: bool,
) -> Result<Bytes, UpstreamError> {
    let needs_fields =
        !cand.strip_request_fields.is_empty() || !cand.inject_request_fields.is_empty();
    let needs_shape = bill.prefs.directive_present
        || bill.unified_reasoning != UnifiedReasoning::Absent
        || needs_fields
        || bill.response_parent.is_some();
    if !needs_shape {
        if verify_tools {
            bill.server_tools.verify_outbound(&body)?;
        }
        return Ok(body);
    }
    let native_responses = if bill.unified_reasoning == UnifiedReasoning::Absent {
        false
    } else {
        ExecutionPlan::compile(bill.ingress, cand, &bill.model, Requirements::default())?
            .native_responses()
    };
    let mut value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) if bill.response_parent.is_some() => {
            return Err(UpstreamError::Build(err.to_string()));
        }
        Err(_) => {
            if verify_tools {
                bill.server_tools.verify_outbound(&body)?;
            }
            return Ok(body);
        }
    };
    let mut changed = false;
    if bill.prefs.directive_present {
        changed |= super::super::routing_prefs::strip_value(&mut value);
    }
    if bill.unified_reasoning != UnifiedReasoning::Absent {
        changed |= if native_responses {
            reasoning::strip_unified_for_responses_value(&mut value)
        } else {
            reasoning::strip_unified_value(&mut value)
        };
    }
    if needs_fields {
        changed |= adjust_request_fields_value(
            &mut value,
            &cand.strip_request_fields,
            &cand.inject_request_fields,
        );
    }
    if let Some(parent) = &bill.response_parent
        && value
            .get("previous_response_id")
            .and_then(serde_json::Value::as_str)
            != Some(parent.id.as_str())
    {
        return Err(UpstreamError::Build(
            "previous_response_id_changed".to_owned(),
        ));
    }
    if verify_tools {
        bill.server_tools.verify_outbound_value(&value)?;
    }
    if !changed {
        return Ok(body);
    }
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))
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
    let body = shape_and_verify_outbound(bill, cand, body)?;
    let upstream_model = cand.upstream_model(&bill.model).to_owned();
    // 订阅 provider 额外带上客户端身份头（真实 Claude Code / Codex CLI 经网关出去时上游看到它自己）
    let outbound = super::super::oauth_cred::outbound_with_client(
        cand,
        &bill.client_headers,
        Some(bill.user_id),
    );
    // 操作臂内部再按 provider 选传输（直连 / bedrock / vertex），见 dialect.rs
    let endpoint = plan.endpoint(stream);
    let infer_call = |model, body, stream| super::super::dialect::InferCall {
        cand,
        endpoint,
        base,
        model,
        body,
        stream,
        outbound: &outbound,
    };
    let resp = match (bill.ingress, plan.operation) {
        (Ingress::ResponsesCompact, Operation::ResponsesCompact) => {
            bill.state
                .responses_via(true, infer_call("", body, false))
                .await
        }
        // Responses 客户端 + 说 Responses 方言的上游：直转，事件原样透出
        (Ingress::Responses, Operation::Responses) => {
            bill.state
                .responses_via(false, infer_call("", body, stream))
                .await
        }
        // Gemini 客户端 + gemini 方言上游：透传 + 计费元数据扫描
        (Ingress::Gemini, Operation::GenerateContent) => {
            match bill
                .state
                .generate_via(cand, endpoint, base, &upstream_model, body, stream)
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
        (Ingress::Gemini, Operation::Messages) => bill
            .state
            .messages_via(infer_call(&upstream_model, body, stream))
            .await
            .and_then(|resp| convert::wrap_messages(resp, &upstream_model))
            .and_then(|resp| conv_g2o::wrap_chat_as_gemini(resp, &upstream_model)),
        // Gemini 客户端 + OpenAI(兼容 / Azure) 上游：chat 形状 → Gemini 形状
        (Ingress::Gemini, Operation::ChatCompletions) => bill
            .state
            .openai_chat(cand, endpoint, &upstream_model, body, stream)
            .await
            .map(surface_stream_failures)
            .and_then(|resp| conv_g2o::wrap_chat_as_gemini(resp, &upstream_model)),
        // OpenAI/Responses 客户端 + gemini 方言上游：providers 内转回 OpenAI 形状
        (Ingress::OpenAi | Ingress::Responses, Operation::GenerateContent) => bill
            .state
            .generate_via(cand, endpoint, base, &upstream_model, body, stream)
            .await
            .and_then(|resp| conv_gem::wrap_generate(resp, &upstream_model)),
        // OpenAI/Responses 客户端 + anthropic 方言上游：providers 内转回 OpenAI 形状
        (Ingress::OpenAi | Ingress::Responses, Operation::Messages) => bill
            .state
            .messages_via(infer_call(&upstream_model, body, stream))
            .await
            .and_then(|resp| convert::wrap_messages(resp, &upstream_model)),
        // 同方言 OpenAI（官方 / 兼容 / Azure）：原样，仅 URL 与鉴权头按 provider 分派
        (Ingress::OpenAi | Ingress::Responses, Operation::ChatCompletions) => {
            bill.state
                .openai_chat(cand, endpoint, &upstream_model, body, stream)
                .await
        }
        // Anthropic 客户端 + anthropic 方言上游：透传 + 计费元数据扫描
        (Ingress::Anthropic, Operation::Messages) => {
            match bill
                .state
                .messages_via(infer_call(&upstream_model, body, stream))
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
        // OpenAI 客户端 + Responses 上游（Responses-only 渠道或开启 chat_via_responses 的 Codex 渠道）
        (Ingress::OpenAi, Operation::Responses) => bill
            .state
            .responses_via(false, infer_call("", body, stream))
            .await
            .map(surface_stream_failures)
            .and_then(|resp| conv_c2r::wrap_responses_as_chat(resp, &upstream_model)),
        // Anthropic 客户端 + Responses 上游：Responses → Chat → Anthropic 两跳回向转换
        (Ingress::Anthropic, Operation::Responses) => bill
            .state
            .responses_via(false, infer_call("", body, stream))
            .await
            .map(surface_stream_failures)
            .and_then(|resp| conv_c2r::wrap_responses_as_chat(resp, &upstream_model))
            .and_then(|resp| wrap_chat_as_anthropic(resp, &upstream_model)),
        // Gemini 客户端 + Responses 上游：Responses → Chat → Gemini 两跳回向转换
        (Ingress::Gemini, Operation::Responses) => bill
            .state
            .responses_via(false, infer_call("", body, stream))
            .await
            .map(surface_stream_failures)
            .and_then(|resp| conv_c2r::wrap_responses_as_chat(resp, &upstream_model))
            .and_then(|resp| conv_g2o::wrap_chat_as_gemini(resp, &upstream_model)),
        // Anthropic 客户端 + OpenAI(兼容 / Azure) 上游：回向转换为 Anthropic 事件/JSON
        (Ingress::Anthropic, Operation::ChatCompletions) => wrap_chat_as_anthropic(
            surface_stream_failures(
                bill.state
                    .openai_chat(cand, endpoint, &upstream_model, body, stream)
                    .await?,
            ),
            &upstream_model,
        ),
        // 编计划时已排除：Messages 入口不接 Gemini 方言，Compact 操作只给同名入口
        (
            Ingress::OpenAi | Ingress::Anthropic | Ingress::Gemini | Ingress::Responses,
            Operation::ResponsesCompact,
        )
        | (Ingress::Anthropic, Operation::GenerateContent)
        | (
            Ingress::ResponsesCompact,
            Operation::ChatCompletions
            | Operation::Messages
            | Operation::GenerateContent
            | Operation::Responses,
        ) => Err(PlanError::Endpoint.into()),
    };
    let mut resp = resp?;
    if matches!(bill.ingress, Ingress::Responses | Ingress::ResponsesCompact) && native_responses {
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

/// 上游说 chat 方言、流要转成别的方言给客户端时：在转换**之前**认出流里的错误事件（`{"error":…}`），
/// 变成 `Err` 交给转换器原样往下传。
///
/// 转换器不认 chat 的错误事件（当普通块处理，至多产出一个 `message_start`），转换后网关再按客户端方言找
/// （`stream::stream_failure_in`）就找不到了：客户端拿到空回复，看不到上游的 429 / 529，也不会按限流退避
/// （10-29 修过的 `server_is_overloaded` 同一类，跨方言这条路漏了）。判定与透传路径同一个函数；同方言透传
/// 不走这里，仍在原始事件上判（不算错误的说明留作空回复的诊断）。
fn surface_stream_failures(resp: ChatResponse) -> ChatResponse {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Stream(h) => ChatResponse::Stream(StreamHandle {
            upstream_request_id: h.upstream_request_id,
            events: Box::pin(h.events.map(|item| {
                if let Ok(event) = &item
                    && let Some(error) = super::stream::stream_failure_in(event, &mut None)
                {
                    return Err(error);
                }
                item
            })),
        }),
        json @ ChatResponse::Json { .. } => json,
    }
}

/// chat 形状 → Anthropic Messages 方言（供 Chat / Responses 上游共享）。
fn wrap_chat_as_anthropic(
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
            let (body, parsed_usage) = conv_a2o::response_openai_to_anthropic(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage: usage.or(parsed_usage),
            })
        }
        ChatResponse::Stream(h) => {
            let mut st = conv_a2o::OaiStreamToAnthropic::new(upstream_model);
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

/// chat 形状 → Responses 方言（response.created/.output_text.delta/.completed 事件骨架）。
pub(super) fn wrap_responses_egress(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    let resp = surface_stream_failures(resp);
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

#[cfg(test)]
mod tests;
