//! /v1/chat/completions 与 /v1/messages：鉴权 → 估价预扣 → 渠道 failover 转发
//! （首字前缓冲）→ 结算。时序对齐 IMPLEMENTATION §2.2；SSE 语义对齐 §3.7。
//! 入口协议 × 渠道协议 四象限（§4.4）：转换在 providers::convert，泵送与结算无感。

use super::clients::detect_client_type;
use super::error::AppError;
use super::error::with_request_id;
use super::estimate::{self, estimate_prompt_tokens};
use super::execution_plan::{ExecutionPlan, Requirements};
use super::sched_redis::response_affinity::{
    self, ResponseBinding, ResponseParent, ResponseWriter,
};
use super::sched_redis::session_hash;
use super::scheduler::{Strategy, order_candidates};
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
use okapi_ledger::{LimitCaps, Pool, ReserveOutcome, SettlementInput};
use okapi_pricing::{CalcContext, PriceBook, Quote, RatioFp};
use okapi_providers::convert::{
    anthropic_to_openai as conv_a2o, gemini_to_openai as conv_g2o, openai_to_anthropic as convert,
    openai_to_gemini as conv_gem, responses_to_chat as conv_resp,
};
use okapi_providers::reasoning::{self, ReasoningDirective};
use okapi_providers::{
    ChatEvent, ChatResponse, StreamHandle, UpstreamError, ensure_stream_usage, rewrite_model,
};
use okapi_store::ChannelCandidate;
use okapi_store::channels::KeyFailure;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub mod websocket;

const DEFAULT_ANTHROPIC_BASE: &str = "https://api.anthropic.com/v1";
/// 首字窗口（连接 + 首个产出事件）；窗口内失败可无痕 failover。
/// 缺省值——渠道可用 `retry_policy.first_output_timeout_secs` 覆盖（5..=300）。
const FIRST_OUTPUT_TIMEOUT: Duration = Duration::from_secs(30);
/// 单请求最多尝试的渠道 key 数。
const MAX_ATTEMPTS: usize = 3;
/// 预扣补全上限的兜底值与硬顶。
const DEFAULT_COMPLETION_CAP: u32 = 2048;
const MAX_COMPLETION_CAP: u32 = 32_768;

use super::ingress::Ingress;

/// 入口探针归一化结果（两种协议解析为同一形状，主链路协议无关）。
struct ProbeInfo {
    authenticated: Option<Arc<okapi_store::AuthedKey>>,
    /// 客户端请求的模型名（别名解析前；透传重写比对用）。
    requested_model: String,
    stream: bool,
    /// 显式请求的补全上限（openai: max_completion_tokens>max_tokens；anthropic: max_tokens）。
    completion_cap_req: Option<u32>,
    /// 一次生成的候选条数（OpenAI `n` / Gemini `candidateCount`，其余入口恒 1）。
    choices: u32,
    /// prompt 精确分词结果（tiktoken；预扣与密度的共同输入）。
    prompt_tokens: u32,
    prompt_chars: usize,
    /// L2 会话标识（头优先，缺省消息前缀哈希）。
    session: Option<String>,
    /// 请求特征（能力感知路由输入，§3.8）。
    needs_tools: bool,
    needs_vision: bool,
    /// OpenAI service_tier 请求声明（tier 计费轴；chat 与 responses 入口有，anthropic 无此概念）。
    service_tier: Option<String>,
}

/// 每请求计费上下文（转发与异步结算共享）。
#[derive(Clone)]
struct RequestBilling {
    trace: super::diagnostics::Trace,
    state: AppState,
    ingress: Ingress,
    book: Arc<PriceBook>,
    calc: CalcContext,
    user_id: i64,
    key_id: i64,
    /// 团 key 归属成员（结算后累计月度消费）。
    member_user_id: Option<i64>,
    request_id: Uuid,
    /// Keep the admitted funding pool even if the Redis refund is deferred.
    reservation_pool: Pool,
    source_window: Option<String>,
    est_prompt: u32,
    /// 本次请求实测的 token/千字符 密度（补全侧只有字符数，用它折算）。
    density: u32,
    /// 预扣补全上限（单条；anthropic 转换的 max_tokens 兜底也用它）。
    completion_cap: u32,
    /// 候选条数：预扣与结算复核按 `completion_cap × choices` 估补全。
    choices: u32,
    /// 请求没声明输出上限、模型 max_output 又超过预扣封顶时，转发前写进请求的上限。
    default_output_cap: Option<u32>,
    /// 归一后的 reasoning 指令（模型名后缀 ∪ 请求体参数，参数优先；§11.26）。
    /// 注入上游时按渠道方言三向展开。
    directive: Option<ReasoningDirective>,
    /// 请求级路由偏好（§11.24；缺省即此前行为）。
    prefs: super::routing_prefs::RoutingPrefs,
    /// canonical 模型名（别名解析后；记账与调度用）。
    model: String,
    requested_model: String,
    group: String,
    is_stream: bool,
    started: Instant,
    /// 历史响应的账号硬绑定，不允许渠道或模型降级。
    response_parent: Option<ResponseParent>,
    /// 会话标识（L2 粘性键）。
    session: Option<String>,
    /// UA 识别的客户端类型。
    client_type: &'static str,
    /// 客户端 IP（CDN 头按序，§14.2）。
    client_ip: Option<String>,
    /// 客户端身份头（只在订阅 provider 的出向上透传，§11.38）。
    client_headers: Arc<Vec<(String, String)>>,
    /// 渠道可见性组（用户全部组并集，§6.3）。
    /// 有序池链（主池 → 降级池）：候选查询与缓存键都吃它。
    pool_chain: Vec<String>,
    pool_strategy: Option<String>,
    /// 请求声明的 service_tier（预扣按此档估；结算只降不升，DESIGN §3-4.5）。
    service_tier: Option<String>,
    /// 模型是否配置了档位倍率（据此决定是否采集响应档位）。
    has_tier_pricing: bool,
    /// 模型级降级链（DESIGN §3.4.1；已过 key 白名单，仅零候选时消费）。
    fallback_models: Arc<Vec<AdmittedFallback>>,
    server_tools: super::server_tools::ToolAdmission,
    reserved_amount: Money,
    /// Every quote that could determine this hold, captured before reserve.
    reservation_snapshot: Arc<serde_json::Value>,
    /// 发生模型级降级时 = 客户端请求的 canonical 模型（写入 pricing_snapshot）。
    downgraded_from: Option<String>,
    /// 预扣由谁收口：HTTP handler 持有，直到后台结算 / 流泵接手（克隆间共享）。
    settlement: SettlementHandoff,
}

/// 预扣的收口责任。handler 在等上游时可能被丢弃（客户端断开），此时必须有人退款；
/// 一旦后台结算任务或流泵接手，由它按实际产出结算，取消兜底不得再退。
#[derive(Clone, Default)]
struct SettlementHandoff(Arc<std::sync::atomic::AtomicBool>);

