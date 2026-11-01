//! 结算：计价档位选择、成功提交、失败退款与终态留痕。
use super::super::error::AppError;
use super::super::ingress::Ingress;
use super::{CandInfo, ForwardFailure, RequestBilling, RespMeta, elapsed_ms_i32};
use okapi_domain::{BillingState, ModelCode, Money, TokenUsage};
use okapi_ledger::{Pool, SettlementInput};
use okapi_pricing::{CalcContext, PriceBook, Quote, RatioFp};

// ---- 结算 ----

/// 按响应模型重选计费上下文（渠道 opt-in，Sub2API 0.1.175 对齐）：
/// 响应模型 ≠ 请求模型且价簿有其**精确名**定价 → 返回重建的上下文；
/// 无价/同名 → None（维持请求 canonical，fail-open 绝不因改名拒付）。
/// 别名解析不参与（结算路径不回 PG）。
pub(super) fn resolve_billing_calc(
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
pub(super) fn pick_settle_tier(
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
pub(super) async fn settle_commit(
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
            bill.server_tools.prices(&bill.book, &calc.model),
            usage.server_tool_usage,
        )
        .and_then(|()| {
            if bill.server_tools.has_tools() && calc.model != bill.calc.model {
                if bill
                    .server_tools
                    .prices(&bill.book, &bill.calc.model)
                    .is_some()
                    && bill.server_tools.prices(&bill.book, &calc.model).is_none()
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
            // 按响应模型计费的预扣上界：实际账单不得超过预扣。预扣按请求声明模型冻结，
            // 不含"悄悄换成更贵模型"的余量，实收越过预扣等于绕过预扣透支余额
            // （fail-closed 退款，与上方算价失败同一出口）。
            if calc.model != bill.calc.model && quote.amount > bill.reserved_amount {
                return Err(okapi_pricing::PricingError::InvalidServerToolAdmission(
                    "response_price_above_reservation",
                ));
            }
            let snapshot = super::super::reservation::settled_snapshot(
                &quote.snapshot,
                &bill.reservation_snapshot,
            )?;
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

    let mut snapshot = super::super::upstream_cost::snapshot(
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
    super::super::auth::record_settlement_counters(
        &bill.state,
        bill.user_id,
        bill.member_user_id,
        quote.amount.as_micros(),
        usage.total_raw(),
    )
    .await;
    // 选路反馈：时延 EWMA 供 least_latency 池排序（key 日消费在落账时已累加）。
    // 放在结算之后 = 不占热路径，且只有成功请求才计入时延样本。
    super::super::auth::record_channel_key_latency(
        &bill.state,
        info.key,
        ttft_ms.unwrap_or_else(|| elapsed_ms_i32(bill.started)),
    )
    .await;
    Ok(())
}

pub(super) async fn settle_failure(bill: &RequestBilling, failure: &ForwardFailure) {
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
pub(super) async fn record_terminal(
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

pub(super) fn usage_dimensions(
    bill: &RequestBilling,
    info: &CandInfo,
) -> okapi_ledger::pg::UsageDimensions {
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
