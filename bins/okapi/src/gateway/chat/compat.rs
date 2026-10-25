//! 适配规则在聊天链路里的接入（设计见 docs/compat-rules.md）。
//!
//! 请求阶段挂在 `shape_upstream_body`：对**已转成上游方言、发送之前**的那份 JSON 执行。
//! 每次尝试都会整形，failover 到下一个候选时从原始请求重新构造，所以规则必须幂等（引擎保证）。
//! 规则只看这里组装的 [`Context`]，看不到价簿、预扣和凭证——计费域与适配域互不相通。

use super::RequestBilling;
use crate::gateway::execution_plan::{ExecutionPlan, Operation};
use bytes::Bytes;
use okapi_compat::{Context, Report};
use okapi_providers::UpstreamError;
use okapi_store::ChannelCandidate;
use serde_json::json;

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
    reqwest::Url::parse(api_base?)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
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
    record(bill, cand, &outcome.report);
    Ok(outcome.body.map_or(body, Bytes::from))
}

fn record(bill: &RequestBilling, cand: &ChannelCandidate, report: &Report) {
    if report.fired.is_empty() && report.skipped.is_empty() {
        return;
    }
    let versions: Vec<&str> = cand
        .compat
        .layers()
        .iter()
        .map(|layer| layer.label.as_str())
        .collect();
    // 多次尝试（重试、failover）共用一条记录，以最后一次有动静的为准；带上渠道 id 才分得清是哪次
    bill.trace.set(
        "compat",
        json!({
            "channel_id": cand.channel_id,
            "versions": versions,
            "fired": report.fired,
            "skipped": report.skipped,
        }),
    );
}
