//! 选路：可用候选的筛选与排序、恢复时间估计、playground 参数。
use super::super::execution_plan::{ExecutionPlan, Requirements};
use super::super::ingress::Ingress;
use super::super::scheduler::Strategy;
use super::super::state::AppState;
use super::{
    ForwardFailure, ProbeInfo, RequestBilling, cached_candidates, lost_tools,
    resolve_with_directive,
};
use crate::kernel::error::AppError;
use crate::kernel::sched_redis::response_affinity;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_domain::{GroupCode, ModelCode, TokenUsage};
use okapi_providers::convert::tool_loss::ToolLossPolicy;
use std::sync::Arc;

// Responses WS 逐轮从数据库读取候选，复用 HTTP 的权限/能力/历史绑定过滤。
#[allow(clippy::too_many_lines)]
pub(super) async fn eligible_candidates(
    bill: &RequestBilling,
    info: &ProbeInfo,
    fresh: bool,
) -> Result<(super::super::scheduler::CandidateQueue, Option<i64>), ForwardFailure> {
    // 候选 5s 进程缓存（热路径零 PG 读；console 写路径主动失效，多副本靠 TTL 收敛）
    // 缓存键含池链：不同池的候选集合不同，混用会把别的池的渠道发给用户
    use super::super::scheduler::{CandidateQueue, CandidateSet};
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
        // soonest_reset 的先后要读额度观测，在下面的准入筛选里排；这里的加权随机只管同一时刻的并列
        Strategy::PriorityWeighted | Strategy::SoonestReset => {
            CandidateQueue::weighted(Arc::clone(&raw))
        }
        Strategy::LeastLatency => {
            let latency = bill
                .state
                .sched
                .channel_key_latencies(raw.iter().map(|c| c.channel_key_id))
                .await;
            CandidateQueue::by_latency(Arc::clone(&raw), &latency)
        }
        Strategy::LeastUsage => {
            let spend = bill
                .state
                .sched
                .channel_key_spends(raw.iter().map(|c| c.channel_key_id))
                .await;
            CandidateQueue::by_load(Arc::clone(&raw), &spend)
        }
        Strategy::LeastConcurrency => {
            let in_flight = bill
                .state
                .sched
                .channel_key_in_flight(raw.iter().map(|c| c.channel_key_id))
                .await;
            CandidateQueue::by_load(Arc::clone(&raw), &in_flight).counting_in_flight()
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
    // 仅限真机的渠道（client_profile.native_only）：其它客户端跳过它们，
    // 由同池其它渠道与降级池承接；被这一条筛空单独给错误码，不冒充渠道全挂。
    if let Some(pin) = &bill.pin {
        candidates.retain(|c| pin.matches(c));
        if candidates.is_empty() {
            return Err(ForwardFailure::app(
                AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::NO_AVAILABLE_CHANNEL)
                    .with_param("channel_test"),
                0,
                None,
            ));
        }
    }
    // 账号测试是管理员点名测这条渠道，不受「仅限真机」约束
    if !bill.claude_code_native && bill.pin.is_none() {
        let before = candidates.len();
        candidates.retain(|c| !okapi_providers::profiles::native_only(&c.extensions));
        if before > 0 && candidates.is_empty() {
            return Err(ForwardFailure::app(
                AppError::new(StatusCode::FORBIDDEN, codes::CLAUDE_CODE_CLIENT_REQUIRED),
                0,
                None,
            ));
        }
    }
    // 只接官方 Codex 客户端的渠道（extensions.codex_client）：同上，第三方客户端与版本不在窗口里的
    // 跳过它们；被这一条筛空时说清是不是官方客户端、版本差在哪。
    if bill.pin.is_none() {
        let before = candidates.len();
        let mut refusal = None;
        candidates.retain(|c| {
            okapi_providers::profiles::codex_client_gate(&c.extensions).is_none_or(|gate| {
                gate.admits(bill.codex_client.as_ref())
                    .map_err(|reason| refusal.get_or_insert(reason).clone())
                    .is_ok()
            })
        });
        if before > 0
            && candidates.is_empty()
            && let Some(refusal) = refusal
        {
            return Err(ForwardFailure::app(
                AppError::new(StatusCode::FORBIDDEN, codes::CODEX_CLIENT_REQUIRED)
                    .with_param(refusal.param()),
                0,
                None,
            ));
        }
    }
    let requirements = Requirements {
        tools: info.needs_tools,
        vision: info.needs_vision,
        server_tools: bill.server_tools.has_tools(),
    };
    candidates
        .retain(|c| ExecutionPlan::compile(bill.ingress, c, &bill.model, requirements).is_ok());
    // 转成别家方言会丢工具：渠道按 tool_loss_policy 决定还能不能走。全被挡下时说清是哪些工具
    // 过不去，而不是笼统的「没有可用渠道」——静默丢掉客户端工具，agent 就在看不见工具的情况下作答。
    let lost = lost_tools(info, bill);
    if !lost.is_empty() {
        let before = candidates.len();
        candidates.retain(|c| {
            ExecutionPlan::compile(bill.ingress, c, &bill.model, requirements)
                .is_ok_and(|plan| !plan.converts(bill.ingress))
                || ToolLossPolicy::parse(c.tool_loss_policy.as_deref()).admits(&lost)
        });
        if before > 0 && candidates.is_empty() {
            let blocking = if lost.client.is_empty() {
                lost.all()
            } else {
                lost.client.clone()
            };
            return Err(ForwardFailure::app(
                AppError::new(StatusCode::BAD_REQUEST, codes::TOOL_CONVERSION_UNSUPPORTED)
                    .with_param(blocking.join(",")),
                0,
                None,
            ));
        }
    }
    // 模型没配工具价格的搜索/抓取只走订阅：API key 渠道上游按次收费，不能白送
    if bill.server_tools.subscription_only() {
        candidates.retain(|c| super::super::oauth_cred::is_oauth_provider(&c.provider));
        if candidates.is_empty() {
            return Err(ForwardFailure::app(
                AppError::new(StatusCode::BAD_REQUEST, codes::SERVER_TOOL_UNPRICED)
                    .with_param("tools"),
                0,
                None,
            ));
        }
    }
    // 负毛利熔断（§11.34）：该分组打这条渠道在亏钱，摘掉让同池其它渠道承接
    let quota_order = match Strategy::parse(bill.pool_strategy.as_deref()) {
        Strategy::PriorityWeighted => super::super::account_control::QuotaOrder::HeadroomTiers,
        Strategy::SoonestReset => super::super::account_control::QuotaOrder::SoonestReset,
        Strategy::LeastLatency | Strategy::LeastUsage | Strategy::LeastConcurrency => {
            super::super::account_control::QuotaOrder::Keep
        }
    };
    // L2 会话粘性映射：额度准入要先知道缓存还热不热（热会话可以用到保留线，冷请求按整段 prompt
    // 估增量），所以读取放在准入之前；把它提到候选首位仍在准入之后——被额度挡下的账号，
    // 亲和不越过准入。
    let sticky_hit = match bill.session.as_ref().filter(|_| bill.pin.is_none()) {
        Some(session) => bill.state.sched.sticky_get(bill.user_id, session).await,
        None => None,
    };
    let warm_key = sticky_hit
        .filter(|hit| {
            hit.is_warm(
                chrono::Utc::now().timestamp(),
                super::super::account_control::forecast::WARM_WINDOW_SECS,
            )
        })
        .map(|hit| hit.key);
    // 额度预判只对有额度观测的订阅渠道有意义；候选里没有就不必估价
    let shape = candidates
        .iter()
        .any(|c| super::super::account_control::has_quota_hook(&c.provider))
        .then(|| forecast_shape(bill, warm_key))
        .flatten();
    let retained = bill
        .state
        .retain_margin_ok_ordered(&bill.group, &mut candidates, quota_order, shape.as_ref())
        .await;
    if candidates.is_empty() {
        let mut err = AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            super::super::state::no_candidates_code(retained.margin_removed),
        );
        if retained.margin_removed == 0
            && let Some(secs) = recovery_secs(bill, retained.next_quota_reset).await
        {
            err = err.with_retry_after(secs);
        }
        return Err(ForwardFailure::app(err, 0, None));
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
    let sticky_key = sticky_hit.map(|hit| hit.key);
    let position =
        sticky_key.and_then(|kid| candidates.iter().position(|c| c.channel_key_id == kid));
    if let Some(position) = position {
        let hit = candidates.remove(position);
        candidates.insert(0, hit);
    }

    Ok((candidates.with_warm_key(warm_key), sticky_key))
}