impl SettlementHandoff {
    /// 在 spawn 接手任务前调用：两者之间不能有 await，否则取消会落在空档里。
    fn hand_off(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    fn handed_off(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// 客户端在响应前断开 → handler future 被 hyper 丢弃，`settle_failure` 不会执行。
/// 预扣与 key 并发槽否则要等过期清理（约 10 分钟）才放，也不留失败日志；
/// 这里在 Drop 时补做同一套退款 + 失败记账（与媒体端点的 `failure::Guard` 同义）。
struct CancelRefund(Option<RequestBilling>);

impl CancelRefund {
    fn arm(bill: &RequestBilling) -> Self {
        Self(Some(bill.clone()))
    }

    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelRefund {
    fn drop(&mut self) {
        let Some(bill) = self.0.take() else {
            return;
        };
        if bill.settlement.handed_off() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let settlements = bill.state.settlements.clone();
        // 退款幂等、失败日志按 request_id 去重：与被取消到一半的 settle_failure 重入也安全
        settlements.spawn(async move {
            let status = StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST);
            let failure =
                ForwardFailure::app(AppError::new(status, codes::CLIENT_CLOSED_REQUEST), 0, None);
            settle_failure(&bill, &failure).await;
        });
    }
}

#[derive(Clone)]
struct AdmittedFallback {
    model: String,
    calc: CalcContext,
    completion_cap: u32,
    default_output_cap: Option<u32>,
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
    upstream: Option<Box<(String, String)>>,
}

impl ForwardFailure {
    fn app(err: AppError, failover: i16, channel: Option<(i64, i64)>) -> Self {
        Self {
            error_code: err.code.clone(),
            upstream_status: None,
            failover_count: failover,
            channel,
            reply: FailureReply::App(err),
            upstream: None,
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

/// 报价单价是否越过请求声明的上限；越过则返回 "轴:实际单价" 供 param 回显。
///
/// 快照里的 `final_unit_price_input_per_1m_usd` 是**输入**侧最终单价；输出侧单价 =
/// 它 × completion_ratio（补全倍率就是"输出比输入贵多少倍"的定义）。
fn price_above_max(
    quote: &okapi_pricing::Quote,
    prefs: &super::routing_prefs::RoutingPrefs,
) -> Option<String> {
    if prefs.max_price.prompt.is_none() && prefs.max_price.completion.is_none() {
        return None;
    }
    let snap = serde_json::to_value(&quote.snapshot).ok()?;
    let input = snap
        .get("final_unit_price_input_per_1m_usd")
        .and_then(serde_json::Value::as_f64)?;
    if let Some(cap) = prefs.max_price.prompt
        && input > cap
    {
        return Some(format!("prompt:{input}"));
    }
    if let Some(cap) = prefs.max_price.completion {
        let ratio = snap
            .get("completion_ratio")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);
        let output = input * ratio;
        if output > cap {
            return Some(format!("completion:{output}"));
        }
    }
    None
}

/// 预扣用的单条补全上界。显式上限照单全收（不超过模型 max_output）：上游按它生成、
/// 按实际计费，把它截到 `MAX_COMPLETION_CAP` 只会让预扣不再是扣费上界、余额可被透支。
/// 未声明时才取模型缺省并封顶，免得为没要求长输出的请求冻结大额余额。
fn admitted_completion_cap(requested: Option<u32>, max_output: Option<u32>) -> u32 {
    match requested {
        Some(requested) => max_output.map_or(requested, |max| requested.min(max)),
        None => max_output
            .unwrap_or(DEFAULT_COMPLETION_CAP)
            .min(MAX_COMPLETION_CAP),
    }
}

/// 未声明输出上限时，预扣只按封顶后的上限估；模型能输出得更多，就得把这个上限
/// 写进请求，否则上游照 max_output 生成、结算多退少补，余额被透支（2026-10-05 定案：
/// 补上限而不是全额预扣，宁可极少数超长输出截断，也不为普通请求冻结大额余额）。
/// max_output 不超过封顶的模型本身生成不到更多，不改请求。
fn default_output_cap(requested: Option<u32>, max_output: Option<u32>) -> Option<u32> {
    (requested.is_none() && max_output.is_some_and(|max| max > MAX_COMPLETION_CAP))
        .then_some(MAX_COMPLETION_CAP)
}

/// 按入口方言写入缺省输出上限（转换路径各自翻译成上游字段）。已有上限的不动；
/// compact 端点不接受输出上限参数，跳过。
fn bound_default_output(ingress: Ingress, cap: Option<u32>, body: Bytes) -> Bytes {
    let Some(cap) = cap else {
        return body;
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(obj) = value.as_object_mut() else {
        return body;
    };
    let inserted = match ingress {
        Ingress::OpenAi => {
            // max_tokens 兼容面最广；推理模型由 model_parameters 改写成 max_completion_tokens
            if obj.contains_key("max_tokens") || obj.contains_key("max_completion_tokens") {
                return body;
            }
            obj.insert("max_tokens".into(), cap.into());
            true
        }
        Ingress::Anthropic => obj.insert("max_tokens".into(), cap.into()).is_none(),
        Ingress::Responses => obj.insert("max_output_tokens".into(), cap.into()).is_none(),
        Ingress::Gemini => {
            let Some(config) = obj
                .entry("generationConfig")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
            else {
                return body;
            };
            config
                .insert("maxOutputTokens".into(), cap.into())
                .is_none()
        }
        Ingress::ResponsesCompact => false,
    };
    if !inserted {
        return body;
    }
    serde_json::to_vec(&value).map_or(body, Bytes::from)
}

/// 上游错误 → key 状态机类别（§3.6 重试矩阵）。
/// 注意顺序：insufficient_quota 判定先于 429——OpenAI 风格的
/// `429 + insufficient_quota` 语义是配额耗尽（冷却到次日），不是限速（60s）。
pub(super) fn failure_kind_of(err: &UpstreamError) -> KeyFailure {
    match err {
        UpstreamError::Status { status, body, .. }
            if *status == 402 || (*status == 429 && body_says_insufficient_quota(body)) =>
        {
            KeyFailure::QuotaExhausted
        }
        UpstreamError::Status { status: 529, .. } => KeyFailure::RateLimited {
            retry_after_secs: Some(err.retry_after_secs().unwrap_or(600)),
        },
        UpstreamError::Status { status: 429, .. } => KeyFailure::RateLimited {
            retry_after_secs: err.retry_after_secs(),
        },
        UpstreamError::Status { status: 401, .. } => KeyFailure::Invalid,
        // 403 只在 body 明说凭证问题时才算凭证失效，否则按资源级失败回退，不改变 key 健康（详见
        // `body_says_credential_rejected`）
        UpstreamError::Status {
            status: 403, body, ..
        } if body_says_credential_rejected(body) => KeyFailure::Invalid,
        UpstreamError::Status {
            status: 500..=599, ..
        } => KeyFailure::Transient,
        // 连接阶段失败：凭证无从判断；走了代理时归给代理的被动熔断（§11.41）
        UpstreamError::Unreachable { proxy_hop, .. } => KeyFailure::Unreachable {
            proxy_hop: *proxy_hop,
        },
        UpstreamError::Status { .. }
        | UpstreamError::Connect(_)
        | UpstreamError::Timeout
        | UpstreamError::Stream(_)
        | UpstreamError::Session { .. }
        | UpstreamError::Build(_) => KeyFailure::Request,
    }
}

/// 上游的 403 是不是真的在说「这把凭证不认」。
///
/// 401 = 没通过认证 → 凭证必然有问题；403 = 认证过了但不被允许，本质是**按资源**的判定。
/// 聚合型上游普遍拿 403 表达「这个模型你的套餐没开通」（实测到的原文：
/// `access_denied / Deposit required to unlock premium models`），而 key 本身完全有效。
/// 此前 401 与 403 一并判 `Invalid`（`status=6`，无冷却、不自愈，控制面也没有复活入口），
/// 于是调一次未开通的模型就把该渠道**所有**模型打死，只能靠重置凭证救回来。
/// 故 403 改为：body 明说凭证问题才算失效，否则不改变 key 健康状态。
fn body_says_credential_rejected(body: &Bytes) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
        "invalid_api_key",
        "invalid api key",
        "incorrect api key",
        "invalid_authentication",
        "authentication_error",
        "invalid token",
        "api key not valid",
        "unauthorized",
    ]
    .iter()
    .any(|needle| text.contains(needle))
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
    let authenticated = match super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_response_with(Some(request_id)),
    };
    let Ok(probe) = serde_json::from_slice::<ChatRequestProbe>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    let (needs_tools, needs_vision) = request_features(Ingress::OpenAi, &body);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.max_completion_tokens.or(probe.max_tokens),
        choices: probe.choices(),
        prompt_tokens: estimate_prompt_tokens(
            &probe.model,
            &probe.prompt_segments(),
            probe.messages.len(),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&headers, &probe.messages),
        service_tier: probe.service_tier.clone(),
        needs_tools,
        needs_vision,
    };
    match Box::pin(handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::OpenAi,
        &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// OpenAI /v1/responses 入口（§4.4：渠道说 Responses 方言则直转，否则降级 ChatCompletions #5209）。
pub async fn responses(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    Box::pin(responses_entry(state, headers, body, Ingress::Responses)).await
}

pub async fn responses_compact(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    Box::pin(responses_entry(
        state,
        headers,
        body,
        Ingress::ResponsesCompact,
    ))
    .await
}

fn compact_request_body(body: &Bytes) -> Result<Bytes, serde_json::Error> {
    let mut fields: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(body)?;
    fields.remove("stream");
    serde_json::to_vec(&fields).map(Bytes::from)
}

async fn responses_entry(
    state: AppState,
    headers: HeaderMap,
    body: Bytes,
    ingress: Ingress,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let authenticated = match super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_response_with(Some(request_id)),
    };
    let Ok(probe) = serde_json::from_slice::<ResponsesRequestProbe>(&body) else {
        return AppError::bad_request().into_response_with(Some(request_id));
    };
    // 后台模式先回「排队中」、不带用量，生成在本次结算之后继续——网关只能按估算收一点，
    // 推理费用全由站方承担，且没有取回结果的接口。WS 入口同样拒绝。
    if probe.background == Some(true) {
        return AppError::bad_request()
            .with_param("background")
            .into_response_with(Some(request_id));
    }
    let body = if ingress == Ingress::ResponsesCompact {
        if probe.stream {
            return AppError::bad_request()
                .with_param("stream")
                .into_response_with(Some(request_id));
        }
        // 通用字段剥除器会保护 stream；compact 协议须单独移除该字段。
        let Ok(body) = compact_request_body(&body) else {
            return AppError::bad_request().into_response_with(Some(request_id));
        };
        body
    } else {
        body
    };
    let input_messages = probe.input_messages();
    let (needs_tools, needs_vision) = request_features(ingress, &body);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.completion_cap_req(),
        choices: 1,
        prompt_tokens: estimate_prompt_tokens(
            &probe.model,
            &probe.prompt_segments(),
            input_messages.len().max(1),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&headers, &input_messages),
        service_tier: probe.service_tier.clone(),
        needs_tools,
        needs_vision,
    };
    match Box::pin(handle_chat(
        &state, &headers, &body, request_id, started, ingress, &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_response_with(Some(request_id)),
    }
}

/// Gemini 入口查询串：只认 `key`（SDK 鉴权）；`alt=sse` 等其余参数忽略。
#[derive(serde::Deserialize)]
pub struct GeminiQuery {
    #[serde(default)]
    pub key: Option<String>,
}

/// Gemini 原生入口 `POST /v1beta/models/{model}:generateContent|streamGenerateContent`。
/// 模型名与流式与否都在路径上（`gemini-2.5-pro:streamGenerateContent`）；鉴权除 Bearer 外
/// 认 Gemini SDK 的 `x-goog-api-key` 头与 `?key=` 查询串。流式一律以 SSE 回（官方 `alt=sse`
/// 形态；不带 alt 的 JSON 数组流式形态不提供）。
pub async fn gemini_generate(
    State(state): State<AppState>,
    axum::extract::Path(model_action): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<GeminiQuery>,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let Some((model, action)) = model_action.rsplit_once(':') else {
        return AppError::new(StatusCode::NOT_FOUND, codes::MODEL_NOT_FOUND)
            .into_gemini_response_with(Some(request_id));
    };
    let stream = match action {
        "generateContent" => false,
        "streamGenerateContent" => true,
        _ => {
            return AppError::bad_request()
                .with_param(action)
                .into_gemini_response_with(Some(request_id));
        }
    };
    // `?key=` 查询串鉴权：搬进头里让 authenticate 统一处理（不改鉴权主链）
    if !headers.contains_key(header::AUTHORIZATION)
        && !headers.contains_key("x-goog-api-key")
        && let Some(key) = query.key.as_deref()
        && let Ok(value) = axum::http::HeaderValue::from_str(key)
    {
        headers.insert("x-goog-api-key", value);
    }
    let authenticated = match super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_gemini_response_with(Some(request_id)),
    };
    let Ok(probe) = serde_json::from_slice::<okapi_api::GeminiRequestProbe>(&body) else {
        return AppError::bad_request().into_gemini_response_with(Some(request_id));
    };
    let input_messages = probe.input_messages();
    let (needs_tools, needs_vision) = request_features(Ingress::Gemini, &body);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: model.to_owned(),
        stream,
        completion_cap_req: probe.completion_cap_req(),
        choices: probe.choices(),
        prompt_tokens: estimate_prompt_tokens(
            model,
            &probe.prompt_segments(),
            input_messages.len().max(1),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&headers, &input_messages),
        service_tier: None,
        needs_tools,
        needs_vision,
    };
    match Box::pin(handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::Gemini,
        &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_gemini_response_with(Some(request_id)),
    }
}

/// Anthropic /v1/messages 入口（§4.4：入口协议 + 上游方向双向）。
pub async fn messages(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let request_id = Uuid::new_v4();
    let started = Instant::now();
    let authenticated = match super::auth::authenticate_data_plane(&state, &headers).await {
        Ok(key) => key,
        Err(error) => return error.into_anthropic_response_with(Some(request_id)),
    };
    let Ok(probe) = serde_json::from_slice::<MessagesRequestProbe>(&body) else {
        return AppError::bad_request().into_anthropic_response_with(Some(request_id));
    };
    let (needs_tools, needs_vision) = request_features(Ingress::Anthropic, &body);
    let info = ProbeInfo {
        authenticated: Some(authenticated),
        requested_model: probe.model.clone(),
        stream: probe.stream,
        completion_cap_req: probe.max_tokens,
        choices: 1,
        prompt_tokens: estimate_prompt_tokens(
            &probe.model,
            &probe.prompt_segments(),
            probe.messages.len(),
        ),
        prompt_chars: probe.prompt_chars(),
        session: session_hash(&headers, &probe.messages),
        service_tier: None,
        needs_tools,
        needs_vision,
    };
    match Box::pin(handle_chat(
        &state,
        &headers,
        &body,
        request_id,
        started,
        Ingress::Anthropic,
        &info,
    ))
    .await
    {
        Ok(resp) => resp,
        Err(err) => err.into_anthropic_response_with(Some(request_id)),
    }
}

/// Anthropic `POST /v1/messages/count_tokens`：鉴权 + 模型可见，**不计费**。
/// 有 anthropic 候选则代理上游 tokenizer；否则本地估算（Claude Code 缺此端点会退本地）。
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

async fn count_tokens_inner(
    state: &AppState,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = super::auth::authenticate_data_plane(state, headers).await?;
    let _permit = super::sched_redis::token_count::CountPermit::acquire(&state.sched, &key).await?;
    // 不计费，但有 anthropic 候选时会拿渠道凭证打上游 tokenizer：按分组窗限速（§11.32）。
    // 不过 check_member_limit——那是月度消费上限，不该挡住不花钱的调用
    super::auth::check_group_rate(state, &key).await?;
    let probe: MessagesRequestProbe =
        serde_json::from_slice(&body).map_err(|_| AppError::bad_request())?;
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
    let cand = order_candidates(rows)
        .into_iter()
        .find(|c| matches!(c.provider.as_str(), "anthropic" | "anthropic_max"));
    if let Some(cand) = cand {
        if let Some(limit) = cand.rpm_limit
            && !state
                .sched
                .channel_key_rate_ok(cand.channel_key_id, i64::from(limit))
                .await
        {
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                codes::RATE_LIMITED,
            ));
        }
        let _slot = super::sched_redis::token_count::ChannelPermit::acquire(&state.sched, &cand)
            .await
            .map_err(|error| super::account_control::attempt_error(&error))?
            .ok_or_else(|| AppError::new(StatusCode::TOO_MANY_REQUESTS, codes::RATE_LIMITED))?;
        let upstream_model = cand.upstream_model(&canonical).to_owned();
        let body_up = rewrite_model(&body, &probe.model, &upstream_model)
            .map_err(|_| AppError::bad_request())?;
        let base = cand
            .api_base
            .clone()
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE.to_owned());
        let outbound = super::oauth_cred::outbound_with_client(
            &cand,
            &super::oauth_cred::client_headers(headers),
        );
        let counted = if cand.provider == "anthropic_max" {
            match super::oauth_cred::fresh_credential(state, &cand).await {
                Ok(cred) => {
                    okapi_providers::oauth::anthropic_max::count_tokens(
                        state.anthropic.http(),
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
                .anthropic
                .count_tokens(&base, &cand.credential, body_up, &outbound)
                .await
        };
        match counted {
            Ok(up) => {
                return Ok(axum::Json(
                    serde_json::from_slice::<serde_json::Value>(&up)
                        .unwrap_or_else(|_| serde_json::json!({"input_tokens": 0})),
                )
                .into_response());
            }
            Err(_) => {
                return Err(AppError::new(
                    StatusCode::BAD_GATEWAY,
                    codes::UPSTREAM_ERROR,
                ));
            }
        }
    }
    let tokens =
        estimate_prompt_tokens(&probe.model, &probe.prompt_segments(), probe.messages.len());
    Ok(axum::Json(serde_json::json!({ "input_tokens": tokens })).into_response())
}

/// 请求特征探测（§3.8 能力感知路由）：tools 数组非空 / 消息含图像部件。
fn request_features(ingress: Ingress, body: &Bytes) -> (bool, bool) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (false, false);
    };
    if ingress == Ingress::Gemini {
        return gemini_request_features(&v);
    }
    let needs_tools = v
        .get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty());
    let image_types: &[&str] = match ingress {
        Ingress::OpenAi => &["image_url"],
        Ingress::Anthropic => &["image"],
        Ingress::Responses | Ingress::ResponsesCompact => &["input_image"],
        Ingress::Gemini => &[],
    };
    let containers = match ingress {
        Ingress::Responses | Ingress::ResponsesCompact => v.get("input"),
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

/// 预扣按张估图片输入：各家按尺寸计 token，本地不解码图片，取主流单张上界
/// （Claude 封顶约 1600、GPT-4o high 约 1100、按 patch 计的 GPT-4.1/5 系约 2500）。
/// 只用于预扣与缺用量时的兜底，实际仍按上游用量结算。
const IMAGE_INPUT_TOKENS: u32 = 2_560;

/// 请求里的图片输入张数，含工具结果里嵌套的图片。
fn image_inputs(ingress: Ingress, body: &Bytes) -> u32 {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return 0;
    };
    input_roots(ingress, &v)
        .into_iter()
        .flatten()
        .map(count_images)
        .fold(0, u32::saturating_add)
}

/// 携带输入内容的顶层字段（消息 / input / contents 与系统指令）。
fn input_roots(ingress: Ingress, v: &serde_json::Value) -> [Option<&serde_json::Value>; 2] {
    match ingress {
        Ingress::OpenAi => [v.get("messages"), None],
        Ingress::Anthropic => [v.get("messages"), v.get("system")],
        Ingress::Responses | Ingress::ResponsesCompact => [v.get("input"), None],
        Ingress::Gemini => [
            v.get("contents"),
            v.get("systemInstruction")
                .or_else(|| v.get("system_instruction")),
        ],
    }
}

/// PDF 每页上界：Anthropic 文档按页计文本 + 页面图像，约 1500–3000 token。
const PDF_PAGE_TOKENS: u32 = 3_000;
/// 音频每 token 至少对应的字节数：码率不低于 32 kbps（4000 B/s），各家最高约 32 token/s（Gemini）。
const AUDIO_BYTES_PER_TOKEN: usize = 125;

