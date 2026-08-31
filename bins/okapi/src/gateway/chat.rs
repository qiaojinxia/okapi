//! /v1/chat/completions 与 /v1/messages：鉴权 → 估价预扣 → 渠道 failover 转发
//! （首字前缓冲）→ 结算。时序对齐 IMPLEMENTATION §2.2；SSE 语义对齐 §3.7。
//! 入口协议 × 渠道协议 四象限（§4.4）：转换在 providers::convert，泵送与结算无感。

use super::clients::detect_client_type;
use super::error::AppError;
use super::estimate::{estimate_completion_tokens, estimate_prompt_tokens};
use super::sched_redis::session_hash;
use super::scheduler::order_candidates;
use super::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use okapi_api::{ChatRequestProbe, MessagesRequestProbe, ResponsesRequestProbe, UsageProbe, codes};
use okapi_domain::{BillingState, GroupCode, ModelCode, Money, TokenUsage, UserId};
use okapi_ledger::{CommitOutcome, LimitCaps, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, PriceBook, Quote, RatioFp, calculate};
use okapi_providers::convert::{
    anthropic_to_openai as conv_a2o, openai_to_anthropic as convert, openai_to_gemini as conv_gem,
    responses_to_chat as conv_resp,
};
use okapi_providers::reasoning::{self, ReasoningDirective};
use okapi_providers::{
    ChatEvent, ChatResponse, StreamHandle, UpstreamError, rewrite_model, split_reasoning_suffix,
};
use okapi_store::ChannelCandidate;
use okapi_store::channels::KeyFailure;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_ANTHROPIC_BASE: &str = "https://api.anthropic.com/v1";
const DEFAULT_GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
/// 首字窗口（连接 + 首个产出事件）；窗口内失败可无痕 failover。
const FIRST_OUTPUT_TIMEOUT: Duration = Duration::from_secs(30);
/// 单请求最多尝试的渠道 key 数。
const MAX_ATTEMPTS: usize = 3;
/// 预扣补全上限的兜底值与硬顶。
const DEFAULT_COMPLETION_CAP: u32 = 2048;
const MAX_COMPLETION_CAP: u32 = 32_768;

/// 入口协议（客户端说的方言）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ingress {
    OpenAi,
    Anthropic,
    /// OpenAI Responses API（降级 ChatCompletions 执行，§4.4 #5209）。
    Responses,
}

/// 入口探针归一化结果（两种协议解析为同一形状，主链路协议无关）。
struct ProbeInfo {
    /// 客户端请求的模型名（别名解析前；透传重写比对用）。
    requested_model: String,
    stream: bool,
    /// 显式请求的补全上限（openai: max_completion_tokens>max_tokens；anthropic: max_tokens）。
    completion_cap_req: Option<u32>,
    prompt_chars: usize,
    message_count: usize,
    /// L2 会话标识（头优先，缺省消息前缀哈希）。
    session: Option<String>,
    /// 请求特征（能力感知路由输入，§3.8）。
    needs_tools: bool,
    needs_vision: bool,
    /// OpenAI service_tier 请求声明（tier 计费轴；anthropic/responses 入口无此概念）。
    service_tier: Option<String>,
}

/// 每请求计费上下文（转发与异步结算共享）。
#[derive(Clone)]
struct RequestBilling {
    state: AppState,
    ingress: Ingress,
    book: Arc<PriceBook>,
    calc: CalcContext,
    user_id: i64,
    key_id: i64,
    /// 团 key 归属成员（结算后累计月度消费）。
    member_user_id: Option<i64>,
    request_id: Uuid,
    est_prompt: u32,
    /// 预扣补全上限（anthropic 转换的 max_tokens 兜底也用它）。
    completion_cap: u32,
    /// reasoning 模型名后缀指令（-high/-thinking-N，注入上游请求参数）。
    directive: Option<ReasoningDirective>,
    /// canonical 模型名（别名解析后；记账与调度用）。
    model: String,
    group: String,
    is_stream: bool,
    started: Instant,
    /// 会话标识（L2 粘性键）。
    session: Option<String>,
    /// UA 识别的客户端类型。
    client_type: &'static str,
    /// 客户端 IP（CDN 头按序，§14.2）。
    client_ip: Option<String>,
    /// 渠道可见性组（用户全部组并集，§6.3）。
    visibility: Vec<String>,
    /// 请求声明的 service_tier（预扣按此档估；结算只降不升，DESIGN §3-4.5）。
    service_tier: Option<String>,
    /// 模型是否配置了档位倍率（据此决定是否采集响应档位）。
    has_tier_pricing: bool,
}

enum FailureReply {
    App(AppError),
    /// 400 类上游错误原样转译返回（§3.6：不计费、不重试）。
    Upstream {
        status: u16,
        body: Bytes,
    },
}

struct ForwardFailure {
    reply: FailureReply,
    error_code: String,
    upstream_status: Option<i16>,
    failover_count: i16,
    channel: Option<(i64, i64)>,
}

impl ForwardFailure {
    fn app(err: AppError, failover: i16, channel: Option<(i64, i64)>) -> Self {
        Self {
            error_code: err.code.clone(),
            upstream_status: None,
            failover_count: failover,
            channel,
            reply: FailureReply::App(err),
        }
    }
}

enum AttemptError {
    /// 首字前失败：可换渠道重试；failure_kind 驱动 key 状态机（§3.4）。
    Retriable {
        code: &'static str,
        upstream_status: Option<i16>,
        failure_kind: KeyFailure,
    },
    /// 不可重试：立即向客户端返回。
    Fatal(ForwardFailure),
}

/// 上游错误 → key 状态机类别（§3.6 重试矩阵）。
/// 注意顺序：insufficient_quota 判定先于 429——OpenAI 风格的
/// `429 + insufficient_quota` 语义是配额耗尽（冷却到次日），不是限速（60s）。
fn failure_kind_of(err: &UpstreamError) -> KeyFailure {
    match err {
        UpstreamError::Status { status, body, .. }
            if *status == 402 || body_says_insufficient_quota(body) =>
        {
            KeyFailure::QuotaExhausted
        }
        UpstreamError::Status { status: 429, .. } => KeyFailure::RateLimited {
            retry_after_secs: err.retry_after_secs(),
        },
        UpstreamError::Status {
            status: 401 | 403, ..
        } => KeyFailure::Invalid,
        UpstreamError::Status { .. }
        | UpstreamError::Connect(_)
        | UpstreamError::Timeout
        | UpstreamError::Stream(_)
        | UpstreamError::Build(_) => KeyFailure::Transient,
    }
}