/// 额度预判用的请求形状：冷启动与热请求各按价簿折一次 API 等价美元，与结算同一个计价器。
/// 价簿算不出来（模型没配价）就不预判。
#[allow(clippy::cast_precision_loss)] // micro-USD 远小于 2^53
pub(super) fn forecast_shape(
    bill: &RequestBilling,
    warm_key: Option<i64>,
) -> Option<super::super::account_control::forecast::RequestShape> {
    /// 一次调用的输出 token 均值（线上两周实测约 0.7k）。
    const EXPECTED_COMPLETION: u32 = 700;
    /// 热请求每轮新增、需要写进缓存的内容（工具结果、新问题）。
    const WARM_NEW_TOKENS: u32 = 3_000;
    /// 预扣用的是 tiktoken 口径，Claude 的真实计数偏高；冷启动是额度消耗的大头，宁可高估。
    const PROMPT_MARGIN_PERCENT: u32 = 110;
    let prompt = bill
        .est_prompt
        .saturating_mul(PROMPT_MARGIN_PERCENT)
        .saturating_div(100);
    let usage = |read: u32, written: u32| TokenUsage {
        prompt_tokens: prompt,
        cached_tokens: read,
        cache_read_reported: true,
        cache_write_reported: true,
        cache_write_tokens: written,
        cache_write_5m_tokens: Some(written),
        cache_write_1h_tokens: Some(0),
        completion_tokens: EXPECTED_COMPLETION,
        ..TokenUsage::default()
    };
    let price = |usage: TokenUsage| {
        okapi_pricing::calculate(&bill.book, &bill.calc, usage)
            .ok()
            .map(|quote| quote.list_price.as_micros() as f64 / 1_000_000.0)
    };
    let fresh = WARM_NEW_TOKENS.min(prompt);
    Some(super::super::account_control::forecast::RequestShape {
        prompt_tokens: bill.est_prompt,
        cold_usd: price(usage(0, prompt))?,
        warm_usd: price(usage(prompt - fresh, fresh))?,
        warm_key,
    })
}

