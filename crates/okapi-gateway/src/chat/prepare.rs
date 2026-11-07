//! 准入：模型解析（含变体与推理指令）、估价与预扣、兜底模型冻结。
use super::super::estimate;
use super::super::ingress::Ingress;
use super::super::state::AppState;
use super::{
    AdmittedFallback, CancelRefund, FailureReply, ForwardFailure, IMAGE_INPUT_TOKENS, ProbeInfo,
    RequestBilling, SettlementHandoff, UnifiedReasoning, admitted_completion_cap,
    attachment_tokens, cached_candidates, default_output_cap, image_inputs, price_above_max,
    resolve_model_cached, settle_failure, try_model, upstream_passthrough_response, user_agent,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use okapi_api::codes;
use okapi_domain::{ModelCode, TokenUsage};
use okapi_kernel::clients::detect_client_type;
use okapi_kernel::error::AppError;
use okapi_kernel::sched_redis::response_affinity;
use okapi_providers::reasoning::{self, ReasoningDirective};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

/// 模型解析 + 修饰符（§4.4 / §11.25）：全名（含别名）直命中优先，未命中才按修饰符
/// 语法剥基名重试。`@key:value` 与旧的 `-high/-thinking[-N]` 两种写法都认，且**归一到
/// 同一个规范计费名**。
///
/// 返回的第三项是**规范计费名**（如 `gpt-5@effort:high`）：它只用于定价与记账，
/// 路由仍走基座 canonical——渠道声明的是基座模型名，拿变体名去选渠道会一个候选都选不到。
pub(super) async fn resolve_with_directive(
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

pub(super) async fn handle_chat(
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
pub(super) async fn prepare_chat(
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
        None => super::super::auth::authenticate_data_plane(state, headers).await?,
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
    let rules_in = super::super::rule_inputs::collect(state, &book, key.user_id).await;
    let now = chrono::Utc::now();
    // 计价名（§11.25）：变体在价簿里配了价就按变体收，否则回退基座——修饰符改的是
    // 上游行为，站长愿不愿意为它单独定价是另一回事，没配价不该让请求失败。
    // 路由仍用 `canonical`：渠道声明的是基座模型名。
    let billing_model = variant
        .filter(|v| book.has_model(&ModelCode::from(v.as_str())))
        .unwrap_or_else(|| canonical.clone());
    // 预扣按请求声明档估（贵档多预扣；结算档只降不升另选）
    let calc = super::super::admission::calc_context(
        &key,
        billing_model.as_str(),
        rules_in,
        now,
        info.service_tier.clone(),
    );

    // 估价（预扣补全缺省 = models.max_output，无则 2048，§5.1）
    let est_prompt = info.prompt_tokens;
    let density = estimate::prompt_density(est_prompt, info.prompt_chars);
    // 请求体在这里只解析一次：图片 / 附件估算与原生工具准入共用（大请求体每多解析一遍都是毫秒级 CPU）
    let parsed: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    // 图片输入另计（密度只描述文本，先算完再加）
    let est_prompt = parsed.as_ref().map_or(est_prompt, |v| {
        est_prompt
            .saturating_add(image_inputs(ingress, v).saturating_mul(IMAGE_INPUT_TOKENS))
            .saturating_add(attachment_tokens(ingress, v))
    });
    let completion_cap = admitted_completion_cap(
        ingress,
        info.completion_cap_req,
        meta.max_output.and_then(|v| u32::try_from(v).ok()),
    );
    let mut server_tools = parsed
        .as_ref()
        .ok_or("tools")
        .and_then(super::super::server_tools::ToolAdmission::from_value)
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
    let pin = super::super::channel_test::current();
    let pool_chain: Vec<&str> = match &pin {
        Some(pin) => pin.pools.iter().map(String::as_str).collect(),
        None => key.pool_chain(),
    };
    let est_usage = with_admission_hints(
        base_usage,
        admission_hints(state, &canonical, &pool_chain).await,
    );
    server_tools.admit(&book, &calc.model);
    let est_quote = server_tools.quote(&book, &calc, est_usage)?;

    // reasoning 意图归一（§11.26）：模型名后缀是一条路，请求体参数是另一条，
    // 合并到同一个 directive 后交给三向注入。**参数优先**——它是本次请求的显式意图。
    // 合并只影响"往上游注入什么"，不影响上面 `variant` 已经定下的计费名：
    // 与 OpenRouter 一致，只有模型名上的变体改计价，请求参数不改。
    // 请求体上面已经解析过一次（`parsed`），推理意图与路由偏好直接从它取，别再各读一遍原文：
    // 带 thinking 的请求（Claude Code 几乎全是）会让原文版的子串粗筛恒为真，白白多出整份解析。
    let request_reasoning = match parsed.as_ref() {
        Some(value) => reasoning::parse_request_value(value),
        None => reasoning::parse_request(body),
    };
    let directive = request_reasoning.merge(directive);
    // 顶层有没有统一 `reasoning` 对象：没有的话转发前的摘除什么也摘不到。解析失败（理论上不会：
    // 入口已经解析过）按「有」走原来的路径。
    let unified_reasoning = if parsed.as_ref().is_none_or(reasoning::has_unified_object) {
        UnifiedReasoning::Present
    } else {
        UnifiedReasoning::Absent
    };

    // 请求级单价上限（§11.24）：拿快照里的**最终**单价判——它已经过模型/分组/个人系数与
    // 修饰器全链，正是这次真会按之的价。判在预扣之前：超限直接拒，别扣了钱再让用户发现贵。
    let prefs = match parsed.as_ref() {
        Some(value) => super::super::routing_prefs::parse_value(value)?,
        None => super::super::routing_prefs::parse(body)?,
    };
    if let Some(over) = price_above_max(&est_quote, &prefs) {
        return Err(
            AppError::new(StatusCode::PAYMENT_REQUIRED, codes::PRICE_ABOVE_MAX).with_param(over),
        );
    }

    // Freeze every usable one-hop fallback before reserve. Never discover a dearer
    // fallback after the hold has already been established.
    let mut reserved_amount = est_quote.amount;
    let mut reserved_tokens = reserved_token_count(&est_usage);
    let mut fallback_models = Vec::new();
    let mut reservation_candidates = vec![super::super::reservation::Candidate::new(
        "primary",
        &canonical,
        calc.model.as_str(),
        est_usage,
        &est_quote,
    )?];
    // 测试钉住了渠道：换模型就不是在测它了
    if prefs.allow_fallbacks && response_parent.is_none() && pin.is_none() {
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
                ingress,
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
            // 兜底链里没给工具定价的备选模型直接跳过，不拖垮整个请求
            if !server_tools.allows_fallback(&book, &fallback_calc.model) {
                continue;
            }
            let Ok(quote) = server_tools.quote(&book, &fallback_calc, fallback_usage) else {
                continue;
            };
            if price_above_max(&quote, &prefs).is_some() {
                continue;
            }
            // 备选模型冻结不下来（用量或报价过不了校验）就和上面几种情形一样跳过：
            // 这条备选只是兜底，不该拖垮主模型本来可以放行的请求
            let candidate = match super::super::reservation::Candidate::new(
                "fallback",
                &fallback.canonical,
                fallback_calc.model.as_str(),
                fallback_usage,
                &quote,
            ) {
                Ok(candidate) => candidate,
                Err(error) => {
                    tracing::warn!(model = %fallback.canonical, %error, "备选模型预扣冻结失败，跳过这条备选");
                    continue;
                }
            };
            reservation_candidates.push(candidate);
            reserved_amount = reserved_amount.max(quote.amount);
            reserved_tokens = reserved_tokens.max(reserved_token_count(&fallback_usage));
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
    let reservation_snapshot = Arc::new(super::super::reservation::freeze(
        &info.requested_model,
        reserved_amount,
        reservation_candidates,
    )?);

    // 团成员月度限额（§6.1 软实时）
    super::super::auth::check_member_limit(state, &key).await?;
    // 分组级 [rpm, rph]（§11.32，限额随鉴权缓存下发）
    super::super::auth::check_group_rate(state, &key).await?;
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
    let super::super::admission::Reserved {
        pool: reservation_pool,
        source_window,
    } = super::super::admission::reserve(
        state,
        &key,
        request_id,
        reserved_amount,
        reserved_tokens,
        now,
    )
    .await?;

    // —— 预扣已建立：此后一切失败路径必须退款（settle_failure）——
    let bill_model_for_tier = canonical.clone();
    let trace = super::super::diagnostics::Trace::current()
        .unwrap_or_else(|| super::super::diagnostics::Trace::new(headers));
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
        unified_reasoning,
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
        claude_code_native: ingress == Ingress::Anthropic
            && okapi_providers::profiles::is_native_claude_code(body, user_agent(headers)),
        codex_client: okapi_providers::profiles::codex_client::detect(headers),
        client_ip: okapi_kernel::clients::detect_client_ip(headers),
        client_headers: Arc::new(super::super::oauth_cred::client_headers(headers)),
        pool_chain: pool_chain.iter().map(|p| (*p).to_owned()).collect(),
        pin: pin.map(Arc::new),
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
/// - 只有零候选（含入口协议不匹配、只剩仅限真机的渠道）触发——上游 4xx/5xx 是"打过了没打通"，
///   换模型只会藏住真实错误并让用户为两次调用付钱；
/// - 单跳：只读请求模型自己的链，不递归降级模型的链；
/// - 按实际服务模型计费（fallback_billing 重建计费上下文，快照记 requested_model）。
pub(super) async fn forward(
    bill: &RequestBilling,
    info: &ProbeInfo,
    body: &Bytes,
) -> Result<Response, ForwardFailure> {
    let first = try_model(bill, info, body).await;
    let zero_candidates = matches!(
        &first,
        Err(f) if matches!(
            f.error_code.as_str(),
            codes::NO_AVAILABLE_CHANNEL
                | codes::UNSUPPORTED_ENDPOINT
                | codes::CLAUDE_CODE_CLIENT_REQUIRED
                | codes::CODEX_CLIENT_REQUIRED
        )
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
                    codes::NO_AVAILABLE_CHANNEL
                        | codes::UNSUPPORTED_ENDPOINT
                        | codes::CLAUDE_CODE_CLIENT_REQUIRED
                        | codes::CODEX_CLIENT_REQUIRED
                ) => {}
            // 成功或真实上游失败：终止。降级只救"无人可打"，不救"打了没打通"
            other => return other,
        }
    }
    first
}

/// Consume the context priced before admission; do not reload fallback caps here.
pub(super) fn fallback_billing(bill: &RequestBilling, fb: &AdmittedFallback) -> RequestBilling {
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

/// 预扣前看这个模型的候选渠道会怎样改写请求（客户端模拟会加 system 文本、1h 缓存断点，
/// 上游分词器也与本地不同），按其中最贵的一个预扣。查不到候选就按原样估算：
/// 路由阶段会照常报无可用渠道，不在这里提前失败。
pub(super) async fn admission_hints(
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

/// 按 TPM 限额计的 token 数：和预扣同一口径（客户端配置放大后的 prompt + 补全上限）。
/// 用放大之前的 prompt 会让开了客户端模拟的 key 实际吃掉的 token 比计入限额的多。
pub(super) fn reserved_token_count(usage: &TokenUsage) -> u64 {
    u64::from(usage.prompt_tokens).saturating_add(u64::from(usage.completion_tokens))
}

/// 把客户端配置带来的成本放进预扣估算：放大 prompt、按 1h 缓存写入计价（首个请求的上界）。
pub(super) fn with_admission_hints(
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