fn body_says_insufficient_quota(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let err = v.get("error")?;
            let code = err.get("code").and_then(|c| c.as_str()).unwrap_or("");
            let kind = err.get("type").and_then(|t| t.as_str()).unwrap_or("");
            Some(code.contains("insufficient_quota") || kind.contains("insufficient_quota"))
        })
        .unwrap_or(false)
}

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let Ok(probe) = serde_json::from_slice::<ChatRequestProbe>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    let (needs_tools, needs_vision) = request_features(Ingress::OpenAi, &body);
    let info = ProbeInfo {
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.max_completion_tokens.or(probe.max_tokens),
        prompt_chars: probe.prompt_chars(),
        message_count: probe.messages.len(),
        session: session_hash(&headers, &probe.messages),
        service_tier: probe.service_tier.clone(),
        needs_tools,
        needs_vision,
    };
    match handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::OpenAi,
        &info,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// OpenAI /v1/responses 入口（§4.4 #5209：降级 ChatCompletions 执行）。
pub async fn responses(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let Ok(probe) = serde_json::from_slice::<ResponsesRequestProbe>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    let input_messages = probe.input_messages();
    let (needs_tools, needs_vision) = request_features(Ingress::Responses, &body);
    let info = ProbeInfo {
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.completion_cap_req(),
        prompt_chars: probe.prompt_chars(),
        message_count: input_messages.len().max(1),
        session: session_hash(&headers, &input_messages),
        service_tier: None,
        needs_tools,
        needs_vision,
    };
    match handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::Responses,
        &info,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// Anthropic /v1/messages 入口（§4.4：入口协议 + 上游方向双向）。
pub async fn messages(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let Ok(probe) = serde_json::from_slice::<MessagesRequestProbe>(&body) else {
        return AppError::bad_request().into_anthropic_response_with(Some(request_id));
    };
    let (needs_tools, needs_vision) = request_features(Ingress::Anthropic, &body);
    let info = ProbeInfo {
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.max_tokens,
        prompt_chars: probe.prompt_chars(),
        message_count: probe.messages.len(),
        session: session_hash(&headers, &probe.messages),
        service_tier: None,
        needs_tools,
        needs_vision,
    };
    match handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::Anthropic,
        &info,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_anthropic_response_with(Some(request_id)),
    }
}

/// 请求特征探测（§3.8 能力感知路由）：tools 数组非空 / 消息含图像部件。
fn request_features(ingress: Ingress, body: &Bytes) -> (bool, bool) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (false, false);
    };
    let needs_tools = v
        .get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty());
    let image_types: &[&str] = match ingress {
        Ingress::OpenAi => &["image_url"],
        Ingress::Anthropic => &["image"],
        Ingress::Responses => &["input_image"],
    };
    let containers = match ingress {
        Ingress::Responses => v.get("input"),
        _ => v.get("messages"),
    };
    let needs_vision = containers
        .and_then(|m| m.as_array())
        .is_some_and(|messages| {
            messages.iter().any(|msg| {
                msg.get("content")
                    .and_then(|c| c.as_array())
                    .is_some_and(|parts| {
                        parts.iter().any(|p| {
                            p.get("type")
                                .and_then(|t| t.as_str())
                                .is_some_and(|t| image_types.contains(&t))
                        })
                    })
            })
        });
    (needs_tools, needs_vision)
}

/// 模型解析 + reasoning 后缀（§4.4）：全名（含别名）直命中优先，
/// 未命中剥 `-high/-medium/-low/-thinking[-N]` 后缀以基名重试。
async fn resolve_with_directive(
    state: &AppState,
    requested: &str,
) -> Result<
    Option<(
        okapi_store::channels::ResolvedModel,
        Option<ReasoningDirective>,
    )>,
    AppError,
> {
    if let Some(meta) = resolve_model_cached(state, requested).await?.as_ref() {
        return Ok(Some((meta.clone(), None)));
    }
    if let Some((base, directive)) = split_reasoning_suffix(requested)
        && let Some(meta) = resolve_model_cached(state, base).await?.as_ref()
    {
        return Ok(Some((meta.clone(), Some(directive))));
    }
    Ok(None)
}

/// 模型解析（60s 进程缓存；miss 回源 PG）。
pub(crate) async fn resolve_model_cached(
    state: &AppState,
    requested: &str,
) -> Result<Arc<Option<okapi_store::channels::ResolvedModel>>, AppError> {
    if let Some(hit) = state.model_cache.get(requested).await {
        return Ok(hit);
    }
    let resolved = Arc::new(okapi_store::channels::resolve_model(&state.pg, requested).await?);
    state
        .model_cache
        .insert(requested.to_owned(), Arc::clone(&resolved))
        .await;
    Ok(resolved)
}