/// 候选全空时最早多少秒后可能有账号回来：被上游额度挡下的账号窗口重置、池里冷却中的 key 到期，
/// 取最早的那个。两样都不知道时为 None——不编一个数给客户端。
pub(super) async fn recovery_secs(
    bill: &RequestBilling,
    next_quota_reset: Option<i64>,
) -> Option<u64> {
    let chain: Vec<&str> = bill.pool_chain.iter().map(String::as_str).collect();
    let cooldown = okapi_store::channels::next_key_recovery(&bill.state.pg, &bill.model, &chain)
        .await
        .inspect_err(|err| tracing::warn!(error = %err, "查询 key 冷却到期时刻失败"))
        .ok()
        .flatten()
        .map(|at| at.timestamp());
    let at = next_quota_reset.into_iter().chain(cooldown).min()?;
    let secs = at.saturating_sub(chrono::Utc::now().timestamp());
    Some(u64::try_from(secs).unwrap_or(0))
}

/// Read-only playground contract: same identity, canonical model, group and
/// routing pool as a real request; no reservation, rate counter or upstream call.
pub(crate) async fn playground_parameters(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
) -> Result<okapi_providers::model_parameters::ParameterProfile, AppError> {
    use okapi_providers::model_parameters::{self, ParameterProfile};
    let key = super::super::auth::authenticate(state, headers).await?;
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
        let dialect = super::super::dialect::upstream_dialect(&c.provider, upstream);
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