/// 内联文件与音频输入的 token 上界（只看请求里带着的数据；URL / file_id 本地取不到，不计）。
/// 只用于预扣与缺用量时的兜底，实际仍按上游用量结算。
fn attachment_tokens(ingress: Ingress, body: &Bytes) -> u32 {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return 0;
    };
    input_roots(ingress, &v)
        .into_iter()
        .flatten()
        .map(count_attachments)
        .fold(0, u32::saturating_add)
}

fn count_attachments(value: &serde_json::Value) -> u32 {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(count_attachments)
            .fold(0, u32::saturating_add),
        serde_json::Value::Object(obj) => {
            let own = match obj.get("type").and_then(serde_json::Value::as_str) {
                // Anthropic document：base64 PDF 或纯文本
                Some("document") => match str_at(value, "/source/type") {
                    Some("base64") => str_at(value, "/source/data").map_or(0, |data| {
                        media_tokens(str_at(value, "/source/media_type"), data)
                    }),
                    Some("text") => str_at(value, "/source/data")
                        .map_or(0, |text| saturating_u32(text.len() / 2)),
                    _ => 0,
                },
                // OpenAI chat file / Responses input_file：data URL
                Some("file") => str_at(value, "/file/file_data").map_or(0, data_url_tokens),
                Some("input_file") => str_at(value, "/file_data").map_or(0, data_url_tokens),
                Some("input_audio") => str_at(value, "/input_audio/data")
                    .map_or(0, |data| media_tokens(Some("audio/"), data)),
                _ => ["inlineData", "inline_data"]
                    .iter()
                    .filter_map(|key| obj.get(*key))
                    .map(|media| {
                        let mime =
                            str_at(media, "/mimeType").or_else(|| str_at(media, "/mime_type"));
                        str_at(media, "/data").map_or(0, |data| media_tokens(mime, data))
                    })
                    .fold(0, u32::saturating_add),
            };
            obj.values()
                .map(count_attachments)
                .fold(own, u32::saturating_add)
        }
        _ => 0,
    }
}

fn str_at<'a>(value: &'a serde_json::Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(serde_json::Value::as_str)
}

fn data_url_tokens(url: &str) -> u32 {
    url.strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
        .map_or(0, |(mime, data)| media_tokens(Some(mime), data))
}

/// 按媒体类型估 base64 数据的 token 上界；图片另按张计，其它类型无从估计。
fn media_tokens(mime: Option<&str>, base64_data: &str) -> u32 {
    use base64::Engine as _;
    let mime = mime.unwrap_or_default();
    if mime.starts_with("audio/") {
        return saturating_u32(base64_data.len() / 4 * 3 / AUDIO_BYTES_PER_TOKEN);
    }
    if mime != "application/pdf" {
        return 0;
    }
    let pages = base64::prelude::BASE64_STANDARD
        .decode(base64_data.trim())
        .map_or(1, |pdf| pdf_pages(&pdf));
    pages.max(1).saturating_mul(PDF_PAGE_TOKENS)
}

/// 页对象数：`/Type /Page`（不含 `/Pages` 目录节点），空格可有可无。
fn pdf_pages(pdf: &[u8]) -> u32 {
    let mut pages = 0_u32;
    for (index, _) in pdf.windows(5).enumerate().filter(|(_, w)| *w == b"/Type") {
        let rest = &pdf[index + 5..];
        let rest = &rest[rest.iter().take_while(|b| b.is_ascii_whitespace()).count()..];
        if rest.starts_with(b"/Page") && !rest.starts_with(b"/Pages") {
            pages = pages.saturating_add(1);
        }
    }
    pages
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn count_images(value: &serde_json::Value) -> u32 {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().map(count_images).fold(0, u32::saturating_add)
        }
        serde_json::Value::Object(obj) => {
            let typed = matches!(
                obj.get("type").and_then(serde_json::Value::as_str),
                Some("image_url" | "image" | "input_image")
            );
            let gemini = ["inlineData", "inline_data", "fileData", "file_data"]
                .iter()
                .filter_map(|key| obj.get(*key))
                .any(|media| {
                    media
                        .get("mimeType")
                        .or_else(|| media.get("mime_type"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|mime| mime.starts_with("image/"))
                });
            obj.values()
                .map(count_images)
                .fold(u32::from(typed || gemini), u32::saturating_add)
        }
        _ => 0,
    }
}

/// Gemini 支持函数和原生工具；tools 数组非空即需要工具能力。
/// contents 部件带 inlineData / fileData（图像 mime）即需要视觉。
fn gemini_request_features(v: &serde_json::Value) -> (bool, bool) {
    let needs_tools = v
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let is_image = |media: &serde_json::Value| {
        media
            .get("mimeType")
            .or_else(|| media.get("mime_type"))
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.starts_with("image/"))
    };
    let needs_vision = v
        .get("contents")
        .and_then(|c| c.as_array())
        .is_some_and(|contents| {
            contents.iter().any(|c| {
                c.get("parts")
                    .and_then(|p| p.as_array())
                    .is_some_and(|parts| {
                        parts.iter().any(|p| {
                            p.get("inlineData")
                                .or_else(|| p.get("inline_data"))
                                .or_else(|| p.get("fileData"))
                                .or_else(|| p.get("file_data"))
                                .is_some_and(is_image)
                        })
                    })
            })
        });
    (needs_tools, needs_vision)
}