// 鉴权→解析→估价→预扣→转发的主链路，拆分损害时序可读性
#[allow(clippy::too_many_lines)]
async fn handle_chat(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: Uuid,
    started: Instant,
    ingress: Ingress,
    info: &ProbeInfo,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate(state, headers).await?;

    // 模型解析（#3001 + §5.1）：别名→canonical + max_output；60s 进程缓存消除热路径 PG 读；
    // 未命中剥 reasoning 后缀重试（§4.4）
    let Some((meta, directive)) = resolve_with_directive(state, &info.requested_model).await?
    else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND));
    };
    let canonical = meta.canonical.clone();
    if !key.allows_model(&canonical) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            codes::MODEL_NOT_ALLOWED,
        ));
    }

    let book = state.pricebook.load();
    let rules_in = super::rule_inputs::collect(state, &book, key.user_id).await;
    let now = chrono::Utc::now();
    let minute_of_day =
        u16::try_from((now.timestamp().div_euclid(60)).rem_euclid(1440)).unwrap_or(0);
    let calc = CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(canonical.as_str()),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: rules_in.monthly_tokens,
        local_minute_of_day: minute_of_day,
        now_unix: now.timestamp(),
        surge_active: rules_in.surge_active,
        // 预扣按请求声明档估（贵档多预扣；结算档只降不升另选）
        service_tier: info.service_tier.clone(),
    };

    // 估价（预扣补全缺省 = models.max_output，无则 2048，§5.1）
    let est_prompt = estimate_prompt_tokens(info.prompt_chars, info.message_count);
    let model_default_cap = meta
        .max_output
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(DEFAULT_COMPLETION_CAP);
    let completion_cap = info
        .completion_cap_req
        .unwrap_or(model_default_cap)
        .min(MAX_COMPLETION_CAP);
    let est_usage = TokenUsage {
        prompt_tokens: est_prompt,
        cached_tokens: 0,
        cache_write_tokens: 0,
        audio_prompt_tokens: 0,
        image_prompt_tokens: 0,
        completion_tokens: completion_cap,
        audio_completion_tokens: 0,
        reasoning_tokens: 0,
    };
    let est_quote = calculate(&book, &calc, est_usage)?;

    // 团成员月度限额（§6.1 软实时）
    super::auth::check_member_limit(state, &key).await?;
    // 用户×模型 RPM（settings.model_rpm_limits，全局按用户；§11.1）
    let model_limits = state.setting_cached("model_rpm_limits").await;
    if let Some(limit) = model_limits
        .as_ref()
        .as_ref()
        .and_then(|v| v.get(&canonical))
        .and_then(serde_json::Value::as_i64)
        .filter(|v| *v > 0)
        && !state
            .sched
            .model_rate_ok(key.user_id, &canonical, limit)
            .await
    {
        return Err(
            AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED)
                .with_param("model_rpm"),
        );
    }

    // 预扣（余额 + RPM/TPM/RPD + 并发，单 Lua 原子）
    let cap = |v: Option<i32>| v.map_or(0, i64::from);
    let caps = LimitCaps {
        rpm: cap(key.rpm_limit),
        tpm: cap(key.tpm_limit),
        rpd: cap(key.rpd_limit),
        concurrency: cap(key.max_concurrency),
    };
    let est_tokens = u64::from(est_prompt).saturating_add(u64::from(completion_cap));
    match state
        .ledger
        .reserve(
            okapi_ledger::ReserveRequest {
                user_id: key.user_id,
                api_key_id: key.key_id,
                request_id,
                est: est_quote.amount,
                caps,
                est_tokens,
            },
            now,
        )
        .await?
    {
        ReserveOutcome::Reserved { .. } => {}
        ReserveOutcome::Insufficient { .. } => {
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                codes::INSUFFICIENT_QUOTA,
            ));
        }
        ReserveOutcome::RateLimited { which } => {
            return Err(
                AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED).with_param(which),
            );
        }
    }

    // —— 预扣已建立：此后一切失败路径必须退款（settle_failure）——
    let bill_model_for_tier = canonical.clone();
    let bill = RequestBilling {
        state: state.clone(),
        ingress,
        book: Arc::clone(&book),
        calc,
        user_id: key.user_id,
        key_id: key.key_id,
        member_user_id: key.member_user_id,
        request_id,
        est_prompt,
        completion_cap,
        directive,
        model: canonical,
        group: key.group_code.clone(),
        is_stream: info.stream,
        started,
        session: info.session.clone(),
        client_type: detect_client_type(headers),
        client_ip: super::clients::detect_client_ip(headers),
        visibility: key.visibility_groups.clone(),
        service_tier: info.service_tier.clone(),
        has_tier_pricing: book.has_tiers(&ModelCode::from(bill_model_for_tier.as_str())),
    };

    match forward(&bill, info, body).await {
        Ok(resp) => Ok(resp),
        Err(failure) => {
            settle_failure(&bill, &failure).await;
            match failure.reply {
                FailureReply::App(err) => Err(err),
                FailureReply::Upstream { status, body } => Ok(upstream_passthrough_response(
                    ingress, status, body, request_id,
                )),
            }
        }
    }
}

