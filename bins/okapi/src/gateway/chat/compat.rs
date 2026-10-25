//! 适配规则在聊天链路里的接入（设计见 docs/compat-rules.md）。
//!
//! - 请求阶段挂在 `shape_upstream_body`：对**已转成上游方言、发送之前**的那份 JSON 执行。
//!   每次尝试都会整形，failover 到下一个候选时从原始请求重新构造，所以规则必须幂等（引擎保证）。
//! - 响应阶段包在 `openai_chat` 的调用上（只接 OpenAI chat 方言的上游），在转回客户端方言**之前**；
//!   非流式整体改写后重算 usage，流式逐帧改写后用与提供者相同的 `parse_event` 重算首字 / 字符数 / usage 元数据。
//! - 错误阶段挂在 `try_model` 的重试循环里：上游返回了网关不替规则决定去向的那类 HTTP 错误时先问规则。
//! - 能力阶段不在这里：候选加载时已经合并进 `ChannelCandidate.capabilities`（store 的 `candidate_capabilities`）。
//!
//! 规则只看这里组装的 [`Context`]，看不到价簿、预扣和凭证——计费域与适配域互不相通。

use super::RequestBilling;
use crate::gateway::execution_plan::{ExecutionPlan, Operation};
use bytes::Bytes;
use futures::StreamExt as _;
use okapi_compat::{Action, Context, Phase, Report};
use okapi_providers::{ChatEvent, ChatResponse, StreamHandle, UpstreamError};
use okapi_store::ChannelCandidate;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};

/// 适配规则选择器里的上游方言词汇。
#[must_use]
pub(crate) const fn dialect_name(operation: Operation) -> &'static str {
    match operation {
        Operation::ChatCompletions => "openai.chat",
        Operation::Messages => "anthropic.messages",
        Operation::GenerateContent => "gemini.generate",
        Operation::Responses | Operation::ResponsesCompact => "openai.responses",
    }
}

/// 适配规则选择器里的操作词汇。
#[must_use]
pub(crate) const fn operation_name(operation: Operation) -> &'static str {
    match operation {
        Operation::ChatCompletions | Operation::Messages | Operation::GenerateContent => "chat",
        Operation::Responses | Operation::ResponsesCompact => "responses",
    }
}

/// 渠道 `api_base` 的主机名（小写），选择器的 `host` 用。
pub(crate) fn host_of(api_base: Option<&str>) -> Option<String> {
    okapi_store::compat::host_of(api_base)
}

/// 一次请求在某个候选上的规则上下文。`host` 由调用方持有（借用要活得比 Context 久）。
pub(crate) fn context<'a>(
    cand: &'a ChannelCandidate,
    operation: Operation,
    model: &'a str,
    requested_model: &'a str,
    stream: bool,
    client: &'static str,
    host: Option<&'a str>,
) -> Context<'a> {
    Context {
        vendor: cand.vendor.as_deref(),
        host,
        model: cand.upstream_model(model),
        client_model: requested_model,
        dialect: dialect_name(operation),
        operation: operation_name(operation),
        stream,
        client: Some(client).filter(|c| !c.is_empty()),
    }
}

/// 请求阶段：对发往上游的体执行适配规则。没有规则被选中时原样返回（不解析、不分配）。
/// 命中或跳过的规则轨迹写进诊断（`compat`：渠道 id、用到的各层版本、命中 / 跳过的规则 id）。
pub(super) fn apply(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    plan: ExecutionPlan,
    body: Bytes,
) -> Result<Bytes, UpstreamError> {
    if cand.compat.is_empty() {
        return Ok(body);
    }
    let host = host_of(cand.api_base.as_deref());
    let ctx = context(
        cand,
        plan.operation,
        &bill.model,
        &bill.requested_model,
        bill.is_stream,
        bill.client_type,
        host.as_deref(),
    );
    let outcome = cand
        .compat
        .apply_request(&ctx, &body)
        .map_err(|failure| UpstreamError::Build(failure.to_string()))?;
    record(bill, cand, "compat", &outcome.report);
    Ok(outcome.body.map_or(body, Bytes::from))
}

fn versions(cand: &ChannelCandidate) -> Vec<String> {
    cand.compat
        .layers()
        .iter()
        .map(|layer| layer.label.clone())
        .collect()
}

fn record(bill: &RequestBilling, cand: &ChannelCandidate, key: &'static str, report: &Report) {
    if report.fired.is_empty() && report.skipped.is_empty() {
        return;
    }
    // 多次尝试（重试、failover）共用一条记录，以最后一次有动静的为准；带上渠道 id 才分得清是哪次
    bill.trace.set(
        key,
        json!({
            "channel_id": cand.channel_id,
            "versions": versions(cand),
            "fired": report.fired,
            "skipped": report.skipped,
        }),
    );
}