/// 模型解析 + 修饰符（§4.4 / §11.25）：全名（含别名）直命中优先，未命中才按修饰符
/// 语法剥基名重试。`@key:value` 与旧的 `-high/-thinking[-N]` 两种写法都认，且**归一到
/// 同一个规范计费名**。
///
/// 返回的第三项是**规范计费名**（如 `gpt-5@effort:high`）：它只用于定价与记账，
/// 路由仍走基座 canonical——渠道声明的是基座模型名，拿变体名去选渠道会一个候选都选不到。
async fn resolve_with_directive(
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
    // 名字里带 `@` 一律按修饰符处理：`@` 是 okapi 自己的分隔符，价簿里叫
    // `base@effort:high` 的行**是一条定价变体、不是一个上游模型**——若让它走"全名直命中"，
    // 路由就会拿变体名去选渠道，而渠道声明的是基座名，结果是一个候选都选不到（503）。
    // 不带 `@` 时仍是全名优先，好让真实存在的 `o3-high` 这类模型不被旧后缀误剥。
    let has_modifier_sep = requested.contains('@');
    if !has_modifier_sep && let Some(meta) = resolve_model_cached(state, requested).await?.as_ref()
    {
        return Ok(Some((meta.clone(), None, None)));
    }
    if let Some(m) = okapi_providers::modifiers::split_model_modifiers(requested)
        && let Some(meta) = resolve_model_cached(state, m.base()).await?.as_ref()
    {
        // 规范名以**解析后的 canonical 基座**为前缀：别名与本名要落到同一条账
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

async fn handle_chat(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: Uuid,
    started: Instant,
    ingress: Ingress,
    info: &ProbeInfo,
) -> Result<Response, AppError> {
    let bill = prepare_chat(state, headers, body, request_id, started, ingress, info).await?;
    // 预扣已建立且两行之间没有 await：此后 handler 被丢弃也会退款
    let cancel = CancelRefund::arm(&bill);
    let outcome = match Box::pin(forward(&bill, info, body)).await {
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
    };
    cancel.disarm();
    outcome
}

// HTTP 与 Responses WS 每轮共用的鉴权、限额、报价与预扣链。
#[allow(clippy::too_many_lines)]
async fn prepare_chat(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    request_id: Uuid,
    started: Instant,
    ingress: Ingress,
    info: &ProbeInfo,
) -> Result<RequestBilling, AppError> {
    let key = match &info.authenticated {
        Some(key) => Arc::clone(key),
        None => super::auth::authenticate_data_plane(state, headers).await?,
    };

    // 模型解析（#3001 + §5.1）：别名→canonical + max_output；60s 进程缓存消除热路径 PG 读；
    // 未命中剥 reasoning 后缀重试（§4.4）
    let Some((meta, directive, variant)) =
        resolve_with_directive(state, &info.requested_model).await?
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

    let response_parent = if matches!(ingress, Ingress::Responses | Ingress::ResponsesCompact) {
        response_affinity::resolve_parent(&state.sched, key.user_id, key.key_id, body).await?
    } else {
        None
    };

    let book = state.pricebook.load();
    let rules_in = super::rule_inputs::collect(state, &book, key.user_id).await;
    let now = chrono::Utc::now();
    let minute_of_day = u16::try_from(
        (now.timestamp()
            .saturating_add(i64::from(
                now.with_timezone(&chrono::Local).offset().local_minus_utc(),
            ))
            .div_euclid(60))
        .rem_euclid(1440),
    )
    .unwrap_or(0);
    // 计价名（§11.25）：变体在价簿里配了价就按变体收，否则回退基座——修饰符改的是
    // 上游行为，站长愿不愿意为它单独定价是另一回事，没配价不该让请求失败。
    // 路由仍用 `canonical`：渠道声明的是基座模型名。
    let billing_model = variant
        .filter(|v| book.has_model(&ModelCode::from(v.as_str())))
        .unwrap_or_else(|| canonical.clone());
    let calc = CalcContext {
        user: UserId::new(key.user_id),
        model: ModelCode::from(billing_model.as_str()),
        group: GroupCode::from(key.group_code.as_str()),
        user_multiplier: RatioFp::from_scaled(key.multiplier_scaled).unwrap_or(RatioFp::ONE),
        monthly_tokens: rules_in.monthly_tokens,
        monthly_spend_micro: rules_in.monthly_spend_micro,
        local_minute_of_day: minute_of_day,
        now_unix: now.timestamp(),
        utc_offset_seconds: now.with_timezone(&chrono::Local).offset().local_minus_utc(),
        surge_active: rules_in.surge_active,
        // 预扣按请求声明档估（贵档多预扣；结算档只降不升另选）
        service_tier: info.service_tier.clone(),
    };

    // 估价（预扣补全缺省 = models.max_output，无则 2048，§5.1）
    let est_prompt = info.prompt_tokens;
    let density = estimate::prompt_density(est_prompt, info.prompt_chars);
    // 图片输入另计（密度只描述文本，先算完再加）
    let est_prompt = est_prompt
        .saturating_add(image_inputs(ingress, body).saturating_mul(IMAGE_INPUT_TOKENS))
        .saturating_add(attachment_tokens(ingress, body));
    let completion_cap = admitted_completion_cap(
        info.completion_cap_req,
        meta.max_output.and_then(|v| u32::try_from(v).ok()),
    );
    let server_tools = super::server_tools::ToolAdmission::parse(body)
        .map_err(|param| AppError::bad_request().with_param(param))?;
    let base_usage = TokenUsage {
        server_tool_usage: server_tools.estimated_usage(),
        prompt_tokens: est_prompt,
        cached_tokens: 0,
        cache_read_reported: false,
        cache_write_reported: false,
        cache_write_tokens: 0,
        audio_prompt_tokens: 0,
        image_prompt_tokens: 0,
        completion_tokens: completion_cap.saturating_mul(info.choices),
        audio_completion_tokens: 0,
        reasoning_tokens: 0,
        ..TokenUsage::default()
    };
    let pool_chain = key.pool_chain();
    let est_usage = with_admission_hints(
        base_usage,
        admission_hints(state, &canonical, &pool_chain).await,
    );
    let est_quote = server_tools.quote(&book, &calc, est_usage)?;

    // reasoning 意图归一（§11.26）：模型名后缀是一条路，请求体参数是另一条，
    // 合并到同一个 directive 后交给三向注入。**参数优先**——它是本次请求的显式意图。
    // 合并只影响"往上游注入什么"，不影响上面 `variant` 已经定下的计费名：
    // 与 OpenRouter 一致，只有模型名上的变体改计价，请求参数不改。
    let directive = reasoning::parse_request(body).merge(directive);

    // 请求级单价上限（§11.24）：拿快照里的**最终**单价判——它已经过模型/分组/个人系数与
    // 修饰器全链，正是这次真会按之的价。判在预扣之前：超限直接拒，别扣了钱再让用户发现贵。
    let prefs = super::routing_prefs::parse(body)?;
    if let Some(over) = price_above_max(&est_quote, &prefs) {
        return Err(
            AppError::new(StatusCode::PAYMENT_REQUIRED, codes::PRICE_ABOVE_MAX).with_param(over),
        );
    }

    // Freeze every usable one-hop fallback before reserve. Never discover a dearer
    // fallback after the hold has already been established.
    let mut reserved_amount = est_quote.amount;
    let mut reserved_completion = est_usage.completion_tokens;
    let mut fallback_models = Vec::new();
    let mut reservation_candidates = vec![super::reservation::Candidate::new(
        "primary",
        &canonical,
        calc.model.as_str(),
        est_usage,
        &est_quote,
    )?];
    if prefs.allow_fallbacks && response_parent.is_none() {
        for fb in meta
            .fallback_models
            .iter()
            .filter(|fb| *fb != &canonical && key.allows_model(fb))
        {
            let Some(fallback) = resolve_model_cached(state, fb).await?.as_ref().clone() else {
                continue;
            };
            if fallback.canonical == canonical {
                continue;
            }
            let mut fallback_calc = calc.clone();
            fallback_calc.model = ModelCode::from(fallback.canonical.as_str());
            let fallback_cap = admitted_completion_cap(
                info.completion_cap_req,
                fallback.max_output.and_then(|n| u32::try_from(n).ok()),
            );
            let fallback_usage = with_admission_hints(
                TokenUsage {
                    completion_tokens: fallback_cap.saturating_mul(info.choices),
                    ..base_usage
                },
                admission_hints(state, &fallback.canonical, &pool_chain).await,
            );
            let Ok(quote) = server_tools.quote(&book, &fallback_calc, fallback_usage) else {
                continue;
            };
            if price_above_max(&quote, &prefs).is_some() {
                continue;
            }
            reservation_candidates.push(super::reservation::Candidate::new(
                "fallback",
                &fallback.canonical,
                fallback_calc.model.as_str(),
                fallback_usage,
                &quote,
            )?);
            reserved_amount = reserved_amount.max(quote.amount);
            reserved_completion = reserved_completion.max(fallback_usage.completion_tokens);
            let has_tier_pricing = book.has_tiers(&fallback_calc.model);
            fallback_models.push(AdmittedFallback {
                model: fallback.canonical.clone(),
                calc: fallback_calc,
                completion_cap: fallback_cap,
                default_output_cap: default_output_cap(
                    info.completion_cap_req,
                    fallback.max_output.and_then(|n| u32::try_from(n).ok()),
                ),
                has_tier_pricing,
            });
        }
    }
    let reservation_snapshot = Arc::new(super::reservation::freeze(
        &info.requested_model,
        reserved_amount,
        reservation_candidates,
    )?);

    // 团成员月度限额（§6.1 软实时）
    super::auth::check_member_limit(state, &key).await?;
    // 分组级 [rpm, rph]（§11.32，限额随鉴权缓存下发）
    super::auth::check_group_rate(state, &key).await?;
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
    let est_tokens = u64::from(est_prompt).saturating_add(u64::from(reserved_completion));
    let (reservation_pool, source_window) = match state
        .reserve_for_key(
            key.quota_limited,
            okapi_ledger::ReserveRequest {
                user_id: key.user_id,
                api_key_id: key.key_id,
                request_id,
                est: reserved_amount,
                caps,
                est_tokens,
            },
            now,
        )
        .await?
    {
        ReserveOutcome::Reserved {
            pool,
            source_window,
            ..
        } => (pool, source_window),
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
    };

    // —— 预扣已建立：此后一切失败路径必须退款（settle_failure）——
    let bill_model_for_tier = canonical.clone();
    let trace = super::diagnostics::Trace::current()
        .unwrap_or_else(|| super::diagnostics::Trace::new(headers));
    if let Some(directive) = directive {
        trace.set(
            "reasoning_effort",
            serde_json::json!(directive.effective_effort().as_str()),
        );
    }
    let bill = RequestBilling {
        trace,
        state: state.clone(),
        ingress,
        book: Arc::clone(&book),
        calc,
        prefs,
        user_id: key.user_id,
        key_id: key.key_id,
        member_user_id: key.member_user_id,
        request_id,
        reservation_pool,
        source_window,
        est_prompt,
        density,
        completion_cap,
        choices: info.choices,
        default_output_cap: default_output_cap(
            info.completion_cap_req,
            meta.max_output.and_then(|v| u32::try_from(v).ok()),
        ),
        directive,
        model: canonical,
        requested_model: info.requested_model.clone(),
        group: key.group_code.clone(),
        is_stream: info.stream,
        started,
        response_parent,
        session: info.session.clone(),
        client_type: detect_client_type(headers),
        client_ip: super::clients::detect_client_ip(headers),
        client_headers: Arc::new(super::oauth_cred::client_headers(headers)),
        pool_chain: key.pool_chain().into_iter().map(str::to_owned).collect(),
        pool_strategy: key.pool_strategy.clone(),
        service_tier: info.service_tier.clone(),
        has_tier_pricing: book.has_tiers(&ModelCode::from(bill_model_for_tier.as_str())),
        fallback_models: Arc::new(fallback_models),
        server_tools,
        reserved_amount,
        reservation_snapshot,
        downgraded_from: None,
        settlement: SettlementHandoff::default(),
    };

    Ok(bill)
}

/// 模型级降级（DESIGN §3.4.1）：请求模型**零可用候选**（渠道停用/冷却/全被限住）
/// 时按 `models.fallback_models` 顺序改投。三条铁律：
/// - 只有零候选（含入口协议不匹配）触发——上游 4xx/5xx 是"打过了没打通"，
///   换模型只会藏住真实错误并让用户为两次调用付钱；
/// - 单跳：只读请求模型自己的链，不递归降级模型的链；
/// - 按实际服务模型计费（fallback_billing 重建计费上下文，快照记 requested_model）。
async fn forward(
    bill: &RequestBilling,
    info: &ProbeInfo,
    body: &Bytes,
) -> Result<Response, ForwardFailure> {
    let first = try_model(bill, info, body).await;
    let zero_candidates = matches!(
        &first,
        Err(f) if matches!(f.error_code.as_str(), codes::NO_AVAILABLE_CHANNEL | codes::UNSUPPORTED_ENDPOINT)
    );
    if !zero_candidates || bill.fallback_models.is_empty() || bill.response_parent.is_some() {
        return first;
    }
    for fb in bill.fallback_models.iter() {
        let fb_bill = fallback_billing(bill, fb);
        tracing::info!(
            request_id = %bill.request_id,
            requested = %bill.model,
            fallback = %fb_bill.model,
            "请求模型零可用候选，模型级降级"
        );
        match try_model(&fb_bill, info, body).await {
            // 降级模型同样零候选 → 链上下一个
            Err(f)
                if matches!(
                    f.error_code.as_str(),
                    codes::NO_AVAILABLE_CHANNEL | codes::UNSUPPORTED_ENDPOINT
                ) => {}
            // 成功或真实上游失败：终止。降级只救"无人可打"，不救"打了没打通"
            other => return other,
        }
    }
    first
}

/// Consume the context priced before admission; do not reload fallback caps here.
fn fallback_billing(bill: &RequestBilling, fb: &AdmittedFallback) -> RequestBilling {
    RequestBilling {
        calc: fb.calc.clone(),
        completion_cap: fb.completion_cap,
        default_output_cap: fb.default_output_cap,
        model: fb.model.clone(),
        has_tier_pricing: fb.has_tier_pricing,
        fallback_models: Arc::new(Vec::new()),
        downgraded_from: Some(bill.model.clone()),
        ..bill.clone()
    }
}

/// 模型在池链上的候选渠道。5s 进程缓存（热路径零 PG 读；console 写路径主动失效，多副本靠 TTL
/// 收敛）；缓存键含池链：不同池的候选集合不同，混用会把别的池的渠道发给用户。
async fn cached_candidates(
    state: &AppState,
    model: &str,
    pool_chain: &[&str],
    fresh: bool,
) -> Result<Arc<Vec<ChannelCandidate>>, okapi_store::StoreError> {
    let cache_key = format!("{model}|{}", pool_chain.join(">"));
    if !fresh && let Some(hit) = state.cand_cache.get(&cache_key).await {
        return Ok(hit);
    }
    let rows = Arc::new(
        okapi_store::channels::candidates_for_model(
            &state.pg,
            model,
            pool_chain,
            state.master_key.as_deref(),
        )
        .await?,
    );
    state.cand_cache.insert(cache_key, Arc::clone(&rows)).await;
    Ok(rows)
}

/// 预扣前看这个模型的候选渠道会怎样改写请求（客户端模拟会加 system 文本、1h 缓存断点，
/// 上游分词器也与本地不同），按其中最贵的一个预扣。查不到候选就按原样估算：
/// 路由阶段会照常报无可用渠道，不在这里提前失败。
async fn admission_hints(
    state: &AppState,
    model: &str,
    pool_chain: &[&str],
) -> okapi_providers::profiles::AdmissionHints {
    cached_candidates(state, model, pool_chain, false)
        .await
        .map(|candidates| {
            candidates
                .iter()
                .map(|candidate| okapi_providers::profiles::admission_hints(&candidate.extensions))
                .fold(
                    okapi_providers::profiles::AdmissionHints::default(),
                    okapi_providers::profiles::AdmissionHints::max,
                )
        })
        .unwrap_or_default()
}

/// 把客户端配置带来的成本放进预扣估算：放大 prompt、按 1h 缓存写入计价（首个请求的上界）。
fn with_admission_hints(
    usage: TokenUsage,
    hints: okapi_providers::profiles::AdmissionHints,
) -> TokenUsage {
    let prompt = hints.prompt_tokens(usage.prompt_tokens);
    if hints.prompt_cache_write_1h {
        TokenUsage {
            prompt_tokens: prompt,
            cache_write_tokens: prompt,
            cache_write_reported: true,
            cache_write_5m_tokens: Some(0),
            cache_write_1h_tokens: Some(prompt),
            ..usage
        }
    } else {
        TokenUsage {
            prompt_tokens: prompt,
            ..usage
        }
    }
}

// Responses WS 逐轮从数据库读取候选，复用 HTTP 的权限/能力/历史绑定过滤。
#[allow(clippy::too_many_lines)]
async fn eligible_candidates(
    bill: &RequestBilling,
    info: &ProbeInfo,
    fresh: bool,
) -> Result<(super::scheduler::CandidateQueue, Option<i64>), ForwardFailure> {
    // 候选 5s 进程缓存（热路径零 PG 读；console 写路径主动失效，多副本靠 TTL 收敛）
    // 缓存键含池链：不同池的候选集合不同，混用会把别的池的渠道发给用户
    use super::scheduler::{CandidateQueue, CandidateSet};
    let chain: Vec<&str> = bill.pool_chain.iter().map(String::as_str).collect();
    let raw = cached_candidates(
        &bill.state,
        &bill.model,
        &chain,
        fresh || bill.response_parent.is_some(),
    )
    .await
    .map_err(|e| ForwardFailure::app(AppError::from(e), 0, None))?;
    let mut candidates = match Strategy::parse(bill.pool_strategy.as_deref()) {
        Strategy::PriorityWeighted => CandidateQueue::weighted(Arc::clone(&raw)),
        Strategy::LeastLatency => {
            let latency = bill
                .state
                .sched
                .channel_key_latencies(raw.iter().map(|c| c.channel_key_id))
                .await;
            CandidateQueue::by_latency(Arc::clone(&raw), &latency)
        }
    };
    // 与诊断和接入示例共用入口规则；配置正常但入口错误时不要报服务不可用。
    candidates.retain(|c| bill.ingress.accepts(c, &bill.model));
    if !raw.is_empty() && candidates.is_empty() {
        return Err(ForwardFailure::app(
            AppError::new(StatusCode::BAD_REQUEST, codes::UNSUPPORTED_ENDPOINT)
                .with_param(Ingress::available_endpoints(raw.as_ref(), &bill.model).join(",")),
            0,
            None,
        ));
    }
    let requirements = Requirements {
        tools: info.needs_tools,
        vision: info.needs_vision,
        server_tools: bill.server_tools.has_tools(),
    };
    candidates
        .retain(|c| ExecutionPlan::compile(bill.ingress, c, &bill.model, requirements).is_ok());
    // 零留存要求（§11.24）：只留声明 data_retention='none' 的渠道。
    // 未声明按不满足处理——"不知道对方留不留"不能当成"不留"。
    // 单独给错误码：候选被这一条筛空时，回 no_available_channel 会让人以为渠道全挂了。
    if bill.prefs.zero_retention {
        let before = candidates.len();
        candidates
            .retain(|c| super::routing_prefs::retention_ok(c.data_retention.as_deref(), true));
        if candidates.is_empty() {
            return Err(ForwardFailure::app(
                AppError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    codes::NO_ZERO_RETENTION_CHANNEL,
                )
                .with_param(before.to_string()),
                0,
                None,
            ));
        }
    }
    // 负毛利熔断（§11.34）：该分组打这条渠道在亏钱，摘掉让同池其它渠道承接
    let margin_removed = bill
        .state
        .retain_margin_ok(&bill.group, &mut candidates)
        .await;
    if candidates.is_empty() {
        return Err(ForwardFailure::app(
            AppError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                super::state::no_candidates_code(margin_removed),
            ),
            0,
            None,
        ));
    }

    // 续聊只允许当前仍有权限、能力和可用性的原账号；不可借 L2 或 failover 改投。
    if let Some(parent) = &bill.response_parent {
        candidates.retain(|c| parent.binding.matches(c));
        if candidates.is_empty() {
            return Err(ForwardFailure::app(
                response_affinity::unavailable(),
                0,
                None,
            ));
        }
    }

    // L2 会话粘性命中：把映射的 channel_key 提到候选首位（§3.2）
    let mut sticky_key: Option<i64> = None;
    if let Some(session) = &bill.session {
        sticky_key = bill.state.sched.sticky_get(bill.user_id, session).await;
        let position =
            sticky_key.and_then(|kid| candidates.iter().position(|c| c.channel_key_id == kid));
        if let Some(position) = position {
            let hit = candidates.remove(position);
            candidates.insert(0, hit);
        }
    }

    Ok((candidates, sticky_key))
}