// failover 主循环：候选过滤/粘性/信号量/状态机联动的完整语义在同一视野内更可读
#[allow(clippy::too_many_lines)]
async fn forward(
    bill: &RequestBilling,
    info: &ProbeInfo,
    body: &Bytes,
) -> Result<Response, ForwardFailure> {
    // 候选 5s 进程缓存（热路径零 PG 读；console 写路径主动失效，多副本靠 TTL 收敛）
    let mut vis_key = bill.visibility.clone();
    vis_key.sort_unstable();
    let cache_key = format!("{}|{}", bill.model, vis_key.join(","));
    let raw = if let Some(hit) = bill.state.cand_cache.get(&cache_key).await {
        hit
    } else {
        let rows = okapi_store::channels::candidates_for_model(
            &bill.state.pg,
            &bill.model,
            &bill.visibility,
        )
        .await
        .map_err(|e| ForwardFailure::app(AppError::from(e), 0, None))?;
        let rows = Arc::new(rows);
        bill.state
            .cand_cache
            .insert(cache_key, Arc::clone(&rows))
            .await;
        rows
    };
    let mut candidates = order_candidates(raw.as_ref().clone());
    // Anthropic 入口暂不路由 gemini 渠道（不做 anthropic→openai→gemini 双跳）
    if bill.ingress == Ingress::Anthropic {
        candidates.retain(|c| c.provider != "gemini");
    }
    // 能力感知路由（§3.8）：渠道显式声明 false 才排除
    let denies = |c: &okapi_store::ChannelCandidate, cap: &str| {
        c.capabilities.get(cap).and_then(serde_json::Value::as_bool) == Some(false)
    };
    if info.needs_tools {
        candidates.retain(|c| !denies(c, "tools"));
    }
    if info.needs_vision {
        candidates.retain(|c| !denies(c, "vision"));
    }
    if candidates.is_empty() {
        return Err(ForwardFailure::app(
            AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL),
            0,
            None,
        ));
    }

    // L2 会话粘性命中：把映射的 channel_key 提到候选首位（§3.2）
    let mut sticky_key: Option<i64> = None;
    if let Some(session) = &bill.session {
        sticky_key = bill.state.sched.sticky_get(bill.user_id, session).await;
        if let Some(kid) = sticky_key
            && let Some(pos) = candidates.iter().position(|c| c.channel_key_id == kid)
        {
            let hit = candidates.remove(pos);
            candidates.insert(0, hit);
        }
    }

    let mut failover: i16 = 0;
    let mut attempted = 0usize;
    let mut last_code: &'static str = codes::UPSTREAM_ERROR;
    let mut last_status: Option<i16> = None;
    let mut last_channel: Option<(i64, i64)> = None;

    for cand in candidates {
        if attempted >= MAX_ATTEMPTS {
            break;
        }
        // 渠道 key 级并发信号量（§3.5 第二层）：满则跳过该候选（不计 failover）
        if !bill
            .state
            .sched
            .acquire_slot(cand.channel_key_id, cand.max_concurrency)
            .await
        {
            tracing::debug!(channel_key = cand.channel_key_id, "并发已满，跳过候选");
            continue;
        }
        attempted += 1;

        let upstream_model = cand.upstream_model(&bill.model).to_owned();
        // 入口协议 × 渠道协议：同方言重写 model 透传，跨方言走出向转换；
        // Responses 恒先降级 chat，再按渠道协议二段转换
        let body_built = match (bill.ingress, cand.provider.as_str()) {
            (Ingress::OpenAi, "anthropic") => {
                convert::request_openai_to_anthropic(body, &upstream_model, bill.completion_cap)
            }
            (Ingress::OpenAi, "gemini") => conv_gem::request_openai_to_gemini(body),
            (Ingress::Anthropic, "anthropic") | (Ingress::OpenAi, _) => {
                rewrite_model(body, &info.requested_model, &upstream_model)
            }
            (Ingress::Anthropic, _) => conv_a2o::request_anthropic_to_openai(body, &upstream_model),
            (Ingress::Responses, provider) => {
                conv_resp::request_responses_to_chat(body, &upstream_model).and_then(|chat_body| {
                    match provider {
                        "anthropic" => convert::request_openai_to_anthropic(
                            &chat_body,
                            &upstream_model,
                            bill.completion_cap,
                        ),
                        "gemini" => conv_gem::request_openai_to_gemini(&chat_body),
                        _ => Ok(chat_body),
                    }
                })
            }
        };
        // reasoning 后缀注入（按渠道方向；显式字段不覆盖）
        let body_built = match (body_built, bill.directive) {
            (Ok(b), Some(d)) => match cand.provider.as_str() {
                "anthropic" => reasoning::apply_anthropic(&b, d),
                "gemini" => reasoning::apply_gemini(&b, d),
                _ => reasoning::apply_openai(&b, d),
            },
            (built, _) => built,
        };
        let Ok(body_up) = body_built else {
            bill.state
                .sched
                .release_slot(cand.channel_key_id, cand.max_concurrency)
                .await;
            return Err(ForwardFailure::app(
                AppError::bad_request(),
                failover,
                Some((cand.channel_id, cand.channel_key_id)),
            ));
        };
        let base = cand.api_base.clone().unwrap_or_else(|| {
            match cand.provider.as_str() {
                "anthropic" => DEFAULT_ANTHROPIC_BASE,
                "gemini" => DEFAULT_GEMINI_BASE,
                _ => DEFAULT_OPENAI_BASE,
            }
            .to_owned()
        });
        last_channel = Some((cand.channel_id, cand.channel_key_id));
        let sticky_layer: i16 = if sticky_key == Some(cand.channel_key_id) {
            2
        } else {
            3
        };

        // §3.6：连接/超时/5xx 允许同 key 先重试 1 次；空回复直接换渠道
        let mut retry: i16 = 0;
        let attempt = loop {
            let result = if info.stream {
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
            };
            let transient = matches!(
                &result,
                Err(AttemptError::Retriable {
                    failure_kind: KeyFailure::Transient,
                    code,
                    ..
                }) if *code != codes::EMPTY_COMPLETION
            );
            if retry == 0 && transient {
                retry = 1;
                tracing::debug!(
                    channel_key = cand.channel_key_id,
                    "瞬态失败，同 key 重试 1 次"
                );
                continue;
            }
            break result;
        };

        match attempt {
            Ok(resp) => {
                // 成功建立输出：刷新会话粘性映射（信号量由结算路径释放）
                if let Some(session) = &bill.session {
                    bill.state
                        .sched
                        .sticky_set(bill.user_id, session, cand.channel_key_id)
                        .await;
                }
                return Ok(resp);
            }
            Err(AttemptError::Retriable {
                code,
                upstream_status,
                failure_kind,
            }) => {
                bill.state
                    .sched
                    .release_slot(cand.channel_key_id, cand.max_concurrency)
                    .await;
                tracing::warn!(
                    request_id = %bill.request_id,
                    channel_key = cand.channel_key_id,
                    code,
                    "首字前失败，failover 下一候选"
                );
                let _ = okapi_store::channels::mark_key_failure(
                    &bill.state.pg,
                    cand.channel_key_id,
                    code,
                    failure_kind,
                )
                .await;
                failover = failover.saturating_add(1);
                last_code = code;
                last_status = upstream_status;
            }
            Err(AttemptError::Fatal(mut failure)) => {
                bill.state
                    .sched
                    .release_slot(cand.channel_key_id, cand.max_concurrency)
                    .await;
                failure.failover_count = failover;
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

    let err_code = if last_code == codes::EMPTY_COMPLETION {
        codes::EMPTY_COMPLETION
    } else {
        last_code
    };
    let mut failure = ForwardFailure::app(
        AppError::new(StatusCode::BAD_GATEWAY, err_code),
        failover,
        last_channel,
    );
    failure.upstream_status = last_status;
    Err(failure)
}

fn classify_fatal(err: UpstreamError, failover: i16, channel: (i64, i64)) -> AttemptError {
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
            };
            failure.error_code = format!("upstream_status_{status}");
            AttemptError::Fatal(failure)
        }
        UpstreamError::Build(_) => AttemptError::Fatal(ForwardFailure::app(
            AppError::bad_request(),
            failover,
            Some(channel),
        )),
        UpstreamError::Connect(_) | UpstreamError::Timeout | UpstreamError::Stream(_) => {
            AttemptError::Fatal(ForwardFailure::app(
                AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
                failover,
                Some(channel),
            ))
        }
    }
}