// ---------------------------------------------------------------------------------------------
// 响应阶段

/// 流式改写要带进 `'static` 闭包的上下文（拥有型；每帧借出一份 [`Context`]）。
struct OwnedContext {
    vendor: Option<String>,
    host: Option<String>,
    model: String,
    client_model: String,
    dialect: String,
    operation: String,
    stream: bool,
    client: Option<String>,
}

impl OwnedContext {
    fn of(ctx: &Context<'_>) -> Self {
        Self {
            vendor: ctx.vendor.map(str::to_owned),
            host: ctx.host.map(str::to_owned),
            model: ctx.model.to_owned(),
            client_model: ctx.client_model.to_owned(),
            dialect: ctx.dialect.to_owned(),
            operation: ctx.operation.to_owned(),
            stream: ctx.stream,
            client: ctx.client.map(str::to_owned),
        }
    }

    fn as_context(&self) -> Context<'_> {
        Context {
            vendor: self.vendor.as_deref(),
            host: self.host.as_deref(),
            model: &self.model,
            client_model: &self.client_model,
            dialect: &self.dialect,
            operation: &self.operation,
            stream: self.stream,
            client: self.client.as_deref(),
        }
    }
}

/// 流式响应的命中汇总：每条规则第一次命中时、之后每 64 次命中刷新一次诊断记录。
struct StreamTrace {
    trace: crate::gateway::diagnostics::Trace,
    channel_id: i64,
    versions: Vec<String>,
    rules: BTreeSet<String>,
    frames: u64,
}

impl StreamTrace {
    fn note(&mut self, report: &Report) {
        if report.fired.is_empty() {
            return;
        }
        self.frames += 1;
        let before = self.rules.len();
        self.rules
            .extend(report.fired.iter().map(|fired| fired.rule.clone()));
        if self.rules.len() > before || self.frames.is_multiple_of(64) {
            self.trace.set(
                "compat_response",
                json!({
                    "channel_id": self.channel_id,
                    "versions": self.versions,
                    "fired": self.rules,
                    "frames": self.frames,
                }),
            );
        }
    }
}

/// 响应阶段：包住 OpenAI chat 方言上游的响应（转回客户端方言之前）。没有响应规则、
/// 或没有规则被这次请求选中时原样返回——不解析、不分配。
pub(super) fn shape_response(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    plan: ExecutionPlan,
    resp: ChatResponse,
) -> ChatResponse {
    if !cand.compat.has_phase(Phase::Response) {
        return resp;
    }
    let host = host_of(cand.api_base.as_deref());
    let ctx = context(
        cand,
        plan.operation,
        &bill.model,
        &bill.requested_model,
        bill.is_stream,
        bill.client_type,
        host.as_deref(),
    );
    if !cand.compat.selects_response(&ctx) {
        return resp;
    }
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        } => {
            let outcome = cand.compat.apply_response(&ctx, &body);
            record(bill, cand, "compat_response", &outcome.report);
            let Some(rewritten) = outcome.body else {
                return ChatResponse::Json {
                    status,
                    upstream_request_id,
                    body,
                    usage,
                };
            };
            // 用量要按改写后的文档重新读：规则改了 usage（usage_map）账单才看得到
            let usage = serde_json::from_slice::<Value>(&rewritten)
                .ok()
                .and_then(|doc| okapi_api::usage_from_chat(&doc));
            ChatResponse::Json {
                status,
                upstream_request_id,
                body: Bytes::from(rewritten),
                usage,
            }
        }
        ChatResponse::Stream(handle) => {
            let compat = std::sync::Arc::clone(&cand.compat);
            let owned = OwnedContext::of(&ctx);
            let mut tally = StreamTrace {
                trace: bill.trace.clone(),
                channel_id: cand.channel_id,
                versions: versions(cand),
                rules: BTreeSet::new(),
                frames: 0,
            };
            let events = handle.events.map(move |item| {
                item.map(|event| rewrite_frame(&compat, &owned.as_context(), event, &mut tally))
            });
            ChatResponse::Stream(StreamHandle {
                upstream_request_id: handle.upstream_request_id,
                events: Box::pin(events),
            })
        }
    }
}

