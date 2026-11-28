//! 上游请求体：按入口 × 渠道方言构造与改写，并经由协议转换注册表与声明式路由分派到上游。
use super::super::execution_plan::{ExecutionPlan, Operation, Requirements};
use super::super::ingress::Ingress;
use super::{
    ProbeInfo, RequestBilling, UnifiedReasoning, bound_default_output_value, lost_tools,
    needs_default_output_bound,
};
use bytes::Bytes;
use okapi_kernel::sched_redis::response_affinity::{ResponseBinding, ResponseWriter};
use okapi_providers::convert::registry::ConvertContext;
use okapi_providers::reasoning;
use okapi_providers::{
    ChatResponse, UpstreamError, ensure_stream_usage_value, rewrite_model_value,
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
    convert_request_in_place(&mut working, bill, info, plan, upstream_model)?;
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
        bill.trace.remove("conversion_converter");
        bill.trace.remove("conversion_quality");
        bill.trace.remove("conversion_diagnostics");
        return;
    }
    bill.trace
        .set("conversion_converter", serde_json::json!(plan.converter.name));
    bill.trace.set(
        "conversion_quality",
        serde_json::json!(plan.converter.quality.as_str()),
    );
    let lost = lost_tools(info, bill);
    if lost.is_empty() {
        bill.trace.remove("dropped_tools");
    } else {
        bill.trace
            .set("dropped_tools", serde_json::json!(lost.all()));
    }
    let dropped = okapi_providers::convert::field_loss::dropped_fields(
        plan.converter.from,
        plan.converter.to,
        source,
    );
    if dropped.is_empty() {
        bill.trace.remove("dropped_fields");
    } else {
        bill.trace
            .set("dropped_fields", serde_json::json!(dropped));
    }
    let diagnostics = plan.converter.diagnostics(source, &lost);
    if diagnostics.is_empty() {
        bill.trace.remove("conversion_diagnostics");
    } else {
        bill.trace
            .set("conversion_diagnostics", serde_json::json!(diagnostics));
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

/// 请求体转换：同方言原生直通按需原地改 model / 补丁，跨方言统一走注册表步链。
fn convert_request_in_place(
    working: &mut WorkingBody<'_>,
    bill: &RequestBilling,
    info: &ProbeInfo,
    plan: ExecutionPlan,
    upstream_model: &str,
) -> Result<(), UpstreamError> {
    if plan.converter.is_native() {
        return match bill.ingress {
            Ingress::Responses | Ingress::ResponsesCompact => {
                if info.requested_model != upstream_model {
                    rewrite_model_value(working.as_mut(), &info.requested_model, upstream_model)?;
                }
                Ok(())
            }
            Ingress::OpenAi => {
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
            Ingress::Anthropic => {
                if info.requested_model != upstream_model {
                    rewrite_model_value(working.as_mut(), &info.requested_model, upstream_model)?;
                }
                if needs_anthropic_default_cap(working.as_value())? {
                    with_anthropic_default_cap_value(working.as_mut(), bill.completion_cap)?;
                }
                Ok(())
            }
            Ingress::Gemini => Ok(()),
        };
    }

    let converted = plan.converter.convert_request(
        working.as_value(),
        &ConvertContext {
            upstream_model,
            completion_cap: bill.completion_cap,
            stream: info.stream,
        },
    )?;
    working.replace(converted);
    Ok(())
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
    shape_and_maybe_verify(bill, cand, body, false).map(|(body, _)| body)
}

fn shape_and_verify_outbound(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: Bytes,
) -> Result<(Bytes, okapi_providers::request_overrides::OverrideHeaderPatch), UpstreamError> {
    shape_and_maybe_verify(bill, cand, body, true)
}

/// 单次解析完成出站塑形（路由偏好摘除、统一 reasoning 摘除、渠道字段增删、条件改写规则、
/// `previous_response_id` 校验）与 `server_tools` 终检；没有任何塑形/检查需求时零解析、零拷贝直通。
fn shape_and_maybe_verify(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: Bytes,
    verify_tools: bool,
) -> Result<(Bytes, okapi_providers::request_overrides::OverrideHeaderPatch), UpstreamError> {
    use okapi_providers::request_overrides::{
        OverrideContext, OverrideHeaderPatch, apply_rules, parse_rules_runtime,
    };

    let needs_fields =
        !cand.strip_request_fields.is_empty() || !cand.inject_request_fields.is_empty();
    let override_rules = parse_rules_runtime(&cand.request_overrides);
    let needs_shape = bill.prefs.directive_present
        || bill.unified_reasoning != UnifiedReasoning::Absent
        || needs_fields
        || override_rules.is_some()
        || bill.response_parent.is_some();
    if !needs_shape {
        if verify_tools {
            bill.server_tools.verify_outbound(&body)?;
        }
        return Ok((body, OverrideHeaderPatch::default()));
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
            return Ok((body, OverrideHeaderPatch::default()));
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
    let mut header_patch = OverrideHeaderPatch::default();
    if let Some(rules) = override_rules.as_deref() {
        let upstream_model = cand.upstream_model(&bill.model);
        let ctx = OverrideContext {
            model: &bill.model,
            requested_model: &bill.requested_model,
            upstream_model: upstream_model.as_ref(),
            ingress: bill.ingress.as_str(),
            user_agent: bill.user_agent.as_deref(),
            client_headers: &bill.client_headers,
        };
        let outcome = apply_rules(rules, &ctx, &mut value)
            .map_err(|rej| UpstreamError::Build(rej.encode_build_reason()))?;
        changed |= outcome.body_changed;
        header_patch = outcome.headers;
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
        return Ok((body, header_patch));
    }
    let encoded = serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|e| UpstreamError::Build(e.to_string()))?;
    Ok((encoded, header_patch))
}

/// 入口协议 × 渠道协议统一分派：传输走 [`AppState::infer_plan`]（兼容内置适配器与
/// `settings.advanced_routes`），回向响应按 [`ExecutionPlan::converter`] 的注册步链转换为客户端方言。
pub(super) async fn dispatch_chat(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    stream: bool,
) -> Result<ChatResponse, UpstreamError> {
    let plan = ExecutionPlan::compile(bill.ingress, cand, &bill.model, Requirements::default())?;
    let native_responses = plan.native_responses();
    let (body, override_headers) = shape_and_verify_outbound(bill, cand, body)?;
    let upstream_model = cand.upstream_model(&bill.model).into_owned();
    // 订阅 provider 额外带上客户端身份头（真实 Claude Code / Codex CLI 经网关出去时上游看到它自己）
    let mut outbound = super::super::oauth_cred::outbound_with_client(
        cand,
        &bill.client_headers,
        Some(bill.user_id),
    );
    override_headers.apply_to(&mut outbound.extra_headers);

    let endpoint = plan.endpoint_for(cand, stream);
    let call_model = if matches!(
        plan.operation,
        Operation::Responses | Operation::ResponsesCompact
    ) && plan.advanced_route_index.is_none()
    {
        ""
    } else {
        upstream_model.as_str()
    };
    let call_stream = plan.operation != Operation::ResponsesCompact && stream;

    let raw = bill
        .state
        .infer_plan(
            plan,
            super::super::dialect::InferCall {
                cand,
                endpoint: endpoint.as_ref(),
                base,
                model: call_model,
                body,
                stream: call_stream,
                outbound: &outbound,
            },
        )
        .await?;
    let resp = plan
        .converter
        .convert_response(raw, &upstream_model, cand.thinking_to_content)?;
    if matches!(bill.ingress, Ingress::Responses | Ingress::ResponsesCompact) && native_responses {
        bill.trace.finish(None, None);
    }
    Ok(resp)
}

/// chat 形状 → Responses 方言（委托给协议转换注册表，供单测复用）。
#[cfg(test)]
pub(super) fn wrap_responses_egress(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    okapi_providers::convert::registry::wrap_chat_as_responses(resp, upstream_model)
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