/// thinking-to-content 渠道开关：OpenAI 方言出口的 reasoning 转 <think> 正文。
fn wrap_thinking_to_content(resp: ChatResponse) -> ChatResponse {
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
/// 渠道字段透传控制（new-api rc.23 #6847 对齐）：剥除配置的请求顶层字段。
/// 方言无关（对入口原文生效，转换路径自然继承）；`model`/`messages`/`stream`
/// 受保护不可剥（防误配打断主链）。仅配置非空时解析（缺省零开销）。
fn strip_request_fields(body: &Bytes, fields: &[String]) -> Option<Bytes> {
    const PROTECTED: [&str; 3] = ["model", "messages", "stream"];
    let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = value.as_object_mut()?;
    let mut changed = false;
    for field in fields {
        if PROTECTED.contains(&field.as_str()) {
            continue;
        }
        if obj.remove(field).is_some() {
            changed = true;
        }
    }
    changed.then(|| Bytes::from(serde_json::to_vec(&value).unwrap_or_default()))
}

// 入口方言 × 上游协议矩阵的收敛点，拆分损害路由全貌可读性
#[allow(clippy::too_many_lines)]
async fn dispatch_chat(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    stream: bool,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    let body = if cand.strip_request_fields.is_empty() {
        body
    } else {
        strip_request_fields(&body, &cand.strip_request_fields).unwrap_or(body)
    };
    let upstream_model = cand.upstream_model(&bill.model).to_owned();
    let resp = match (bill.ingress, cand.provider.as_str()) {
        // OpenAI/Responses 客户端 + gemini 上游：providers 内转回 OpenAI 形状
        (Ingress::OpenAi | Ingress::Responses, "gemini") => {
            conv_gem::chat(
                &bill.state.gemini,
                base,
                &cand.credential,
                body,
                &upstream_model,
                stream,
            )
            .await
        }
        // OpenAI/Responses 客户端 + anthropic 上游：providers 内转回 OpenAI 形状
        (Ingress::OpenAi | Ingress::Responses, "anthropic") => {
            convert::chat(
                &bill.state.anthropic,
                base,
                &cand.credential,
                body,
                &upstream_model,
                stream,
            )
            .await
        }
        // 同方言 OpenAI：原样
        (Ingress::OpenAi | Ingress::Responses, _) => {
            bill.state
                .upstream
                .chat(base, &cand.credential, body, stream)
                .await
        }
        // Anthropic 客户端 + anthropic 上游：透传 + 计费元数据扫描
        (Ingress::Anthropic, "anthropic") => {
            match bill
                .state
                .anthropic
                .messages(base, &cand.credential, body, stream)
                .await?
            {
                okapi_providers::anthropic::MessagesResponse::Json {
                    status,
                    upstream_request_id,
                    body,
                } => {
                    let usage = serde_json::from_slice::<serde_json::Value>(&body)
                        .ok()
                        .map(|v| convert::usage_from_anthropic(v.get("usage")));
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
        // Anthropic 客户端 + OpenAI(兼容) 上游：回向转换为 Anthropic 事件/JSON
        (Ingress::Anthropic, _) => {
            match bill
                .state
                .upstream
                .chat(base, &cand.credential, body, stream)
                .await?
            {
                ChatResponse::Json {
                    status,
                    upstream_request_id,
                    body,
                    ..
                } => {
                    let (body, usage) = conv_a2o::response_openai_to_anthropic(&body)?;
                    Ok(ChatResponse::Json {
                        status,
                        upstream_request_id,
                        body,
                        usage,
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
fn wrap_responses_egress(
    resp: ChatResponse,
    upstream_model: &str,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    match resp {
        ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            ..
        } => {
            let (body, usage) = conv_resp::response_chat_to_responses(&body)?;
            Ok(ChatResponse::Json {
                status,
                upstream_request_id,
                body,
                usage,
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

// ---- 流式 ----

async fn attempt_stream(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    failover: i16,
    sticky_layer: i16,
    retry: i16,
) -> Result<Response, AttemptError> {
    let channel = (cand.channel_id, cand.channel_key_id);
    let connect = dispatch_chat(bill, cand, base, body, true);
    let resp = match tokio::time::timeout(FIRST_OUTPUT_TIMEOUT, connect).await {
        Err(_) => {
            return Err(AttemptError::Retriable {
                code: codes::UPSTREAM_TIMEOUT,
                upstream_status: None,
                failure_kind: KeyFailure::Transient,
            });
        }
        Ok(Err(err)) => return Err(classify_fatal(err, failover, channel)),
        Ok(Ok(resp)) => resp,
    };
    let ChatResponse::Stream(mut handle) = resp else {
        return Err(AttemptError::Retriable {
            code: codes::UPSTREAM_ERROR,
            upstream_status: None,
            failure_kind: KeyFailure::Transient,
        });
    };

    // 首字前只缓冲：窗口内失败/空回复对客户端无痕（§3.7-1/2）
    let mut buffered: Vec<ChatEvent> = Vec::new();
    let first = tokio::time::timeout(FIRST_OUTPUT_TIMEOUT, async {
        loop {
            match handle.events.next().await {
                Some(Ok(event @ ChatEvent::Data { .. })) => {
                    let has_output = matches!(
                        event,
                        ChatEvent::Data {
                            has_output: true,
                            ..
                        }
                    );
                    buffered.push(event);
                    if has_output {
                        return Ok(true);
                    }
                }
                Some(Ok(ChatEvent::Done)) | None => return Ok(false),
                Some(Err(err)) => return Err(err),
            }
        }
    })
    .await;

    match first {
        Err(_) => Err(AttemptError::Retriable {
            code: codes::UPSTREAM_TIMEOUT,
            upstream_status: None,
            failure_kind: KeyFailure::Transient,
        }),
        Ok(Err(_)) => Err(AttemptError::Retriable {
            code: codes::UPSTREAM_ERROR,
            upstream_status: None,
            failure_kind: KeyFailure::Transient,
        }),
        Ok(Ok(false)) => Err(AttemptError::Retriable {
            code: codes::EMPTY_COMPLETION,
            upstream_status: None,
            failure_kind: KeyFailure::Transient,
        }),
        Ok(Ok(true)) => {
            let ttft_ms = elapsed_ms_i32(bill.started);
            Ok(spawn_stream_pump(
                bill.clone(),
                cand_info(cand, sticky_layer, retry),
                handle,
                buffered,
                ttft_ms,
                failover,
            ))
        }
    }
}

#[derive(Clone)]
struct CandInfo {
    channel: i64,
    key: i64,
    /// key 级并发上限（结算路径释放信号量用）。
    cap: Option<i32>,
    sticky_layer: i16,
    /// 同 key 重试次数（§3.6，记账列 retry_count）。
    retry: i16,
    upstream_request_id: Option<String>,
    /// 渠道开关：按上游响应模型计费（Sub2API 0.1.175 对齐）。
    bill_resp_model: bool,
}

fn cand_info(cand: &ChannelCandidate, sticky_layer: i16, retry: i16) -> CandInfo {
    CandInfo {
        channel: cand.channel_id,
        key: cand.channel_key_id,
        cap: cand.max_concurrency,
        sticky_layer,
        retry,
        upstream_request_id: None,
        bill_resp_model: cand.bill_by_response_model,
    }
}

fn spawn_stream_pump(
    bill: RequestBilling,
    mut info: CandInfo,
    mut handle: StreamHandle,
    buffered: Vec<ChatEvent>,
    ttft_ms: i32,
    failover: i16,
) -> Response {
    info.upstream_request_id = handle.upstream_request_id.take();
    let request_id = bill.request_id;
    let (mut tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);

    // detach 说明：pump 生命周期与上游流绑定；客户端断开经 send 失败感知并取消上游，
    // 结算在任何退出路径都执行（settle_stream）。
    tokio::spawn(async move {
        let mut usage: Option<UsageProbe> = None;
        let mut content_chars: usize = 0;
        let mut client_gone = false;
        // 响应元数据采集：model（渠道 opt-in）/ service_tier（模型配了档位倍率）
        let mut resp_meta = RespMeta::default();
        let want_meta = info.bill_resp_model || bill.has_tier_pricing;

        for event in buffered {
            if want_meta && (resp_meta.model.is_none() || resp_meta.service_tier.is_none()) {
                capture_chunk_meta(&event, &mut resp_meta);
            }
            if !push_event(
                &mut tx,
                &event,
                bill.ingress,
                &mut usage,
                &mut content_chars,
            )
            .await
            {
                client_gone = true;
                break;
            }
        }
        let mut saw_done = false;
        while !client_gone && !saw_done {
            match handle.events.next().await {
                Some(Ok(event)) => {
                    saw_done = matches!(event, ChatEvent::Done);
                    if want_meta && (resp_meta.model.is_none() || resp_meta.service_tier.is_none())
                    {
                        capture_chunk_meta(&event, &mut resp_meta);
                    }
                    if !push_event(
                        &mut tx,
                        &event,
                        bill.ingress,
                        &mut usage,
                        &mut content_chars,
                    )
                    .await
                    {
                        client_gone = true;
                    }
                }
                // 首字后断流：不可回退，按已产出结算（§3.6）
                Some(Err(err)) => {
                    tracing::warn!(request_id = %request_id, error = %err, "首字后断流，按已产出结算");
                    break;
                }
                None => break,
            }
        }
        drop(handle); // 取消上游（客户端断开路径）
        // 立即关闭 SSE 发送端：流结束不等结算（结算耗时曾拖住客户端收尾，
        // 压测定位见 docs/perf-report.md）
        drop(tx);
        settle_stream(
            &bill,
            &info,
            usage,
            content_chars,
            ttft_ms,
            failover,
            client_gone,
            resp_meta,
        )
        .await;
        // 结算完成后释放渠道 key 并发信号量（§3.5）
        bill.state.sched.release_slot(info.key, info.cap).await;
    });

    let sse = Sse::new(rx).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    );
    with_request_id(sse.into_response(), request_id)
}

/// 返回 false 表示客户端已断开。
/// 终止符按入口方言：OpenAI 发 `data: [DONE]`；Anthropic 以 message_stop 事件收尾
/// （已作为 Data 透出），Done 不再发帧。
async fn push_event(
    tx: &mut mpsc::Sender<Result<Event, Infallible>>,
    event: &ChatEvent,
    ingress: Ingress,
    usage: &mut Option<UsageProbe>,
    content_chars: &mut usize,
) -> bool {
    let sse_event = match event {
        ChatEvent::Data {
            raw,
            event: name,
            content_chars: chars,
            usage: ev_usage,
            ..
        } => {
            *content_chars = content_chars.saturating_add(*chars);
            if ev_usage.is_some() {
                *usage = *ev_usage;
            }
            let mut ev = Event::default();
            if let Some(name) = name {
                ev = ev.event(name);
            }
            ev.data(raw)
        }
        ChatEvent::Done => match ingress {
            Ingress::OpenAi => Event::default().data("[DONE]"),
            // Anthropic 以 message_stop、Responses 以 response.completed 收尾，无终止帧
            Ingress::Anthropic | Ingress::Responses => return true,
        },
    };
    tx.send(Ok(sse_event)).await.is_ok()
}

#[allow(clippy::too_many_arguments)]
async fn settle_stream(
    bill: &RequestBilling,
    info: &CandInfo,
    usage: Option<UsageProbe>,
    content_chars: usize,
    ttft_ms: i32,
    failover: i16,
    client_gone: bool,
    resp_meta: RespMeta,
) {
    // usage 缺失（未开 include_usage / 客户端提前断开）→ 兜底估算；
    // trust_upstream_usage=false 的本地 tokenizer 复核在 M2 接入 tiktoken 后启用。
    let usage = usage.map_or_else(
        || TokenUsage {
            prompt_tokens: bill.est_prompt,
            cached_tokens: 0,
            cache_write_tokens: 0,
            audio_prompt_tokens: 0,
            image_prompt_tokens: 0,
            completion_tokens: estimate_completion_tokens(content_chars),
            audio_completion_tokens: 0,
            reasoning_tokens: 0,
        },
        UsageProbe::to_token_usage,
    );
    if client_gone {
        tracing::info!(request_id = %bill.request_id, "客户端提前断开，按已产出结算");
    }
    settle_commit(bill, info, usage, Some(ttft_ms), failover, resp_meta).await;
}

/// 上游响应元数据（按需采集：model 供响应模型计费、service_tier 供档位计费）。
#[derive(Default, Clone)]
struct RespMeta {
    model: Option<String>,
    service_tier: Option<String>,
}

/// 流式 chunk 的响应元数据采集（开关/tier 定价启用时才调用；首个非空值生效）。
fn capture_chunk_meta(event: &ChatEvent, meta: &mut RespMeta) {
    #[derive(serde::Deserialize)]
    struct MetaOnly {
        model: Option<String>,
        service_tier: Option<String>,
    }
    if let ChatEvent::Data { raw, .. } = event
        && let Ok(probe) = serde_json::from_str::<MetaOnly>(raw)
    {
        if meta.model.is_none()
            && let Some(model) = probe.model.filter(|m| !m.is_empty())
        {
            meta.model = Some(model);
        }
        if meta.service_tier.is_none()
            && let Some(tier) = probe.service_tier.filter(|t| !t.is_empty())
        {
            meta.service_tier = Some(tier);
        }
    }
}

/// 非流式 JSON 响应的元数据提取（开关/tier 定价启用时才调用）。
fn extract_body_meta(body: &Bytes) -> RespMeta {
    #[derive(serde::Deserialize)]
    struct MetaOnly {
        model: Option<String>,
        service_tier: Option<String>,
    }
    serde_json::from_slice::<MetaOnly>(body).map_or_else(
        |_| RespMeta::default(),
        |p| RespMeta {
            model: p.model.filter(|m| !m.is_empty()),
            service_tier: p.service_tier.filter(|t| !t.is_empty()),
        },
    )
}

// ---- 非流式 ----

async fn attempt_json(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    failover: i16,
    sticky_layer: i16,
    retry: i16,
) -> Result<Response, AttemptError> {
    let channel = (cand.channel_id, cand.channel_key_id);
    match dispatch_chat(bill, cand, base, body, false).await {
        Ok(ChatResponse::Json {
            status,
            upstream_request_id,
            body,
            usage,
        }) => {
            let usage = usage.map_or_else(
                || TokenUsage {
                    prompt_tokens: bill.est_prompt,
                    cached_tokens: 0,
                    cache_write_tokens: 0,
                    audio_prompt_tokens: 0,
                    image_prompt_tokens: 0,
                    completion_tokens: estimate_completion_tokens(non_stream_content_chars(
                        bill.ingress,
                        &body,
                    )),
                    audio_completion_tokens: 0,
                    reasoning_tokens: 0,
                },
                UsageProbe::to_token_usage,
            );
            let mut info = cand_info(cand, sticky_layer, retry);
            info.upstream_request_id = upstream_request_id;
            // 响应元数据（model 渠道 opt-in / service_tier 模型配档位倍率）
            let resp_meta = if info.bill_resp_model || bill.has_tier_pricing {
                let mut m = extract_body_meta(&body);
                if !info.bill_resp_model {
                    m.model = None; // 未开开关不按响应模型计费
                }
                m
            } else {
                RespMeta::default()
            };
            // 结算移出响应路径（与流式同语义：响应先行、结算后台；
            // Redis commit 幂等 + 悬置由 sweep 兜底，压测驱动优化见 docs/perf-report.md）
            let bill_bg = bill.clone();
            // detach 说明：结算生命周期独立于响应；所有失败路径内部自兜底
            tokio::spawn(async move {
                settle_commit(&bill_bg, &info, usage, None, failover, resp_meta).await;
                bill_bg.state.sched.release_slot(info.key, info.cap).await;
            });
            let mut resp = Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            resp = with_request_id(resp, bill.request_id);
            Ok(resp)
        }
        Ok(ChatResponse::Stream(_)) => Err(AttemptError::Retriable {
            code: codes::UPSTREAM_ERROR,
            upstream_status: None,
            failure_kind: KeyFailure::Transient,
        }),
        Err(err) => Err(classify_fatal(err, failover, channel)),
    }
}

fn non_stream_content_chars(ingress: Ingress, body: &Bytes) -> usize {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return 0;
    };
    match ingress {
        Ingress::OpenAi => v
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map_or(0, |s| s.chars().count()),
        Ingress::Anthropic => v
            .get("content")
            .and_then(|c| c.as_array())
            .map_or(0, |blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        b.get("text")
                            .or_else(|| b.get("thinking"))
                            .and_then(|t| t.as_str())
                    })
                    .map(|s| s.chars().count())
                    .sum()
            }),
        Ingress::Responses => v
            .get("output")
            .and_then(|o| o.as_array())
            .map_or(0, |items| {
                items
                    .iter()
                    .filter_map(|i| i.get("content").and_then(|c| c.as_array()))
                    .flatten()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .map(|s| s.chars().count())
                    .sum()
            }),
    }
}

// ---- 结算 ----

/// 按响应模型重选计费上下文（渠道 opt-in，Sub2API 0.1.175 对齐）：
/// 响应模型 ≠ 请求模型且价簿有其**精确名**定价 → 返回重建的上下文；
/// 无价/同名 → None（维持请求 canonical，fail-open 绝不因改名拒付）。
/// 别名解析不参与（结算路径不回 PG）。
fn resolve_billing_calc(
    bill: &RequestBilling,
    resp_model: Option<&str>,
    usage: TokenUsage,
) -> Option<CalcContext> {
    let rm = resp_model.filter(|m| *m != bill.model && !m.is_empty())?;
    let mut candidate = bill.calc.clone();
    candidate.model = ModelCode::from(rm);
    if calculate(&bill.book, &candidate, usage).is_ok() {
        tracing::debug!(request_id = %bill.request_id, requested = %bill.model, billed = rm, "按上游响应模型计费");
        Some(candidate)
    } else {
        None
    }
}

/// 结算档位选择（只降不升）：请求声明档与响应报告档中**有效倍率较低者**；
/// 未配置的档位名与 None 均按 1.0。两者皆无 → None。
fn pick_settle_tier(
    book: &PriceBook,
    model: &ModelCode,
    requested: Option<&str>,
    reported: Option<&str>,
) -> Option<String> {
    let ratio_of = |t: Option<&str>| -> i64 {
        t.and_then(|t| book.tier_ratio(model, t))
            .map_or(RatioFp::ONE.as_scaled(), okapi_pricing::RatioFp::as_scaled)
    };
    let pick = if ratio_of(reported) <= ratio_of(requested) {
        reported
    } else {
        requested
    };
    pick.map(str::to_owned)
}

// 结算收敛点：响应模型/档位重选 + commit + 记账的线性时序
#[allow(clippy::too_many_lines)]
async fn settle_commit(
    bill: &RequestBilling,
    info: &CandInfo,
    usage: TokenUsage,
    ttft_ms: Option<i32>,
    failover: i16,
    resp_meta: RespMeta,
) {
    let calc_override = resolve_billing_calc(bill, resp_meta.model.as_deref(), usage);
    let billed_model: String = if calc_override.is_some() {
        resp_meta
            .model
            .clone()
            .unwrap_or_else(|| bill.model.clone())
    } else {
        bill.model.clone()
    };
    // service_tier 结算档：只降不升（DESIGN §3-4.5；按最终计费模型查档位倍率）
    let mut calc = calc_override.unwrap_or_else(|| bill.calc.clone());
    calc.service_tier = pick_settle_tier(
        &bill.book,
        &calc.model,
        bill.service_tier.as_deref(),
        resp_meta.service_tier.as_deref(),
    );
    let calc = &calc;
    let billed_model: &str = &billed_model;
    let quote: Quote = match calculate(&bill.book, calc, usage) {
        Ok(q) => q,
        Err(err) => {
            // 结算算价失败：退款 + 失败记账（fail-closed，不猜测金额）
            tracing::error!(request_id = %bill.request_id, error = %err, "结算算价失败，退款");
            let _ = bill
                .state
                .ledger
                .refund(bill.user_id, bill.key_id, bill.request_id)
                .await;
            record_terminal(
                bill,
                info,
                usage,
                Money::ZERO,
                None,
                BillingState::Failed,
                5,
                Some("pricing_settle_failed"),
                ttft_ms,
                failover,
                "refund",
                0,
            )
            .await;
            return;
        }
    };

    match bill
        .state
        .ledger
        .commit(bill.user_id, bill.key_id, bill.request_id, quote.amount)
        .await
    {
        Ok(CommitOutcome::Committed { balance_after, .. }) => {
            let snapshot = serde_json::to_value(&quote.snapshot).ok();
            let input = SettlementInput {
                request_id: bill.request_id,
                log_type: 2,
                user_id: bill.user_id,
                api_key_id: bill.key_id,
                group_code: &bill.group,
                model_name: billed_model,
                channel_id: Some(info.channel),
                channel_key_id: Some(info.key),
                state: BillingState::Committed,
                usage,
                amount: quote.amount,
                original: quote.original,
                discount: quote.discount,
                pricing_epoch: Some(bill.book.epoch()),
                pricing_snapshot: snapshot,
                latency_ms: elapsed_ms_i32(bill.started),
                ttft_ms,
                is_stream: bill.is_stream,
                retry_count: info.retry,
                failover_count: failover,
                upstream_status: Some(200),
                error_code: None,
                upstream_request_id: info.upstream_request_id.as_deref(),
                node: bill.state.node.as_ref(),
                sticky_layer: info.sticky_layer,
                client_type: bill.client_type,
                client_ip: bill.client_ip.as_deref(),
                delta_micro: quote.amount.as_micros().saturating_neg(),
                balance_after: Some(balance_after),
                event_type: "commit",
            };
            bill.state.settle_write(input).await;
            super::auth::record_settlement_counters(
                &bill.state,
                bill.user_id,
                bill.member_user_id,
                quote.amount.as_micros(),
                usage.total_raw(),
            )
            .await;
        }
        Ok(CommitOutcome::NoReservation) => {
            tracing::warn!(request_id = %bill.request_id, "重复结算竞争：预扣不存在，跳过");
        }
        Err(err) => {
            tracing::error!(request_id = %bill.request_id, error = %err, "Redis 结算失败（预扣悬置，待对账清理）");
        }
    }
}

async fn settle_failure(bill: &RequestBilling, failure: &ForwardFailure) {
    if let Err(err) = bill
        .state
        .ledger
        .refund(bill.user_id, bill.key_id, bill.request_id)
        .await
    {
        tracing::error!(request_id = %bill.request_id, error = %err, "退款失败（预扣悬置，待对账清理）");
    }
    let (channel, key) = failure.channel.map_or((0, 0), |(c, k)| (c, k));
    record_terminal(
        bill,
        &CandInfo {
            channel,
            key,
            cap: None,
            sticky_layer: 0,
            retry: 0,
            upstream_request_id: None,
            bill_resp_model: false,
        },
        TokenUsage::default(),
        Money::ZERO,
        failure.upstream_status,
        BillingState::Failed,
        5,
        Some(failure.error_code.as_str()),
        None,
        failure.failover_count,
        "refund",
        0,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn record_terminal(
    bill: &RequestBilling,
    info: &CandInfo,
    usage: TokenUsage,
    amount: Money,
    upstream_status: Option<i16>,
    state: BillingState,
    log_type: i16,
    error_code: Option<&str>,
    ttft_ms: Option<i32>,
    failover: i16,
    event_type: &str,
    delta_micro: i64,
) {
    let input = SettlementInput {
        request_id: bill.request_id,
        log_type,
        user_id: bill.user_id,
        api_key_id: bill.key_id,
        group_code: &bill.group,
        model_name: &bill.model,
        channel_id: (info.channel != 0).then_some(info.channel),
        channel_key_id: (info.key != 0).then_some(info.key),
        state,
        usage,
        amount,
        original: Money::ZERO,
        discount: Money::ZERO,
        pricing_epoch: Some(bill.book.epoch()),
        pricing_snapshot: None,
        latency_ms: elapsed_ms_i32(bill.started),
        ttft_ms,
        is_stream: bill.is_stream,
        retry_count: info.retry,
        failover_count: failover,
        upstream_status,
        error_code,
        upstream_request_id: info.upstream_request_id.as_deref(),
        node: bill.state.node.as_ref(),
        sticky_layer: info.sticky_layer,
        client_type: bill.client_type,
        client_ip: bill.client_ip.as_deref(),
        delta_micro,
        balance_after: None,
        event_type,
    };
    bill.state.settle_write(input).await;
}

// ---- 响应工具 ----

fn with_request_id(mut resp: Response, request_id: Uuid) -> Response {
    if let Ok(value) = axum::http::HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut().insert("x-okapi-request-id", value);
    }
    resp
}

fn upstream_passthrough_response(
    ingress: Ingress,
    status: u16,
    body: Bytes,
    request_id: Uuid,
) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    // Anthropic 入口：上游若非 anthropic 错误壳（如 OpenAI 渠道 400），转译为协议壳
    let body = if ingress == Ingress::Anthropic && !body_is_anthropic_error(&body) {
        let message = String::from_utf8_lossy(&body);
        Bytes::from(
            serde_json::json!({
                "type": "error",
                "error": {"type": "upstream_error", "message": message},
            })
            .to_string(),
        )
    } else {
        body
    };
    let resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    with_request_id(resp, request_id)
}

fn body_is_anthropic_error(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .is_some_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("error"))
}

fn elapsed_ms_i32(started: Instant) -> i32 {
    i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}