/// 改写一帧：不是 JSON 或没有规则命中就原样放行；命中则重新序列化并重算元数据。
fn rewrite_frame(
    compat: &okapi_compat::EffectiveRules,
    ctx: &Context<'_>,
    event: ChatEvent,
    tally: &mut StreamTrace,
) -> ChatEvent {
    let ChatEvent::Data { raw, .. } = &event else {
        return event;
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(raw) else {
        return event;
    };
    let report = compat.apply_response_value(ctx, &mut doc);
    tally.note(&report);
    if report.fired.is_empty() {
        return event;
    }
    match serde_json::to_string(&doc) {
        Ok(text) => okapi_providers::openai::parse_event(&text),
        // 序列化一个刚解析出来的 Value 不会失败；万一失败，宁可放行原帧
        Err(_) => event,
    }
}

// ---------------------------------------------------------------------------------------------
// 错误阶段

/// 全部错误规则加起来，同一个候选上最多按规则重发几次（每条规则自己还有 `max`）。
const MAX_RULE_RETRIES: u8 = 3;

/// 规则对一次上游错误的处理决定（已经是拥有型，调用方可以随意改 `result`）。
#[derive(Debug)]
pub(super) enum ErrorDecision {
    /// 用改写后的请求体在同一候选上重发。
    Resend(Bytes),
    /// 当作首字前失败，换下一个候选；带 `cooldown_secs` 则让这把 key 冷却那么久。
    Failover { cooldown_secs: Option<u32> },
    /// 改写返回给客户端的错误。
    Fail {
        status: Option<u16>,
        code: Option<String>,
    },
}

/// 同一个候选上已经按规则重发了几次（按规则 id 记）。
#[derive(Debug, Default)]
pub(super) struct RuleRetries {
    by_rule: HashMap<String, u8>,
    total: u8,
}

/// 错误阶段：上游返回了不可重试的 HTTP 错误（`status` + `body`），按规则决定怎么办。
/// `sent` 是刚发出去的请求体（`retry_same` 在它基础上改）。没有规则命中、或命中的规则已用完
/// 重发次数、或改完没有变化（重发只会得到同一个错误）时返回 `None`，走网关原来的处理。
pub(super) fn react_to_error(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    status: u16,
    body: &Bytes,
    sent: &Bytes,
    retries: &mut RuleRetries,
) -> Option<ErrorDecision> {
    if !cand.compat.has_phase(Phase::Error) {
        return None;
    }
    let plan = ExecutionPlan::compile(
        bill.ingress,
        cand,
        &bill.model,
        crate::gateway::execution_plan::Requirements::default(),
    )
    .ok()?;
    let host = host_of(cand.api_base.as_deref());
    let ctx = context(
        cand,
        plan.operation,
        &bill.model,
        &bill.requested_model,
        bill.is_stream,
        bill.client_type,
        host.as_deref(),
    );
    // 命中的规则按执行顺序挑第一条「能用」的：重发还有次数且改完确实有变化，或是换渠道 / 改写错误
    let (reaction, decision) = cand
        .compat
        .reactions(&ctx, status, body)
        .into_iter()
        .find_map(|reaction| {
            let decision = match reaction.action {
                Action::RetrySame { mutation, max } => {
                    let used = retries.by_rule.get(reaction.rule).copied().unwrap_or(0);
                    if used >= max || retries.total >= MAX_RULE_RETRIES {
                        return None;
                    }
                    let resend = mutation.apply(sent)?;
                    retries.by_rule.insert(reaction.rule.to_owned(), used + 1);
                    retries.total += 1;
                    ErrorDecision::Resend(Bytes::from(resend))
                }
                Action::Failover { cooldown_secs } => ErrorDecision::Failover { cooldown_secs },
                Action::Fail { status, code } => ErrorDecision::Fail {
                    status,
                    code: code.map(str::to_owned),
                },
            };
            Some((reaction, decision))
        })?;
    let mut summary = reaction.summary();
    summary["channel_id"] = json!(cand.channel_id);
    summary["upstream_status"] = json!(status);
    summary["versions"] = json!(versions(cand));
    bill.trace.set("compat_error", summary);
    Some(decision)
}

/// 把 `fail` 动作落到要返回的错误上：给了 `code` 就换成网关的稳定错误壳（只带码，由客户端按语言包渲染），
/// 否则保留上游错误体、只换状态码。`error_code` 是计费 / 日志里记的稳定码，跟着一起换。
pub(super) fn rewrite_failure(
    reply: &mut super::FailureReply,
    error_code: &mut String,
    status: Option<u16>,
    code: Option<String>,
) {
    use super::FailureReply;
    let current = match reply {
        FailureReply::Upstream { status, .. } => Some(*status),
        FailureReply::App(error) => Some(error.status.as_u16()),
    };
    match code {
        Some(code) => {
            let http = status
                .or(current)
                .and_then(|s| axum::http::StatusCode::from_u16(s).ok())
                .unwrap_or(axum::http::StatusCode::BAD_REQUEST);
            *reply = FailureReply::App(crate::gateway::error::AppError::new(http, &code));
            *error_code = code;
        }
        None => {
            if let (
                FailureReply::Upstream {
                    status: reply_status,
                    ..
                },
                Some(new),
            ) = (reply, status)
            {
                *reply_status = new;
            }
        }
    }
}