/// Read-only playground contract: same identity, canonical model, group and
/// routing pool as a real request; no reservation, rate counter or upstream call.
pub(crate) async fn playground_parameters(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
) -> Result<okapi_providers::model_parameters::ParameterProfile, AppError> {
    use okapi_providers::model_parameters::{self, ParameterProfile};
    let key = super::auth::authenticate(state, headers).await?;
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
        let dialect = super::dialect::upstream_dialect(&c.provider, upstream);
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

// failover 主循环：候选过滤/粘性/信号量/状态机联动的完整语义在同一视野内更可读
#[allow(clippy::too_many_lines)]
async fn try_model(
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

    for mut cand in candidates {
        if attempted >= MAX_ATTEMPTS {
            break;
        }
        // key 级 RPM 闸：未配置上限时不产生任何 Redis 往返
        if let Some(limit) = cand.rpm_limit
            && !bill
                .state
                .sched
                .channel_key_rate_ok(cand.channel_key_id, i64::from(limit))
                .await
        {
            tracing::debug!(channel_key = cand.channel_key_id, "key RPM 超限，跳过候选");
            continue;
        }
        // key 级当日消费闸（软实时：结算后累加，故可能略超）
        if let Some(cap) = cand.daily_spend_cap_micro
            && bill
                .state
                .sched
                .channel_key_spend_get(cand.channel_key_id)
                .await
                >= cap
        {
            tracing::debug!(
                channel_key = cand.channel_key_id,
                "key 当日消费已达上限，跳过候选"
            );
            continue;
        }
        attempted += 1;

        let upstream_model = cand.upstream_model(&bill.model).to_owned();
        let Ok(mut body_up) = build_upstream_body(bill, info, &cand, body, &upstream_model) else {
            return Err(ForwardFailure::app(
                AppError::bad_request(),
                failover,
                Some((cand.channel_id, cand.channel_key_id)),
            ));
        };
        let base = cand.api_base.clone().unwrap_or_else(|| {
            okapi_providers::registry::lookup(&cand.provider)
                .and_then(|adapter| adapter.default_base)
                .unwrap_or_default()
                .to_owned()
        });
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
        let oauth = super::oauth_cred::is_oauth_provider(&cand.provider);
        let mut refreshed_401 = false;
        let attempt = loop {
            // Keep the candidate equal to the token actually sent; a forced refresh can
            // then distinguish a rejected token from one another request just rotated.
            if oauth {
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(bill.started + Duration::from_mins(8)),
                    super::oauth_cred::fresh_credential(&bill.state, &cand),
                )
                .await
                .unwrap_or(Err(UpstreamError::Timeout))
                {
                    Ok(credential) => cand.credential = credential.to_plaintext(),
                    Err(err) => {
                        break Err(classify_fatal(
                            super::oauth_cred::unavailable(err),
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
                        super::oauth_cred::refresh_rejected_credential(&bill.state, &cand),
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
            // Responses 直转撞上 404/405 = 这个上游根本没有 /responses（"openai" 渠道指着
            // 只实现了 chat 的第三方地址是常态）。同一候选就地改走降级链再来一次：
            // 不计 failover、不计 retry、不标 key 失败——渠道没坏，是方言不对。
            if bill.ingress == Ingress::Responses
                && bill.response_parent.is_none()
                && cand.responses_native
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
                if let Some(session) = &bill.session {
                    bill.state
                        .sched
                        .sticky_set(bill.user_id, session, cand.channel_key_id)
                        .await;
                }
                super::key_health::success(&bill.state, &cand).await;
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
                    last_code = code;
                    continue;
                }
                tracing::warn!(
                    request_id = %bill.request_id,
                    channel_key = cand.channel_key_id,
                    code,
                    "首字前失败，failover 下一候选"
                );
                super::key_health::failure(&bill.state, &cand, code, failure_kind).await;
                last_code = code;
                last_status = upstream_status;
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

    let err_code = if last_code == codes::EMPTY_COMPLETION {
        codes::EMPTY_COMPLETION
    } else {
        last_code
    };
    let mut failure = ForwardFailure::app(
        AppError::new(
            if err_code == codes::NO_AVAILABLE_CHANNEL {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_GATEWAY
            },
            err_code,
        ),
        failover,
        last_channel,
    );
    failure.upstream_status = last_status;
    failure.upstream = last_upstream.map(Box::new);
    Err(failure)
}

/// 按 入口协议 × 渠道协议 构造上游请求体：同方言重写 model 透传，跨方言走出向转换；
/// Responses 对说 Responses 方言的渠道直转，否则先降级 chat，再按渠道协议二段转换。
/// 末尾做 reasoning 后缀注入（按上游方言；显式字段不覆盖）。
fn build_upstream_body(
    bill: &RequestBilling,
    info: &ProbeInfo,
    cand: &ChannelCandidate,
    body: &Bytes,
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
    let native_responses = plan.native_responses();
    let dialect = plan.dialect.as_str();
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
            body,
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
fn with_anthropic_default_cap(body: Bytes, cap: u32) -> Result<Bytes, UpstreamError> {
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

fn classify_fatal(err: UpstreamError, failover: i16, channel: (i64, i64)) -> AttemptError {
    if let Some(trace) = super::diagnostics::Trace::current() {
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
        UpstreamError::Build(_) => AttemptError::Fatal(ForwardFailure::app(
            AppError::bad_request(),
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
    const PROTECTED: [&str; 5] = [
        "model",
        "messages",
        "stream",
        "previous_response_id",
        "conversation",
    ];
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

/// 渠道强制注入（在 strip 之后）：浅合并顶层键。受保护键跳过。
fn inject_request_fields(
    body: &Bytes,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Option<Bytes> {
    const PROTECTED: [&str; 6] = [
        "model",
        "messages",
        "stream",
        "provider",
        "previous_response_id",
        "conversation",
    ];
    if fields.is_empty() {
        return None;
    }
    let mut value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let obj = value.as_object_mut()?;
    let mut changed = false;
    for (key, val) in fields {
        if PROTECTED.contains(&key.as_str()) {
            continue;
        }
        obj.insert(key.clone(), val.clone());
        changed = true;
    }
    changed.then(|| Bytes::from(serde_json::to_vec(&value).unwrap_or_default()))
}

fn shape_upstream_body(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    body: Bytes,
) -> Result<Bytes, UpstreamError> {
    // okapi 自己的路由指令必须先剥掉：上游不认识 `provider`，会 400。
    // 与渠道级 strip_request_fields 分开做——那是管理员配置，这是协议要求，不可关。
    let body = super::routing_prefs::strip(&body).unwrap_or(body);
    let native_responses =
        ExecutionPlan::compile(bill.ingress, cand, &bill.model, Requirements::default())?
            .native_responses();
    // 统一 `reasoning` 对象同理：意图已翻译进各方言的原生字段，原对象上游不认识（§11.26）。
    // Responses 直转是唯一的例外——`reasoning.effort` 就是上游的原生键，只摘非原生键。
    let body = if native_responses {
        reasoning::strip_unified_for_responses(&body).unwrap_or(body)
    } else {
        reasoning::strip_unified(&body).unwrap_or(body)
    };
    let body = if cand.strip_request_fields.is_empty() {
        body
    } else {
        strip_request_fields(&body, &cand.strip_request_fields).unwrap_or(body)
    };
    let body = if cand.inject_request_fields.is_empty() {
        body
    } else {
        inject_request_fields(&body, &cand.inject_request_fields).unwrap_or(body)
    };
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
async fn dispatch_chat(
    bill: &RequestBilling,
    cand: &ChannelCandidate,
    base: &str,
    body: Bytes,
    stream: bool,
) -> Result<ChatResponse, UpstreamError> {
    use futures::StreamExt as _;
    let plan = ExecutionPlan::compile(bill.ingress, cand, &bill.model, Requirements::default())?;
    let native_responses = plan.native_responses();
    let body = bound_default_output(bill.ingress, bill.default_output_cap, body);
    let body = shape_upstream_body(bill, cand, body)?;
    bill.server_tools.verify_outbound(&body)?;
    let upstream_model = cand.upstream_model(&bill.model).to_owned();
    // 订阅 provider 额外带上客户端身份头（真实 Claude Code / Codex CLI 经网关出去时上游看到它自己）
    let outbound = super::oauth_cred::outbound_with_client(cand, &bill.client_headers);
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

fn response_writer(bill: &RequestBilling, cand: &ChannelCandidate) -> Option<ResponseWriter> {
    (bill.ingress == Ingress::Responses && cand.responses_native)
        .then(|| ResponseWriter::new(ResponseBinding::from_candidate(cand)))
}

async fn capture_response_event(
    writer: &mut Option<ResponseWriter>,
    bill: &RequestBilling,
    event: &ChatEvent,
) -> Result<(), AppError> {
    if let (Some(writer), ChatEvent::Data { raw, .. }) = (writer, event) {
        writer
            .capture(&bill.state.sched, bill.user_id, bill.key_id, raw.as_bytes())
            .await?;
    }
    Ok(())
}

// ---- 流式 ----

fn first_output_buffer_full(buffered: &[ChatEvent]) -> bool {
    buffered.len() >= 256
        || buffered
            .iter()
            .filter_map(|e| match e {
                ChatEvent::Data { raw, .. } => Some(raw.len()),
                ChatEvent::Done => None,
            })
            .sum::<usize>()
            > 16 * 1024 * 1024
}

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
    let first_output_window = first_output_window(cand);
    let resp = match tokio::time::timeout(first_output_window, connect).await {
        Err(_) => {
            bill.trace.failure(&UpstreamError::Timeout);
            return Err(AttemptError::Retriable {
                code: codes::UPSTREAM_TIMEOUT,
                upstream_status: None,
                failure_kind: KeyFailure::Request,
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
    let first = tokio::time::timeout(first_output_window, async {
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
                    if first_output_buffer_full(&buffered) {
                        return Err(UpstreamError::Stream("first_output_buffer_limit".into()));
                    }
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
        Err(_) => {
            bill.trace.failure(&UpstreamError::Timeout);
            Err(AttemptError::Retriable {
                code: codes::UPSTREAM_TIMEOUT,
                upstream_status: None,
                failure_kind: KeyFailure::Request,
            })
        }
        Ok(Err(err)) => Err(classify_fatal(err, failover, channel)),
        Ok(Ok(false)) => {
            bill.trace
                .failure(&UpstreamError::Stream(codes::EMPTY_COMPLETION.into()));
            Err(empty_completion(bill.ingress, &buffered))
        }
        Ok(Ok(true)) => {
            let mut writer = response_writer(bill, cand);
            for event in &buffered {
                capture_response_event(&mut writer, bill, event)
                    .await
                    .map_err(|err| {
                        AttemptError::Fatal(ForwardFailure::app(err, failover, Some(channel)))
                    })?;
            }
            let ttft_ms = elapsed_ms_i32(bill.started);
            Ok(spawn_stream_pump(
                bill.clone(),
                cand_info(
                    cand,
                    &bill.model,
                    bill.is_stream,
                    bill.ingress,
                    sticky_layer,
                    retry,
                ),
                handle,
                buffered,
                writer,
                ttft_ms,
                failover,
            )
            .await)
        }
    }
}

/// 空流照旧换渠道、不计费；只有说不出原因的空流才记成这把 key 的瞬态失败。
fn empty_completion(ingress: Ingress, buffered: &[ChatEvent]) -> AttemptError {
    AttemptError::Retriable {
        code: codes::EMPTY_COMPLETION,
        upstream_status: None,
        failure_kind: if request_caused_stop(ingress, buffered) {
            KeyFailure::Request
        } else {
            KeyFailure::Transient
        },
    }
}

/// 空流是不是请求自己收的尾：输出预算耗尽（推理模型把 max_tokens 全花在思考上）或
/// 内容策略拦截。这说明不了凭证好坏——若记成 key 的瞬态失败，调用方用几次极小的
/// max_completion_tokens 就能把整组 key 打进冷却。事件已是客户端方言（转换器已对齐各家原因）。
fn request_caused_stop(ingress: Ingress, events: &[ChatEvent]) -> bool {
    use serde_json::Value;
    events.iter().any(|event| {
        let ChatEvent::Data { raw, .. } = event else {
            return false;
        };
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return false;
        };
        let reason = |pointer: &str| value.pointer(pointer).and_then(Value::as_str);
        let any_reason = |list: &str, field: &str, stops: &[&str]| {
            value
                .get(list)
                .and_then(Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get(field)
                            .and_then(Value::as_str)
                            .is_some_and(|stop| stops.contains(&stop))
                    })
                })
        };
        match ingress {
            Ingress::OpenAi => {
                any_reason("choices", "finish_reason", &["length", "content_filter"])
            }
            Ingress::Anthropic => matches!(
                reason("/delta/stop_reason"),
                Some("max_tokens" | "refusal" | "model_context_window_exceeded")
            ),
            Ingress::Responses | Ingress::ResponsesCompact => matches!(
                reason("/response/incomplete_details/reason"),
                Some("max_output_tokens" | "content_filter")
            ),
            Ingress::Gemini => {
                reason("/promptFeedback/blockReason").is_some_and(|block| !block.is_empty())
                    || any_reason(
                        "candidates",
                        "finishReason",
                        &[
                            "MAX_TOKENS",
                            "SAFETY",
                            "RECITATION",
                            "BLOCKLIST",
                            "PROHIBITED_CONTENT",
                            "SPII",
                            "IMAGE_SAFETY",
                        ],
                    )
            }
        }
    })
}

#[cfg(test)]
mod empty_stop_tests {
    use super::*;

    fn data(raw: &serde_json::Value) -> ChatEvent {
        ChatEvent::Data {
            event: None,
            raw: raw.to_string(),
            content_chars: 0,
            usage: None,
            has_output: false,
        }
    }

    #[test]
    fn budget_and_policy_stops_are_not_credential_failures() {
        for (ingress, raw) in [
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}),
            ),
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}),
            ),
            (
                Ingress::Anthropic,
                serde_json::json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
            ),
            (
                Ingress::Responses,
                serde_json::json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
            ),
            (
                Ingress::Gemini,
                serde_json::json!({"candidates":[{"finishReason":"MAX_TOKENS","content":{"parts":[]}}]}),
            ),
            (
                Ingress::Gemini,
                serde_json::json!({"promptFeedback":{"blockReason":"SAFETY"}}),
            ),
        ] {
            assert!(request_caused_stop(ingress, &[data(&raw)]), "{raw}");
        }
    }

    #[test]
    fn unexplained_empty_streams_still_count_against_the_key() {
        for (ingress, raw) in [
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
            ),
            (
                Ingress::OpenAi,
                serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            ),
            (
                Ingress::Anthropic,
                serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            ),
            (
                Ingress::Gemini,
                serde_json::json!({"candidates":[{"finishReason":"STOP"}]}),
            ),
            // 方言不对的原因字段不能被误认
            (
                Ingress::Anthropic,
                serde_json::json!({"choices":[{"finish_reason":"length"}]}),
            ),
        ] {
            assert!(!request_caused_stop(ingress, &[data(&raw)]), "{raw}");
        }
        assert!(!request_caused_stop(Ingress::OpenAi, &[ChatEvent::Done]));
    }
}

#[derive(Clone)]
struct CandInfo {
    /// A generated WS turn can have billable usage and still end in an error.
    outcome: Option<(i16, String)>,
    channel: i64,
    cost_milli: i64,
    key: i64,
    sticky_layer: i16,
    /// 同 key 重试次数（§3.6，记账列 retry_count）。
    retry: i16,
    upstream_request_id: Option<String>,
    /// 渠道开关：按上游响应模型计费（Sub2API 0.1.175 对齐）。
    bill_resp_model: bool,
    /// 渠道开关：不信任上游 usage，结算前本地复核（取两者较大值）。
    trust_usage: bool,
    upstream_model: String,
    upstream_endpoint: String,
}

/// 该渠道的首字窗口：未配 `retry_policy.first_output_timeout_secs` 时用全局缺省。
fn first_output_window(cand: &ChannelCandidate) -> Duration {
    if cand.first_output_timeout_secs == FIRST_OUTPUT_TIMEOUT.as_secs() {
        FIRST_OUTPUT_TIMEOUT
    } else {
        Duration::from_secs(cand.first_output_timeout_secs)
    }
}

fn cand_info(
    cand: &ChannelCandidate,
    model: &str,
    stream: bool,
    ingress: Ingress,
    sticky_layer: i16,
    retry: i16,
) -> CandInfo {
    CandInfo {
        outcome: None,
        channel: cand.channel_id,
        cost_milli: cand.cost_milli,
        key: cand.channel_key_id,
        sticky_layer,
        retry,
        upstream_request_id: None,
        bill_resp_model: cand.bill_by_response_model,
        trust_usage: cand.trust_upstream_usage,
        upstream_model: cand.upstream_model(model).to_owned(),
        upstream_endpoint: upstream_endpoint(cand, stream, ingress).to_owned(),
    }
}
fn upstream_endpoint(cand: &ChannelCandidate, stream: bool, ingress: Ingress) -> &'static str {
    match (ingress, cand.provider.as_str()) {
        (Ingress::ResponsesCompact, _) => "/v1/responses/compact",
        (Ingress::Responses, _) if cand.responses_native => "/v1/responses",
        (_, "anthropic") => "/v1/messages",
        (_, "bedrock") if stream => "/model/{model}/invoke-with-response-stream",
        (_, "bedrock") => "/model/{model}/invoke",
        (_, "vertex") if stream => "/publishers/{publisher}/models/{model}:stream",
        (_, "vertex") => "/publishers/{publisher}/models/{model}:predict",
        (Ingress::OpenAi | Ingress::Responses | Ingress::Gemini, "gemini") if stream => {
            "/v1beta/models/{model}:streamGenerateContent"
        }
        (Ingress::OpenAi | Ingress::Responses | Ingress::Gemini, "gemini") => {
            "/v1beta/models/{model}:generateContent"
        }
        (_, "azure") => "/openai/deployments/{model}/chat/completions",
        _ => "/v1/chat/completions",
    }
}

#[allow(clippy::too_many_lines)]
async fn spawn_stream_pump(
    bill: RequestBilling,
    mut info: CandInfo,
    mut handle: StreamHandle,
    buffered: Vec<ChatEvent>,
    mut writer: Option<ResponseWriter>,
    ttft_ms: i32,
    failover: i16,
) -> Response {
    let setting = bill.state.setting_cached("streaming_policy").await;
    let policy = super::stream_policy::StreamPolicy::from_setting(setting.as_ref().as_ref());
    info.upstream_request_id = handle.upstream_request_id.take();
    let request_id = bill.request_id;
    let (mut tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);

    // pump 生命周期与上游流绑定；客户端断开经 send 失败感知并取消上游，
    // 结算在任何退出路径都执行（settle_stream）。经 settlements 计数：优雅下线要等它落账。
    let settlements = bill.state.settlements.clone();
    bill.settlement.hand_off();
    settlements.spawn(async move {
        let mut usage: Option<UsageProbe> = None;
        let mut content_chars: usize = 0;
        let mut client_gone = false;
        // Observe the returned model independently of the opt-in billing policy.
        let mut resp_meta = RespMeta::default();
        let mut next_sequence = 0;

        let mut terminal = Vec::new();
        for event in buffered {
            if let ChatEvent::Data { raw, .. } = &event { bill.trace.stream_event(raw); }
            if resp_meta.model.is_none() || (bill.has_tier_pricing && resp_meta.service_tier.is_none()) {
                capture_chunk_meta(&event, &mut resp_meta);
            }
            if defer_settlement_terminal(&event, bill.ingress) {
                capture_terminal_usage(&event, &mut usage, &mut content_chars);
                terminal.push(event);
                continue;
            }
            advance_stream_sequence(&event, bill.ingress, &mut next_sequence);
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
            let next = tokio::time::timeout_at(policy.deadline(tokio::time::Instant::from_std(bill.started + Duration::from_mins(8))), handle.events.next()).await
                .unwrap_or(Some(Err(UpstreamError::Timeout)));
            match next {
                Some(Ok(event)) => {
                    if let ChatEvent::Data { raw, .. } = &event { bill.trace.stream_event(raw); }
                    if resp_meta.model.is_none() || (bill.has_tier_pricing && resp_meta.service_tier.is_none()) {
                        capture_chunk_meta(&event, &mut resp_meta);
                    }
                    if let Err(err) = capture_response_event(&mut writer, &bill, &event).await {
                        // 首字已发送：终止而不改投，保留终态 usage 供实际产出结算。
                        if let ChatEvent::Data { usage: Some(reported), .. } = &event { usage = Some(reported.with_previous(usage)); }
                        let event = super::error::stream_error_event(bill.ingress, &err, request_id, Some(next_sequence));
                        let _ = tokio::time::timeout(Duration::from_secs(5), tx.send(Ok(event))).await;
                        break;
                    }
                    saw_done = matches!(event, ChatEvent::Done);
                    if defer_settlement_terminal(&event, bill.ingress) {
                        capture_terminal_usage(&event, &mut usage, &mut content_chars);
                        if terminal.len() >= 16 { break; }
                        terminal.push(event);
                        continue;
                    }
                    advance_stream_sequence(&event, bill.ingress, &mut next_sequence);
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
                    bill.trace.failure(&err);
                    bill.trace.set("request_failed", serde_json::json!(true));
                    bill.trace.set("stream_end_reason", serde_json::json!("upstream_error"));
                    info.outcome = Some((err.upstream_status().unwrap_or(502), err.error_code().into()));
                    tracing::warn!(request_id = %request_id, error = %err, "首字后断流，按已产出结算");
                    let error = AppError::new(if matches!(err, UpstreamError::Timeout | UpstreamError::Unreachable { timed_out: true, .. }) { StatusCode::GATEWAY_TIMEOUT } else { StatusCode::BAD_GATEWAY }, err.error_code());
                    let event = super::error::stream_error_event(bill.ingress, &error, request_id, Some(next_sequence));
                    let _ = tokio::time::timeout(Duration::from_secs(5),tx.send(Ok(event))).await;
                    break;
                }
                None => break,
            }
        }
        drop(handle); // 取消上游（客户端断开路径）
        bill.trace.response_model(resp_meta.model.as_deref());
        if info.outcome.is_none() && bill.trace.snapshot()["request_failed"] == true {
            info.outcome = Some((502, codes::UPSTREAM_ERROR.into()));
        }
        if bill.trace.snapshot().get("stream_end_reason").is_none() {
            bill.trace.set("stream_end_reason", serde_json::json!(if client_gone { "client_closed" } else if saw_done { "completed" } else { "upstream_closed" }));
        }
        if !info.bill_resp_model { resp_meta.model = None; }
        let result = settle_stream(
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
        finish_settled_stream(tx, result, terminal, bill.ingress, request_id, next_sequence).await;
        // 结算完成后释放渠道 key 并发信号量（§3.5）
    });

    let sse = Sse::new(rx).keep_alive(KeepAlive::new().interval(policy.heartbeat).text("ping"));
    with_request_id(sse.into_response(), request_id)
}

fn capture_terminal_usage(event: &ChatEvent, usage: &mut Option<UsageProbe>, chars: &mut usize) {
    if let ChatEvent::Data {
        usage: reported,
        content_chars,
        ..
    } = event
    {
        if let Some(reported) = reported {
            *usage = Some(reported.with_previous(*usage));
        }
        *chars = chars.saturating_add(*content_chars);
    }
}

fn advance_stream_sequence(event: &ChatEvent, ingress: Ingress, next: &mut u64) {
    if matches!(ingress, Ingress::Responses | Ingress::ResponsesCompact)
        && let ChatEvent::Data { raw, .. } = event
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(raw)
        && let Some(sequence) = value
            .get("sequence_number")
            .and_then(serde_json::Value::as_u64)
    {
        *next = (*next).max(sequence.saturating_add(1));
    }
}

async fn finish_settled_stream(
    mut tx: mpsc::Sender<Result<Event, Infallible>>,
    result: Result<(), AppError>,
    terminal: Vec<ChatEvent>,
    ingress: Ingress,
    request_id: Uuid,
    next_sequence: u64,
) {
    if let Err(error) = result {
        tracing::error!(%request_id, ?error, "stream usage persistence failed");
        let event =
            super::error::stream_error_event(ingress, &error, request_id, Some(next_sequence));
        let _ = tokio::time::timeout(Duration::from_secs(5), tx.send(Ok(event))).await;
    } else {
        let mut usage = None;
        let mut chars = 0;
        for event in terminal {
            if !push_event(&mut tx, &event, ingress, &mut usage, &mut chars).await {
                break;
            }
        }
    }
}

fn defer_settlement_terminal(event: &ChatEvent, ingress: Ingress) -> bool {
    match event {
        ChatEvent::Done => true,
        ChatEvent::Data {
            event: name, raw, ..
        } => {
            if matches!(
                name.as_deref(),
                Some(
                    "message_stop"
                        | "response.completed"
                        | "response.failed"
                        | "response.incomplete"
                )
            ) {
                return true;
            }
            if ingress == Ingress::Gemini {
                return serde_json::from_str::<serde_json::Value>(raw)
                    .ok()
                    .and_then(|v| {
                        v.get("candidates")
                            .and_then(serde_json::Value::as_array)
                            .cloned()
                    })
                    .is_some_and(|rows| rows.iter().any(|c| c.get("finishReason").is_some()));
            }
            false
        }
    }
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
            if let Some(reported) = ev_usage {
                *usage = Some(reported.with_previous(*usage));
            }
            let mut ev = Event::default();
            if let Some(name) = name {
                ev = ev.event(name);
            }
            ev.data(raw)
        }
        ChatEvent::Done => match ingress {
            Ingress::OpenAi => Event::default().data("[DONE]"),
            // Anthropic 以 message_stop、Responses 以 response.completed、Gemini 以带
            // finishReason 的 chunk 收尾，均无终止帧
            Ingress::Anthropic
            | Ingress::Responses
            | Ingress::ResponsesCompact
            | Ingress::Gemini => return true,
        },
    };
    tokio::time::timeout(Duration::from_secs(10), tx.send(Ok(sse_event)))
        .await
        .is_ok_and(|result| result.is_ok())
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
) -> Result<(), AppError> {
    let usage = match estimate::resolve_usage(
        usage,
        bill.est_prompt,
        content_chars,
        bill.density,
        info.trust_usage,
    ) {
        Ok(usage) => usage,
        Err(error) => {
            tracing::warn!(request_id = %bill.request_id, %error, "invalid upstream usage");
            let error = AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR);
            let failure = ForwardFailure::app(error, failover, Some((info.channel, info.key)));
            settle_failure(bill, &failure).await;
            return Err(AppError::new(
                StatusCode::BAD_GATEWAY,
                codes::UPSTREAM_ERROR,
            ));
        }
    };
    if client_gone {
        tracing::info!(request_id = %bill.request_id, "客户端提前断开，按已产出结算");
    }
    settle_commit(bill, info, usage, Some(ttft_ms), failover, resp_meta).await
}

/// 上游响应元数据（按需采集：model 供响应模型计费、service_tier 供档位计费）。
#[derive(Default, Clone)]
struct RespMeta {
    model: Option<String>,
    service_tier: Option<String>,
}

/// 流式 chunk 的响应元数据采集（开关/tier 定价启用时才调用；首个非空值生效）。
/// chat chunk 的 model/service_tier 在顶层；Responses 直转事件在 `response` 对象内
/// （response.created 就带 model，service_tier 到 response.completed 才定）。
fn capture_chunk_meta(event: &ChatEvent, meta: &mut RespMeta) {
    #[derive(serde::Deserialize)]
    struct MetaOnly {
        /// Gemini 形状叫 modelVersion
        #[serde(alias = "modelVersion")]
        model: Option<String>,
        service_tier: Option<String>,
        #[serde(alias = "message")]
        response: Option<Box<MetaOnly>>,
    }
    fn absorb(meta: &mut RespMeta, probe: MetaOnly) {
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
        if let Some(inner) = probe.response {
            absorb(meta, *inner);
        }
    }
    if let ChatEvent::Data { raw, .. } = event
        && let Ok(probe) = serde_json::from_str::<MetaOnly>(raw)
    {
        absorb(meta, probe);
    }
}

#[cfg(test)]
mod native_response_meta_tests {
    use super::*;

    #[test]
    fn message_start_model_is_preserved_for_opted_in_response_pricing() {
        let mut meta = RespMeta::default();
        let event = ChatEvent::Data {
            event: Some("message_start".into()),
            raw: serde_json::json!({"type":"message_start","message":{"model":"native-actual"}})
                .to_string(),
            content_chars: 0,
            usage: None,
            has_output: false,
        };
        capture_chunk_meta(&event, &mut meta);
        assert_eq!(meta.model.as_deref(), Some("native-actual"));
        let later = ChatEvent::Data {
            event: None,
            raw: serde_json::json!({"model":"later"}).to_string(),
            content_chars: 0,
            usage: None,
            has_output: false,
        };
        capture_chunk_meta(&later, &mut meta);
        assert_eq!(meta.model.as_deref(), Some("native-actual"));
    }
}

/// 非流式 JSON 响应的元数据提取（开关/tier 定价启用时才调用）。
fn extract_body_meta(body: &Bytes) -> RespMeta {
    #[derive(serde::Deserialize)]
    struct MetaOnly {
        #[serde(alias = "modelVersion")]
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
            if let Some(mut writer) = response_writer(bill, cand) {
                writer
                    .capture(&bill.state.sched, bill.user_id, bill.key_id, &body)
                    .await
                    .map_err(|err| {
                        AttemptError::Fatal(ForwardFailure::app(err, failover, Some(channel)))
                    })?;
            }
            let content_chars = non_stream_content_chars(bill.ingress, &body);
            let usage = estimate::resolve_usage(
                usage,
                bill.est_prompt,
                content_chars,
                bill.density,
                cand.trust_upstream_usage,
            )
            .map_err(|error| {
                tracing::warn!(request_id = %bill.request_id, %error, "invalid upstream usage");
                AttemptError::Fatal(ForwardFailure::app(
                    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR),
                    failover,
                    Some(channel),
                ))
            })?;
            let mut info = cand_info(
                cand,
                &bill.model,
                bill.is_stream,
                bill.ingress,
                sticky_layer,
                retry,
            );
            info.upstream_request_id = upstream_request_id;
            bill.trace
                .response_model(extract_body_meta(&body).model.as_deref());
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
            // Preserve the independently tracked settlement if the client disconnects,
            // and wait for it before returning a complete JSON response.
            let bill_bg = bill.clone();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            bill.settlement.hand_off();
            bill.state.settlements.spawn(async move {
                let result = settle_commit(&bill_bg, &info, usage, None, failover, resp_meta).await;
                let _ = done_tx.send(result);
            });
            done_rx
                .await
                .unwrap_or_else(|_| Err(AppError::internal()))
                .map_err(|error| {
                    AttemptError::Fatal(ForwardFailure::app(error, failover, Some(channel)))
                })?;
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
    use serde_json::Value;
    let Ok(v) = serde_json::from_slice::<Value>(body) else {
        return 0;
    };
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map_or(0, |t| t.chars().count())
    };
    // 无 usage 时的补全估算输入：全部候选、正文与可见推理，以及工具调用参数——
    // 只数首个候选的正文，n>1 与纯工具调用的回答都会被低估成个位数 token。
    match ingress {
        // output 中保留的用户消息不是新生成内容，密文也不能按字符数估算。
        Ingress::ResponsesCompact => 0,
        Ingress::OpenAi => json_items(v.get("choices"))
            .filter_map(|choice| choice.get("message"))
            .map(|message| {
                let reasoning = message
                    .get("reasoning_content")
                    .or_else(|| message.get("reasoning"));
                text(message.get("content"))
                    + text(reasoning)
                    + text(message.get("refusal"))
                    + json_items(message.get("tool_calls"))
                        .map(|call| text(call.pointer("/function/arguments")))
                        .sum::<usize>()
            })
            .sum(),
        Ingress::Anthropic => json_items(v.get("content"))
            .map(|block| {
                text(block.get("text").or_else(|| block.get("thinking")))
                    + tool_argument_chars(block.get("input"))
            })
            .sum(),
        Ingress::Responses => json_items(v.get("output"))
            .map(|item| match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => json_items(item.get("summary"))
                    .map(|part| text(part.get("text")))
                    .sum(),
                Some("function_call") => tool_argument_chars(item.get("arguments")),
                Some("custom_tool_call") => tool_argument_chars(item.get("input")),
                _ => json_items(item.get("content"))
                    .map(|part| text(part.get("text")))
                    .sum(),
            })
            .sum(),
        Ingress::Gemini => json_items(v.get("candidates"))
            .flat_map(|candidate| json_items(candidate.pointer("/content/parts")))
            .map(|part| {
                text(part.get("text"))
                    + tool_argument_chars(
                        part.pointer("/functionCall/args")
                            .or_else(|| part.pointer("/function_call/args")),
                    )
            })
            .sum(),
    }
}

fn json_items(value: Option<&serde_json::Value>) -> impl Iterator<Item = &serde_json::Value> {
    value
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
}

/// 工具参数的字符数：字符串原样计，结构化值按其 JSON 文本计。
fn tool_argument_chars(value: Option<&serde_json::Value>) -> usize {
    match value {
        Some(serde_json::Value::String(s)) => s.chars().count(),
        Some(serde_json::Value::Null) | None => 0,
        Some(value) => value.to_string().chars().count(),
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
    if bill
        .server_tools
        .quote(&bill.book, &candidate, usage)
        .is_ok()
    {
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
) -> Result<(), AppError> {
    let calc_override = resolve_billing_calc(bill, resp_meta.model.as_deref(), usage);
    // 记账的模型名必须与**实际计价所用的名字**一致，否则账单解释器拿 model_name 去查价
    // 会对不上。`bill.calc.model` 就是那个名字：修饰符变体配了价就是变体名
    // （`gpt-5@effort:high`），没配价则已回退成基座名（§11.25）。
    let billed_model: String = if calc_override.is_some() {
        resp_meta
            .model
            .clone()
            .unwrap_or_else(|| bill.calc.model.to_string())
    } else {
        bill.calc.model.to_string()
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
    let (quote, snapshot): (Quote, serde_json::Value) = match bill
        .server_tools
        .validate_usage(
            bill.book.server_tool_prices(&calc.model),
            usage.server_tool_usage,
        )
        .and_then(|()| {
            if bill.server_tools.has_tools() && calc.model != bill.calc.model {
                if bill.book.server_tool_prices(&bill.calc.model).is_some()
                    && bill.book.server_tool_prices(&calc.model).is_none()
                {
                    return Err(okapi_pricing::PricingError::InvalidServerToolAdmission(
                        "response_tool_price_missing",
                    ));
                }
                let estimate = TokenUsage {
                    prompt_tokens: bill.est_prompt,
                    completion_tokens: bill.completion_cap.saturating_mul(bill.choices),
                    server_tool_usage: bill.server_tools.estimated_usage(),
                    ..TokenUsage::default()
                };
                if bill.server_tools.quote(&bill.book, calc, estimate)?.amount
                    > bill.reserved_amount
                {
                    return Err(okapi_pricing::PricingError::InvalidServerToolAdmission(
                        "response_price_above_reservation",
                    ));
                }
            }
            bill.server_tools.quote(&bill.book, calc, usage)
        })
        .and_then(|quote| {
            let snapshot =
                super::reservation::settled_snapshot(&quote.snapshot, &bill.reservation_snapshot)?;
            Ok((quote, snapshot))
        }) {
        Ok(q) => q,
        Err(err) => {
            // 结算算价失败：退款 + 失败记账（fail-closed，不猜测金额）
            tracing::error!(request_id = %bill.request_id, error = %err, "结算算价失败，退款");
            let pool = bill
                .state
                .ledger
                .refund(bill.user_id, bill.key_id, bill.request_id)
                .await
                .map_or(bill.reservation_pool, |r| {
                    if r.released.is_zero() {
                        bill.reservation_pool
                    } else {
                        r.pool
                    }
                });
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
                pool,
            )
            .await;
            return Err(err.into());
        }
    };

    let mut snapshot = super::upstream_cost::snapshot(
        Some(snapshot),
        info.channel,
        info.cost_milli,
        quote.list_price,
    );
    if bill.server_tools.has_tools()
        && let Some(serde_json::Value::Object(map)) = snapshot.as_mut()
    {
        map.insert(
            "server_tool_admission".into(),
            bill.server_tools.snapshot(bill.reserved_amount),
        );
    }
    if let Some(coverage) = bill.server_tools.cost_coverage(usage.server_tool_usage)
        && let Some(serde_json::Value::Object(map)) = snapshot.as_mut()
    {
        map.insert("server_tool_cost_coverage".into(), coverage);
    }
    // 模型级降级的账单可解释性（DESIGN §3.4）：仅降级时写 requested_model，
    // 用户能核对"我要的是 A、实际用了 B、按 B 计价"
    if let Some(from) = &bill.downgraded_from
        && let Some(serde_json::Value::Object(map)) = snapshot.as_mut()
    {
        map.insert("requested_model".into(), serde_json::json!(from));
    }
    let input = SettlementInput {
        source_window: bill.source_window.clone(),
        dimensions: usage_dimensions(bill, info),
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
        list_price: quote.list_price,
        upstream_cost: None,
        pricing_epoch: Some(bill.book.epoch()),
        pricing_snapshot: snapshot,
        latency_ms: elapsed_ms_i32(bill.started),
        ttft_ms,
        is_stream: bill.is_stream,
        retry_count: info.retry,
        failover_count: failover,
        upstream_status: Some(info.outcome.as_ref().map_or(200, |v| v.0)),
        error_code: info.outcome.as_ref().map(|v| v.1.as_str()),
        upstream_request_id: info.upstream_request_id.as_deref(),
        node: bill.state.node.as_ref(),
        sticky_layer: info.sticky_layer,
        client_type: bill.client_type,
        client_ip: bill.client_ip.as_deref(),
        delta_micro: quote.amount.as_micros().saturating_neg(),
        balance_after: None,
        event_type: "commit",
        pool: bill.reservation_pool,
    };
    if !bill.state.settle_success(input).await? {
        return Ok(());
    }
    super::auth::record_settlement_counters(
        &bill.state,
        bill.user_id,
        bill.member_user_id,
        quote.amount.as_micros(),
        usage.total_raw(),
    )
    .await;
    // 选路反馈：时延 EWMA 供 least_latency 池排序，key 日消费供上限闸。
    // 放在结算之后 = 不占热路径，且只有成功请求才计入时延样本。
    super::auth::record_channel_key_feedback(
        &bill.state,
        info.key,
        ttft_ms.unwrap_or_else(|| elapsed_ms_i32(bill.started)),
        quote.amount.as_micros(),
    )
    .await;
    Ok(())
}

async fn settle_failure(bill: &RequestBilling, failure: &ForwardFailure) {
    let pool = match bill
        .state
        .ledger
        .refund(bill.user_id, bill.key_id, bill.request_id)
        .await
    {
        Ok(r) if !r.released.is_zero() => r.pool,
        Ok(_) => bill.reservation_pool,
        Err(err) => {
            tracing::error!(request_id = %bill.request_id, error = %err, "退款失败（预扣悬置，待对账清理）");
            bill.reservation_pool
        }
    };
    let (channel, key) = failure.channel.unwrap_or((0, 0));
    record_terminal(
        bill,
        &CandInfo {
            outcome: None,
            channel,
            cost_milli: 0,
            key,
            sticky_layer: 0,
            retry: 0,
            upstream_request_id: None,
            bill_resp_model: false,
            // 失败路径不结算 usage，复核开关取不影响结果的一侧
            trust_usage: true,
            upstream_model: failure
                .upstream
                .as_ref()
                .map_or_else(String::new, |v| v.0.clone()),
            upstream_endpoint: failure
                .upstream
                .as_ref()
                .map_or_else(String::new, |v| v.1.clone()),
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
        pool,
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
    pool: Pool,
) {
    let input = SettlementInput {
        source_window: bill.source_window.clone(),
        dimensions: usage_dimensions(bill, info),
        request_id: bill.request_id,
        log_type,
        user_id: bill.user_id,
        api_key_id: bill.key_id,
        group_code: &bill.group,
        // 与成功记账同口径：记实际计价所用的名字
        model_name: &bill.calc.model.to_string(),
        channel_id: (info.channel != 0).then_some(info.channel),
        channel_key_id: (info.key != 0).then_some(info.key),
        state,
        usage,
        amount,
        original: Money::ZERO,
        discount: Money::ZERO,
        list_price: Money::ZERO,
        upstream_cost: None,
        pricing_epoch: Some(bill.book.epoch()),
        pricing_snapshot: Some(serde_json::json!({
            "epoch": bill.book.epoch(),
            "reservation": bill.reservation_snapshot.as_ref(),
        })),
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
        pool,
    };
    if state == BillingState::Committed {
        if let Err(error) = bill.state.settle_success(input).await {
            tracing::error!(request_id=%bill.request_id, ?error, "terminal usage persistence failed");
        }
    } else {
        bill.state.settle_write(input).await;
    }
}

fn usage_dimensions(bill: &RequestBilling, info: &CandInfo) -> okapi_ledger::pg::UsageDimensions {
    let endpoint = match bill.ingress {
        Ingress::OpenAi => "/v1/chat/completions",
        Ingress::Anthropic => "/v1/messages",
        Ingress::Responses => "/v1/responses",
        Ingress::ResponsesCompact => "/v1/responses/compact",
        Ingress::Gemini if bill.is_stream => "/v1beta/models/{model}:streamGenerateContent",
        Ingress::Gemini => "/v1beta/models/{model}:generateContent",
    };
    okapi_ledger::pg::UsageDimensions::new(
        &bill.requested_model,
        &info.upstream_model,
        endpoint,
        &info.upstream_endpoint,
    )
    .with_diagnostics(Some(bill.trace.snapshot()))
}

// ---- 响应工具 ----

fn upstream_passthrough_response(
    ingress: Ingress,
    status: u16,
    body: Bytes,
    request_id: Uuid,
) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    // Anthropic / Gemini 入口：上游若非本协议错误壳（如 OpenAI 渠道 400），转译为协议壳
    let body = match ingress {
        Ingress::Anthropic if !body_is_anthropic_error(&body) => {
            let message = String::from_utf8_lossy(&body);
            Bytes::from(
                serde_json::json!({
                    "type": "error",
                    "error": {"type": "upstream_error", "message": message},
                })
                .to_string(),
            )
        }
        Ingress::Gemini if !body_is_gemini_error(&body) => {
            let message = String::from_utf8_lossy(&body);
            Bytes::from(
                serde_json::json!({
                    "error": {
                        "code": status.as_u16(),
                        "message": message,
                        "status": super::error::gemini_status_name(status),
                    },
                })
                .to_string(),
            )
        }
        Ingress::OpenAi
        | Ingress::Responses
        | Ingress::ResponsesCompact
        | Ingress::Anthropic
        | Ingress::Gemini => body,
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
        .is_ok_and(|v| v.get("type").and_then(|t| t.as_str()) == Some("error"))
}

/// google.rpc.Status 壳：`error.code` 为数字且带 `error.status`。
fn body_is_gemini_error(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").cloned())
        .is_some_and(|e| {
            e.get("code").is_some_and(serde_json::Value::is_number) && e.get("status").is_some()
        })
}

fn elapsed_ms_i32(started: Instant) -> i32 {
    i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod failure_scope_tests {
    use super::*;

    #[test]
    fn visible_reasoning_is_counted_once_and_opaque_context_is_not() {
        let chat = Bytes::from(
            serde_json::json!({"choices":[{"message":{
                "content":"OK", "reasoning_content":"思考", "reasoning":"ignored alias"
            }}]})
            .to_string(),
        );
        assert_eq!(non_stream_content_chars(Ingress::OpenAi, &chat), 4);
        let responses = Bytes::from(serde_json::json!({"output":[
            {"type":"reasoning","summary":[{"type":"summary_text","text":"思考"}],"encrypted_content":"opaque"},
            {"type":"message","content":[{"type":"output_text","text":"OK"}]}
        ]}).to_string());
        assert_eq!(non_stream_content_chars(Ingress::Responses, &responses), 4);
        assert_eq!(
            non_stream_content_chars(Ingress::ResponsesCompact, &responses),
            0
        );
    }

    #[test]
    fn tool_arguments_and_every_choice_feed_the_fallback_estimate() {
        let body = |value: serde_json::Value| Bytes::from(value.to_string());
        let chat = body(serde_json::json!({"choices":[
            {"message":{"content":"ab","tool_calls":[{"function":{"name":"f","arguments":"{\"x\":1}"}}]}},
            {"message":{"content":"cd"}}
        ]}));
        assert_eq!(non_stream_content_chars(Ingress::OpenAi, &chat), 2 + 7 + 2);
        let anthropic = body(serde_json::json!({"content":[
            {"type":"text","text":"ab"},
            {"type":"tool_use","name":"f","input":{"x":1}}
        ]}));
        assert_eq!(
            non_stream_content_chars(Ingress::Anthropic, &anthropic),
            2 + 7
        );
        let responses = body(serde_json::json!({"output":[
            {"type":"function_call","name":"f","arguments":"{\"x\":1}"},
            {"type":"custom_tool_call","name":"g","input":"patch"}
        ]}));
        assert_eq!(
            non_stream_content_chars(Ingress::Responses, &responses),
            7 + 5
        );
        let gemini = body(serde_json::json!({"candidates":[
            {"content":{"parts":[{"text":"ab"},{"functionCall":{"name":"f","args":{"x":1}}}]}},
            {"content":{"parts":[{"text":"cd"}]}}
        ]}));
        assert_eq!(
            non_stream_content_chars(Ingress::Gemini, &gemini),
            2 + 7 + 2
        );
    }

    #[test]
    fn explicit_caps_are_reserved_in_full_and_defaults_stay_bounded() {
        assert_eq!(admitted_completion_cap(Some(100_000), None), 100_000);
        assert_eq!(admitted_completion_cap(Some(100_000), Some(40_000)), 40_000);
        assert_eq!(admitted_completion_cap(Some(512), Some(40_000)), 512);
        assert_eq!(
            admitted_completion_cap(None, Some(128_000)),
            MAX_COMPLETION_CAP
        );
        assert_eq!(admitted_completion_cap(None, Some(8_192)), 8_192);
        assert_eq!(admitted_completion_cap(None, None), DEFAULT_COMPLETION_CAP);
    }

    /// 四种入口的图片都计张，工具结果里嵌套的也算；非图片的内联文件不算。
    #[test]
    fn image_inputs_are_counted_across_ingresses() {
        let count =
            |ingress, v: serde_json::Value| image_inputs(ingress, &Bytes::from(v.to_string()));
        assert_eq!(
            count(
                Ingress::OpenAi,
                serde_json::json!({"messages":[{"role":"user","content":[
                    {"type":"text","text":"hi"},
                    {"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}},
                    {"type":"image_url","image_url":{"url":"https://x/y.png"}}
                ]}]})
            ),
            2
        );
        assert_eq!(
            count(
                Ingress::Anthropic,
                serde_json::json!({"messages":[{"role":"user","content":[
                    {"type":"image","source":{"type":"base64","data":"AA"}},
                    {"type":"tool_result","tool_use_id":"t","content":[
                        {"type":"image","source":{"type":"url","url":"https://x"}}
                    ]}
                ]}]})
            ),
            2
        );
        assert_eq!(
            count(
                Ingress::Responses,
                serde_json::json!({"input":[{"role":"user","content":[
                    {"type":"input_text","text":"hi"},
                    {"type":"input_image","image_url":"https://x"}
                ]}]})
            ),
            1
        );
        assert_eq!(
            count(
                Ingress::Gemini,
                serde_json::json!({"contents":[{"parts":[
                    {"inlineData":{"mimeType":"image/png","data":"AA"}},
                    {"inlineData":{"mimeType":"application/pdf","data":"AA"}},
                    {"text":"hi"}
                ]}]})
            ),
            1
        );
        assert_eq!(
            count(Ingress::OpenAi, serde_json::json!({"messages":[]})),
            0
        );
    }

    /// PDF 按页、音频按字节、纯文本文档按长度计入；URL 引用与未知类型不计。
    #[test]
    fn attachments_are_bounded_from_inline_data() {
        use base64::Engine as _;
        let b64 = |bytes: &[u8]| base64::prelude::BASE64_STANDARD.encode(bytes);
        let pdf = b64(
            b"%PDF-1.4 1 0 obj <</Type /Pages /Count 2>> 2 0 obj <</Type /Page>> 3 0 obj <</Type/Page>>",
        );
        let count =
            |ingress, v: serde_json::Value| attachment_tokens(ingress, &Bytes::from(v.to_string()));
        assert_eq!(
            count(
                Ingress::Anthropic,
                serde_json::json!({"messages":[{"role":"user","content":[
                    {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":pdf}},
                    {"type":"document","source":{"type":"text","media_type":"text/plain","data":"abcdef"}},
                    {"type":"document","source":{"type":"url","url":"https://x/a.pdf"}}
                ]}]})
            ),
            2 * PDF_PAGE_TOKENS + 3
        );
        assert_eq!(
            count(
                Ingress::OpenAi,
                serde_json::json!({"messages":[{"role":"user","content":[
                    {"type":"file","file":{"filename":"a.pdf","file_data":format!("data:application/pdf;base64,{pdf}")}},
                    {"type":"input_audio","input_audio":{"format":"wav","data":b64(&[0_u8; 12_500])}}
                ]}]})
            ),
            2 * PDF_PAGE_TOKENS + 100
        );
        assert_eq!(
            count(
                Ingress::Responses,
                serde_json::json!({"input":[{"role":"user","content":[
                    {"type":"input_file","file_data":format!("data:application/pdf;base64,{pdf}")},
                    {"type":"input_file","file_id":"file_1"}
                ]}]})
            ),
            2 * PDF_PAGE_TOKENS
        );
        assert_eq!(
            count(
                Ingress::Gemini,
                serde_json::json!({"contents":[{"parts":[
                    {"inlineData":{"mimeType":"audio/mp3","data":b64(&[0_u8; 1_250])}},
                    {"inlineData":{"mimeType":"image/png","data":"AA"}}
                ]}]})
            ),
            10
        );
    }

    /// 放大后的估算必须仍是合法用量：计价入口先校验，不合法会让整个请求失败。
    #[test]
    fn admission_hints_keep_the_estimate_valid_for_pricing() {
        let base = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 64,
            ..TokenUsage::default()
        };
        let hinted = with_admission_hints(
            base,
            okapi_providers::profiles::admission_hints(
                &serde_json::json!({"client_profile":{"name":"claude-code","mode":"mimic"}}),
            ),
        );
        hinted.validate().unwrap();
        assert_eq!(hinted.prompt_tokens, 1728);
        assert_eq!(hinted.cache_write_1h_tokens, Some(1728));
        let plain =
            with_admission_hints(base, okapi_providers::profiles::AdmissionHints::default());
        plain.validate().unwrap();
        assert_eq!(plain, base);
    }

    /// 缺省上限只在模型能输出得比预扣封顶更多时写入；显式上限、未知 max_output 不动。
    #[test]
    fn omitted_cap_is_bounded_only_when_the_model_could_exceed_the_hold() {
        assert_eq!(
            default_output_cap(None, Some(128_000)),
            Some(MAX_COMPLETION_CAP)
        );
        assert_eq!(default_output_cap(None, Some(8_192)), None);
        assert_eq!(default_output_cap(None, None), None);
        assert_eq!(default_output_cap(Some(100), Some(128_000)), None);

        let cap = Some(MAX_COMPLETION_CAP);
        let json = |ingress, v: serde_json::Value| -> serde_json::Value {
            let out = bound_default_output(ingress, cap, Bytes::from(v.to_string()));
            serde_json::from_slice(&out).unwrap()
        };
        let chat = json(
            Ingress::OpenAi,
            serde_json::json!({"model":"m","messages":[]}),
        );
        assert_eq!(chat["max_tokens"], MAX_COMPLETION_CAP);
        let explicit = json(
            Ingress::OpenAi,
            serde_json::json!({"model":"m","max_completion_tokens":7}),
        );
        assert_eq!(explicit["max_completion_tokens"], 7);
        assert!(explicit.get("max_tokens").is_none());
        let responses = json(Ingress::Responses, serde_json::json!({"model":"m"}));
        assert_eq!(responses["max_output_tokens"], MAX_COMPLETION_CAP);
        let gemini = json(
            Ingress::Gemini,
            serde_json::json!({"contents":[],"generationConfig":{"temperature":0.2}}),
        );
        assert_eq!(
            gemini["generationConfig"]["maxOutputTokens"],
            MAX_COMPLETION_CAP
        );
        assert_eq!(gemini["generationConfig"]["temperature"], 0.2);
        let anthropic = json(Ingress::Anthropic, serde_json::json!({"model":"m"}));
        assert_eq!(anthropic["max_tokens"], MAX_COMPLETION_CAP);
        let compact = json(Ingress::ResponsesCompact, serde_json::json!({"model":"m"}));
        assert!(compact.get("max_output_tokens").is_none());
        let untouched = Bytes::from_static(b"{\"model\":\"m\"}");
        assert_eq!(
            bound_default_output(Ingress::OpenAi, None, untouched.clone()),
            untouched
        );
    }

    #[test]
    fn responses_error_sequence_follows_forwarded_events() {
        let event = |sequence| ChatEvent::Data {
            raw:
                serde_json::json!({"type":"response.output_text.delta","sequence_number":sequence})
                    .to_string(),
            event: Some("response.output_text.delta".into()),
            has_output: true,
            content_chars: 1,
            usage: None,
        };
        let mut next = 0;
        advance_stream_sequence(&event(7), Ingress::Responses, &mut next);
        assert_eq!(next, 8);
        advance_stream_sequence(&event(4), Ingress::Responses, &mut next);
        assert_eq!(next, 8);
        advance_stream_sequence(&event(9), Ingress::OpenAi, &mut next);
        assert_eq!(next, 8);
        advance_stream_sequence(&ChatEvent::Done, Ingress::Responses, &mut next);
        assert_eq!(next, 8);
    }

    fn status(status: u16, body: &str) -> UpstreamError {
        UpstreamError::Status {
            status,
            body: Bytes::copy_from_slice(body.as_bytes()),
            retry_after_secs: None,
        }
    }
    #[test]
    fn request_errors_and_resource_permissions_do_not_change_account_health() {
        for error in [
            UpstreamError::Connect("dns".into()),
            UpstreamError::Timeout,
            UpstreamError::Stream("transport".into()),
            status(403, r#"{"error":{"code":"access_denied"}}"#),
            status(408, ""),
        ] {
            assert!(matches!(failure_kind_of(&error), KeyFailure::Request));
        }
        assert!(matches!(
            failure_kind_of(&status(401, "")),
            KeyFailure::Invalid
        ));
        assert!(matches!(
            failure_kind_of(&status(403, r#"{"error":{"code":"invalid_api_key"}}"#)),
            KeyFailure::Invalid
        ));
        assert!(matches!(
            failure_kind_of(&status(429, r#"{"error":{"code":"insufficient_quota"}}"#)),
            KeyFailure::QuotaExhausted
        ));
        assert!(matches!(
            failure_kind_of(&status(529, "")),
            KeyFailure::RateLimited {
                retry_after_secs: Some(600)
            }
        ));
        assert!(matches!(
            failure_kind_of(&status(503, "")),
            KeyFailure::Transient
        ));
    }
}
