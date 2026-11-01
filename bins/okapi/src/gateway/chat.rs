//! /v1/chat/completions 与 /v1/messages：鉴权 → 估价预扣 → 渠道 failover 转发
//! （首字前缓冲）→ 结算。时序对齐 IMPLEMENTATION §2.2；SSE 语义对齐 §3.7。
//! 入口协议 × 渠道协议 四象限（§4.4）：转换在 providers::convert，泵送与结算无感。
//!
//! 本文件放各阶段共用的请求 / 计费 / 失败类型与失败分类；阶段按请求走过的顺序分在子模块：
//! entry（HTTP 入口）→ input（校验与特征）→ prepare（模型解析、估价预扣）→ routing（候选）
//! → attempt（failover 主循环）→ upstream（上游请求体与分派）→ stream / pump（流式首字前 /
//! 首字后）或 json（非流式）→ settle（结算）；另有 count（count_tokens）与 limits（上限）。
//! 子模块的条目都从这里原路径再导出，媒体端点、WebSocket 与控制面的引用不变。

use super::error::AppError;
use super::error::with_request_id;
use super::estimate::estimate_prompt_tokens;
use super::sched_redis::response_affinity::{ResponseBinding, ResponseParent, ResponseWriter};
use super::sched_redis::session_hash;
use super::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use bytes::Bytes;
use okapi_api::{ResponsesRequestProbe, UsageProbe, codes};
use okapi_domain::{BillingState, Money, TokenUsage};
use okapi_ledger::Pool;
use okapi_pricing::{CalcContext, PriceBook};
use okapi_providers::account_fault::AccountFault;
use okapi_providers::convert::tool_loss::ForeignTools;
use okapi_providers::reasoning::ReasoningDirective;
use okapi_providers::{ChatEvent, StreamHandle, UpstreamError};
use okapi_store::ChannelCandidate;
use okapi_store::channels::KeyFailure;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

mod attempt;
mod count;
mod entry;
mod input;
mod json;
mod limits;
mod prepare;
mod pump;
mod routing;
mod settle;
mod stream;
mod upstream;
pub mod websocket;

use self::attempt::{classify_fatal, try_model};
pub use self::count::messages_count_tokens;
use self::count::user_agent;
pub use self::entry::{
    GeminiQuery, chat_completions, gemini_generate, messages, responses, responses_compact,
};
#[cfg(test)]
use self::input::PDF_MAX_PAGES;
#[cfg(test)]
use self::input::PDF_PAGE_TOKENS;
#[cfg(test)]
use self::input::media_tokens;
use self::input::{IMAGE_INPUT_TOKENS, attachment_tokens, image_inputs, lost_tools, parse_checked};
pub(super) use self::input::{check_input, request_features};
#[cfg(test)]
use self::json::non_stream_content_chars;
use self::json::{RespMeta, attempt_json, capture_chunk_meta, upstream_passthrough_response};
pub(super) use self::json::{non_json_failure, non_json_kind};
use self::limits::{
    admitted_completion_cap, bound_default_output, default_output_cap, price_above_max,
};
#[cfg(test)]
use self::prepare::reserved_token_count;
#[cfg(test)]
use self::prepare::with_admission_hints;
pub(crate) use self::prepare::{cached_candidates, resolve_model_cached};
use self::prepare::{handle_chat, prepare_chat, resolve_with_directive};
#[cfg(test)]
use self::pump::advance_stream_sequence;
use self::pump::{
    CandInfo, cand_info, first_output_window, settle_stream, spawn_stream_pump, upstream_endpoint,
};
pub(crate) use self::routing::playground_parameters;
use self::routing::{eligible_candidates, recovery_secs};
use self::settle::{record_terminal, settle_commit, settle_failure};
use self::stream::{attempt_stream, capture_response_event, with_relayed};
use self::upstream::{build_upstream_body, dispatch_chat, response_writer, shape_upstream_body};

const DEFAULT_ANTHROPIC_BASE: &str = "https://api.anthropic.com/v1";
/// 首字窗口（连接 + 首个产出事件）；窗口内失败可无痕 failover。
/// 缺省值——渠道可用 `retry_policy.first_output_timeout_secs` 覆盖（5..=300）。
const FIRST_OUTPUT_TIMEOUT: Duration = Duration::from_secs(30);
/// 上游已回过事件后的首字空闲窗口（见 `attempt_stream`）。
const STARTED_IDLE_WINDOW: Duration = Duration::from_mins(5);
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
    /// 转成别家方言会丢的工具（渠道按 `tool_loss_policy` 拒绝或丢弃）。
    foreign_tools: ForeignTools,
    /// OpenAI service_tier 请求声明（tier 计费轴；chat 与 responses 入口有，anthropic 无此概念）。
    service_tier: Option<String>,
}

/// 请求体顶层有没有统一 `reasoning` 对象（入口解析时看一眼）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnifiedReasoning {
    /// 没有：转发前什么也摘不到，别为确认这句「没有」再解析整份请求体。
    Absent,
    /// 有，或没法确认：转发前照旧走摘除。
    Present,
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
    /// 转发前要不要摘统一 `reasoning` 对象（每次尝试不必再读原文确认「没有」）。
    unified_reasoning: UnifiedReasoning,
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
    /// 控制台账号测试钉住的渠道 / 账号（数据面正常请求为 None）。
    pin: Option<Arc<super::channel_test::ChannelPin>>,
    /// 请求来自真实 Claude Code 客户端（只看 `/v1/messages`）：仅限真机的渠道只收这类请求。
    claude_code_native: bool,
    /// 请求出自哪个 Codex 官方客户端（按请求头认，见 `profiles::codex_client`）；
    /// 只接官方客户端的 Codex 渠道（`extensions.codex_client`）据此放行。
    codex_client: Option<okapi_providers::profiles::codex_client::CodexClient>,
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
        // 状态码看不出来、正文明说账号不能用了（余额耗尽 / 组织停用 / key 无效，常被包成 400 或
        // 不带关键词的 403）：这把 key 要按账号级故障登记，否则它会一直被选中、一直失败
        UpstreamError::Status { .. } => match err.account_fault() {
            Some(AccountFault::QuotaExhausted) => KeyFailure::QuotaExhausted,
            Some(AccountFault::Invalid) => KeyFailure::Invalid,
            None => KeyFailure::Request,
        },
        UpstreamError::Connect(_)
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

fn elapsed_ms_i32(started: Instant) -> i32 {
    i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod failure_scope_tests;
