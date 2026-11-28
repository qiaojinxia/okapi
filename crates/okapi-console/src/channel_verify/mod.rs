//! 渠道模型验真（POST /admin/channels/{id}/verify，SSE 实时推送）：
//!
//! 通过进程内 `DataPlaneBridge` + `ChannelPin` 钉住目标渠道（及可选账号），依次运行：
//! 1. L1 协议与能力探针：返回模型家族一致性、确定性 Nonce Tool Calling、Anthropic `thinking.signature`
//! 2. 贝叶斯开集 Tokenizer 指纹（内置 62 类 / 72 探针，4-12 次极短请求自适应早停 + 基线漂移检查）
//! 3. L2 深度行为指纹归因（`lmfpd` / ModelTrace `shared-detector-v2` 3 轮随机整数挑战）
//!
//! 与 `channel_try` 保持一致的计费与日志口径（不伪造免费请求，不自动封禁渠道）。

mod wire;

use crate::admin::{audit, ensure_channel_owner, guard_scoped};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::{self, FuturesUnordered, StreamExt};
use okapi_api::{codes, permissions};
use okapi_engine::channel_test::ChannelPin;
use okapi_store::AuthedKey;
use okapi_engine::model_verify::{
    BankStatus, ChallengeSampleSummary, CheckStatus, DEFAULT_MAX_PROBES, FingerprintReport,
    ModelNameCheck, Output, ProtocolCheckResult, SignatureCheck, TokenizerObservation,
    TokenizerReport, ToolCallCheck, VerdictInput, VerifyCheckSelection, VerifyEvent, VerifyReport,
    bank_status, compute_posterior, compute_verdict, evaluate_challenge_text, generate_challenges,
    load_detector, next_probes, probing_done, score_outputs, synthesize_verdict, wrap_probe_text,
};
use okapi_engine::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::{Json as ExtractJson, Path};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use wire::{CallContext, ParsedResponse, Surface, invoke_pinned};

const MIN_TOKENIZER_PROBES: usize = 4;
const MAX_TOKENIZER_PROBES_CAP: usize = 24;
const TOKENIZER_BATCH_SIZE: usize = 3;
const TOOL_ECHO_NAME: &str = "verify_echo";
const MIN_SIGNATURE_LEN: usize = 16;

#[derive(Debug, Deserialize)]
pub struct VerifyReq {
    pub model: String,
    #[serde(default)]
    pub key_id: Option<i64>,
    #[serde(default)]
    pub checks: VerifyCheckSelection,
    #[serde(default)]
    pub max_tokenizer_probes: Option<usize>,
}

struct PreparedVerify {
    actor: Arc<AuthedKey>,
    audit_meta: okapi_store::admin::AuditMeta,
    channel_id: i64,
    provider: String,
    model: String,
    upstream_model: String,
    key_id: Option<i64>,
    mimic_profile_active: bool,
    primary_surface: Surface,
    upstream_dialect: &'static str,
    checks: VerifyCheckSelection,
    max_tokenizer_probes: usize,
    ctx: CallContext,
}

/// 查询当前模型验真参考库（Tokenizer 银行 + ModelTrace 行为指纹库）的本地状态。
pub async fn verify_bank_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<BankStatus>, AppError> {
    let _ = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    Ok(Json(bank_status().await))
}

/// 管理员手动强制刷新 ModelTrace / `lmfpd` 行为指纹参考库缓存。
pub async fn refresh_verify_bank(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<BankStatus>, AppError> {
    let (actor, _) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    load_detector(true).await.map_err(|err| {
        tracing::warn!(error = %err, "刷新模型验真参考库失败");
        AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
    })?;
    let status = bank_status().await;
    audit(
        &state,
        &actor,
        "channel.verify_bank_refresh",
        "model_verify_bank",
        json!({"built_at": status.built_at, "loaded": status.loaded}),
    )
    .await;
    Ok(Json(status))
}

/// 发起一次实时 SSE 流式渠道模型验真。
pub async fn verify_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(channel_id): Path<i64>,
    ExtractJson(req): ExtractJson<VerifyReq>,
) -> Response {
    let prepared = match prepare(&state, &headers, channel_id, req).await {
        Ok(p) => p,
        Err(error) => return error.into_response_with(None),
    };
    // 审计由异步流结束时写入完整验真结论，避免 audit_fallback 重复补一条通用记录
    crate::audit_fallback::note_audited();

    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    tokio::spawn(async move {
        run_verification(prepared, tx).await;
    });

    let stream = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn prepare(
    state: &AppState,
    headers: &HeaderMap,
    channel_id: i64,
    req: VerifyReq,
) -> Result<PreparedVerify, AppError> {
    let (actor, scope) = guard_scoped(state, headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(state, channel_id, &actor, scope).await?;

    let model = req.model.trim().to_owned();
    if model.is_empty() {
        return Err(AppError::bad_request().with_param("model"));
    }
    if !req.checks.protocol && !req.checks.tokenizer && !req.checks.fingerprint {
        return Err(AppError::bad_request().with_param("checks"));
    }

    let row: Option<(String, Value, Value, Value, Vec<String>, bool)> = sqlx::query_as(
        r"SELECT c.provider, c.models, c.model_mapping, c.settings,
                 COALESCE(array_agg(pc.pool_code::text) FILTER (WHERE pc.pool_code IS NOT NULL), '{}'),
                 $2::bigint IS NULL OR EXISTS (
                     SELECT 1 FROM channel_keys ck WHERE ck.channel_id = c.id AND ck.id = $2)
          FROM channels c
          LEFT JOIN pool_channels pc ON pc.channel_id = c.id
          WHERE c.id = $1 AND c.deleted_at IS NULL
          GROUP BY c.id",
    )
    .bind(channel_id)
    .bind(req.key_id)
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;

    let Some((provider, models, model_mapping, settings, pools, key_ok)) = row else {
        return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND));
    };
    if !key_ok {
        return Err(AppError::bad_request().with_param("key_id"));
    }
    if !models
        .as_array()
        .is_some_and(|list| list.iter().any(|m| m.as_str() == Some(model.as_str())))
    {
        return Err(AppError::bad_request().with_param("model"));
    }
    if pools.is_empty() {
        return Err(AppError::bad_request().with_param("channel_orphan"));
    }

    let _ = state.data_plane()?;
    let upstream_model =
        okapi_store::model_mapping::resolve(&model_mapping, &model).into_owned();

    let extensions = settings.get("extensions").cloned().unwrap_or(Value::Null);
    let mimic_profile_active = matches!(
        okapi_providers::profiles::client_profile(&extensions),
        Ok(Some(
            okapi_providers::profiles::ClientProfile::ClaudeCode { mode, .. }
        )) if mode != okapi_providers::profiles::ProfileMode::Passthrough
    );

    let upstream_dialect = okapi_engine::dialect::upstream_dialect(&provider, &upstream_model);
    let primary_surface = if provider == "codex" {
        Surface::Responses
    } else if upstream_dialect == "anthropic" {
        Surface::Messages
    } else {
        Surface::Chat
    };

    let max_tokenizer_probes = req
        .max_tokenizer_probes
        .unwrap_or(DEFAULT_MAX_PROBES)
        .clamp(MIN_TOKENIZER_PROBES, MAX_TOKENIZER_PROBES_CAP);

    let pin = ChannelPin {
        channel_id,
        key_id: req.key_id,
        pools,
    };
    let ctx = CallContext::new(state.clone(), headers.clone(), pin);
    let audit_meta = crate::audit_fallback::current_meta(Some(actor.key_id));

    Ok(PreparedVerify {
        actor,
        audit_meta,
        channel_id,
        provider,
        model,
        upstream_model,
        key_id: req.key_id,
        mimic_profile_active,
        primary_surface,
        upstream_dialect,
        checks: req.checks,
        max_tokenizer_probes,
        ctx,
    })
}

async fn emit(tx: &mpsc::Sender<Result<Event, Infallible>>, event: &VerifyEvent) -> bool {
    let event_name = match event {
        VerifyEvent::Init { .. } => "init",
        VerifyEvent::Stage { .. } => "stage",
        VerifyEvent::ProtocolResult { .. } => "protocol_result",
        VerifyEvent::TokenizerProgress { .. } => "tokenizer_progress",
        VerifyEvent::TokenizerResult { .. } => "tokenizer_result",
        VerifyEvent::FingerprintSample { .. } => "fingerprint_sample",
        VerifyEvent::FingerprintResult { .. } => "fingerprint_result",
        VerifyEvent::Done { .. } => "done",
        VerifyEvent::Error { .. } => "error",
    };
    let Ok(payload) = serde_json::to_string(event) else {
        return false;
    };
    tx.send(Ok(Event::default().event(event_name).data(payload)))
        .await
        .is_ok()
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

async fn run_verification(p: PreparedVerify, tx: mpsc::Sender<Result<Event, Infallible>>) {
    let total_started = Instant::now();
    let bank = bank_status().await;
    if !emit(
        &tx,
        &VerifyEvent::Init {
            channel_id: p.channel_id,
            provider: p.provider.clone(),
            model: p.model.clone(),
            upstream_model: p.upstream_model.clone(),
            key_id: p.key_id,
            mimic_profile_active: p.mimic_profile_active,
            checks: p.checks,
            bank,
        },
    )
    .await
    {
        return;
    }

    let mut errors: Vec<String> = Vec::new();
    let protocol = maybe_run_protocol(&p, &tx).await;
    let tokenizer = maybe_run_tokenizer(&p, &tx, &mut errors).await;
    let fingerprint = maybe_run_fingerprint(&p, &tx, &mut errors).await;

    let (verdict, warnings) = synthesize_verdict(&VerdictInput {
        protocol: protocol.as_ref(),
        tokenizer: tokenizer.as_ref(),
        fingerprint: fingerprint.as_ref(),
        mimic_profile_active: p.mimic_profile_active,
        errors: &errors,
    });

    let report = VerifyReport {
        channel_id: p.channel_id,
        provider: p.provider.clone(),
        model: p.model.clone(),
        upstream_model: p.upstream_model.clone(),
        key_id: p.key_id,
        mimic_profile_active: p.mimic_profile_active,
        verdict,
        warnings,
        protocol,
        tokenizer,
        fingerprint,
        elapsed_ms: elapsed_ms(total_started),
    };

    let _ = emit(
        &tx,
        &VerifyEvent::Done {
            report: Box::new(report.clone()),
        },
    )
    .await;
    record_verify_audit(&p, &report, tx.is_closed()).await;
}

async fn maybe_run_protocol(
    p: &PreparedVerify,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Option<ProtocolCheckResult> {
    if !p.checks.protocol || tx.is_closed() {
        return None;
    }
    let _ = emit(
        tx,
        &VerifyEvent::Stage {
            stage: "protocol".into(),
            status: "running".into(),
            detail: None,
            completed: None,
            total: None,
        },
    )
    .await;
    let res = run_protocol_checks(p).await;
    let _ = emit(tx, &VerifyEvent::ProtocolResult { result: res.clone() }).await;
    Some(res)
}

async fn maybe_run_tokenizer(
    p: &PreparedVerify,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    errors: &mut Vec<String>,
) -> Option<TokenizerReport> {
    if !p.checks.tokenizer || tx.is_closed() {
        return None;
    }
    match run_tokenizer_check(p, tx).await {
        Ok(res) => {
            let _ = emit(tx, &VerifyEvent::TokenizerResult { result: res.clone() }).await;
            Some(res)
        }
        Err(err) => {
            let _ = emit(
                tx,
                &VerifyEvent::Error {
                    stage: "tokenizer".into(),
                    message: err.clone(),
                },
            )
            .await;
            errors.push(format!("tokenizer: {err}"));
            None
        }
    }
}

async fn maybe_run_fingerprint(
    p: &PreparedVerify,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    errors: &mut Vec<String>,
) -> Option<FingerprintReport> {
    if !p.checks.fingerprint || tx.is_closed() {
        return None;
    }
    match run_fingerprint_check(p, tx).await {
        Ok(res) => {
            let _ = emit(
                tx,
                &VerifyEvent::FingerprintResult {
                    result: res.clone(),
                },
            )
            .await;
            Some(res)
        }
        Err(err) => {
            let _ = emit(
                tx,
                &VerifyEvent::Error {
                    stage: "fingerprint".into(),
                    message: err.clone(),
                },
            )
            .await;
            errors.push(format!("fingerprint: {err}"));
            None
        }
    }
}

async fn record_verify_audit(p: &PreparedVerify, report: &VerifyReport, cancelled: bool) {
    let detail = json!({
        "model": report.model,
        "upstream_model": report.upstream_model,
        "key_id": report.key_id,
        "verdict": report.verdict,
        "cancelled": cancelled,
        "elapsed_ms": report.elapsed_ms,
        "tokenizer_top": report.tokenizer.as_ref().map(|t| &t.verdict.top.id),
        "fingerprint_prediction": report.fingerprint.as_ref().map(|f| &f.prediction),
        "fingerprint_family": report.fingerprint.as_ref().map(|f| &f.family_prediction),
    });
    crate::audit::record_event_with_meta(
        &p.ctx.state,
        &format!("admin:{}", p.actor.actor_user_id()),
        "channel.verify",
        &p.channel_id.to_string(),
        detail,
        &p.audit_meta,
    )
    .await;
}

async fn run_protocol_checks(p: &PreparedVerify) -> ProtocolCheckResult {
    let (tool_call, mut response_model) = run_tool_call_probe(p).await;
    let (signature, sig_model) = run_signature_probe(p).await;
    if response_model.is_none() {
        response_model = sig_model;
    }
    let model_name = ModelNameCheck::evaluate(&p.model, &p.upstream_model, response_model);
    ProtocolCheckResult {
        model_name,
        tool_call,
        signature,
    }
}

async fn run_tool_call_probe(p: &PreparedVerify) -> (ToolCallCheck, Option<String>) {
    let started = Instant::now();
    let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let nonce = format!("okapi-{short_id}");
    let prompt = format!(
        "You must call the `{TOOL_ECHO_NAME}` tool with nonce=\"{nonce}\". Do not write any other text."
    );
    let body = build_tool_call_body(p.primary_surface, &p.model, &prompt);
    match invoke_pinned(&p.ctx, p.primary_surface, &body).await {
        Ok(resp) => {
            let check = evaluate_tool_call_response(&nonce, &resp, elapsed_ms(started));
            (check, resp.response_model)
        }
        Err(err) => (
            ToolCallCheck {
                status: CheckStatus::Error,
                nonce,
                tool_called: false,
                nonce_matched: false,
                arguments_valid_json: false,
                latency_ms: elapsed_ms(started),
                detail: Some(err),
            },
            None,
        ),
    }
}

fn build_tool_call_body(surface: Surface, model: &str, prompt: &str) -> Value {
    let schema = json!({
        "type": "object",
        "properties": {
            "nonce": { "type": "string", "description": "Exact verification nonce to echo back" }
        },
        "required": ["nonce"],
        "additionalProperties": false
    });
    match surface {
        Surface::Chat => json!({
            "model": model,
            "stream": true,
            "max_tokens": 128,
            "stream_options": { "include_usage": true },
            "messages": [{ "role": "user", "content": prompt }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": TOOL_ECHO_NAME,
                    "description": "Echo back the exact verification nonce.",
                    "parameters": schema
                }
            }],
            "tool_choice": { "type": "function", "function": { "name": TOOL_ECHO_NAME } }
        }),
        Surface::Responses => json!({
            "model": model,
            "stream": true,
            "store": false,
            "max_output_tokens": 128,
            "input": [{ "role": "user", "content": prompt }],
            "tools": [{
                "type": "function",
                "name": TOOL_ECHO_NAME,
                "description": "Echo back the exact verification nonce.",
                "parameters": schema
            }],
            "tool_choice": { "type": "function", "name": TOOL_ECHO_NAME }
        }),
        Surface::Messages => json!({
            "model": model,
            "stream": true,
            "max_tokens": 128,
            "messages": [{ "role": "user", "content": prompt }],
            "tools": [{
                "name": TOOL_ECHO_NAME,
                "description": "Echo back the exact verification nonce.",
                "input_schema": schema
            }],
            "tool_choice": { "type": "tool", "name": TOOL_ECHO_NAME }
        }),
    }
}

fn evaluate_tool_call_response(
    nonce: &str,
    resp: &ParsedResponse,
    latency_ms: u64,
) -> ToolCallCheck {
    let matching = resp.tool_calls.iter().find(|c| c.name == TOOL_ECHO_NAME);
    let Some(call) = matching else {
        let detail = if resp.tool_calls.is_empty() {
            "Model replied with text instead of invoking the required tool.".to_owned()
        } else {
            format!("Model called unexpected tool `{}`.", resp.tool_calls[0].name)
        };
        return ToolCallCheck {
            status: CheckStatus::Fail,
            nonce: nonce.to_owned(),
            tool_called: !resp.tool_calls.is_empty(),
            nonce_matched: false,
            arguments_valid_json: false,
            latency_ms,
            detail: Some(detail),
        };
    };

    let parsed: Result<Value, _> = serde_json::from_str(&call.arguments);
    let (arguments_valid_json, nonce_matched) = match parsed {
        Ok(val) => {
            let echoed = val
                .get("nonce")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            (true, echoed == nonce)
        }
        Err(_) => (false, false),
    };
    let status = if arguments_valid_json && nonce_matched {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    let detail = if status == CheckStatus::Pass {
        None
    } else if !arguments_valid_json {
        Some("Tool call arguments were not valid JSON.".to_owned())
    } else {
        Some("Tool call returned wrong nonce value.".to_owned())
    };
    ToolCallCheck {
        status,
        nonce: nonce.to_owned(),
        tool_called: true,
        nonce_matched,
        arguments_valid_json,
        latency_ms,
        detail,
    }
}

fn resolve_anthropic_thinking_mode(upstream_model: &str) -> Option<&'static str> {
    let profile = okapi_providers::model_parameters::profile("anthropic", upstream_model, None);
    if !profile.efforts.is_empty() {
        return Some("adaptive");
    }
    if profile.budget_min.is_some() {
        return Some("enabled");
    }
    let lower = upstream_model.to_ascii_lowercase();
    if lower.contains("claude-4")
        || lower.contains("claude-sonnet-4")
        || lower.contains("claude-opus-4")
        || lower.contains("claude-haiku-4")
        || lower.contains("claude-3-7")
        || lower.contains("claude-fable")
        || lower.contains("claude-mythos")
    {
        return Some("enabled");
    }
    None
}

async fn run_signature_probe(p: &PreparedVerify) -> (SignatureCheck, Option<String>) {
    if p.upstream_dialect != "anthropic" {
        return (
            SignatureCheck::skipped("Applicable only to Anthropic-dialect channels."),
            None,
        );
    }
    let Some(mode) = resolve_anthropic_thinking_mode(&p.upstream_model) else {
        return (
            SignatureCheck::skipped("Declared Claude model does not advertise extended thinking."),
            None,
        );
    };

    let started = Instant::now();
    let mut body = json!({
        "model": p.model,
        "stream": true,
        "max_tokens": 1500,
        "messages": [{
            "role": "user",
            "content": "Compute 17 * 23 step by step, then state the final integer."
        }]
    });
    if mode == "adaptive" {
        body["thinking"] = json!({ "type": "adaptive" });
        body["output_config"] = json!({ "effort": "low" });
    } else {
        body["thinking"] = json!({ "type": "enabled", "budget_tokens": 1024 });
    }

    match invoke_pinned(&p.ctx, Surface::Messages, &body).await {
        Ok(resp) => {
            let sig_len = resp.signature.len();
            let has_sig = sig_len >= MIN_SIGNATURE_LEN;
            let (status, detail) = if resp.has_thinking_block && has_sig {
                (CheckStatus::Pass, None)
            } else if resp.has_thinking_block {
                (
                    CheckStatus::Fail,
                    Some(
                        "Thinking block returned without cryptographic signature (stripped or forged)."
                            .to_owned(),
                    ),
                )
            } else {
                (
                    CheckStatus::Warn,
                    Some(
                        "No thinking block returned; upstream may have suppressed extended thinking."
                            .to_owned(),
                    ),
                )
            };
            (
                SignatureCheck {
                    status,
                    applicable: true,
                    has_thinking_block: resp.has_thinking_block,
                    has_signature: has_sig,
                    signature_len: has_sig.then_some(sig_len),
                    thinking_mode: Some(mode.to_owned()),
                    latency_ms: elapsed_ms(started),
                    detail,
                },
                resp.response_model,
            )
        }
        Err(err) => (
            SignatureCheck {
                status: CheckStatus::Error,
                applicable: true,
                has_thinking_block: false,
                has_signature: false,
                signature_len: None,
                thinking_mode: Some(mode.to_owned()),
                latency_ms: elapsed_ms(started),
                detail: Some(err),
            },
            None,
        ),
    }
}

fn build_text_probe_body(
    surface: Surface,
    model: &str,
    prompt: &str,
    max_tokens: Option<u32>,
) -> Value {
    match surface {
        Surface::Chat => {
            let mut v = json!({
                "model": model,
                "stream": true,
                "stream_options": { "include_usage": true },
                "messages": [{ "role": "user", "content": prompt }]
            });
            if let Some(max) = max_tokens {
                v["max_tokens"] = json!(max);
            }
            v
        }
        Surface::Responses => {
            let mut v = json!({
                "model": model,
                "stream": true,
                "store": false,
                "input": [{ "role": "user", "content": prompt }]
            });
            if let Some(max) = max_tokens {
                v["max_output_tokens"] = json!(max.max(16));
            }
            v
        }
        Surface::Messages => json!({
            "model": model,
            "stream": true,
            "max_tokens": max_tokens.unwrap_or(8192),
            "messages": [{ "role": "user", "content": prompt }]
        }),
    }
}

async fn send_tokenizer_probe(
    p: &PreparedVerify,
    probe_id: Option<&str>,
) -> Result<TokenizerObservation, String> {
    let prompt = wrap_probe_text(probe_id)?;
    let body = build_text_probe_body(p.primary_surface, &p.model, &prompt, Some(16));
    let resp = invoke_pinned(&p.ctx, p.primary_surface, &body).await?;
    let tokens = resp.prompt_tokens.ok_or_else(|| {
        "Upstream response did not report prompt_tokens / input_tokens.".to_owned()
    })?;
    Ok(TokenizerObservation {
        probe: probe_id.map(str::to_owned),
        tokens,
        response_model: resp.response_model,
    })
}

async fn run_tokenizer_check(
    p: &PreparedVerify,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<TokenizerReport, String> {
    let started = Instant::now();
    let max_probes = p.max_tokenizer_probes;
    let _ = emit(
        tx,
        &VerifyEvent::Stage {
            stage: "tokenizer".into(),
            status: "running".into(),
            detail: Some("Measuring baseline prompt tokens".into()),
            completed: Some(0),
            total: Some(max_probes),
        },
    )
    .await;

    let base_start = send_tokenizer_probe(p, None).await?;
    let baseline_tokens = base_start.tokens;
    let mut observations = vec![base_start];
    let mut asked: HashSet<String> = HashSet::new();
    let mut posterior = compute_posterior(&observations)?;
    let mut probes_run = 0usize;

    while probes_run < max_probes && !tx.is_closed() {
        if let Some(ref post) = posterior
            && probing_done(post, max_probes)
        {
            break;
        }
        let batch_size = TOKENIZER_BATCH_SIZE.min(max_probes - probes_run);
        let plan = next_probes(posterior.as_ref(), &asked, batch_size)?;
        if plan.is_empty() {
            break;
        }
        for id in &plan {
            asked.insert(id.clone());
        }
        let futs = plan
            .iter()
            .map(|id| async move { send_tokenizer_probe(p, Some(id.as_str())).await });
        let results = futures::future::join_all(futs).await;
        let mut batch_ok = 0usize;
        let mut last_err = None;
        for res in results {
            match res {
                Ok(obs) => {
                    observations.push(obs);
                    probes_run += 1;
                    batch_ok += 1;
                }
                Err(err) => last_err = Some(err),
            }
        }
        if batch_ok == 0 {
            return Err(last_err.unwrap_or_else(|| "all tokenizer probes failed".into()));
        }
        posterior = compute_posterior(&observations)?;
        if let Some(ref post) = posterior {
            let _ = emit(
                tx,
                &VerifyEvent::TokenizerProgress {
                    probes_run,
                    max_probes,
                    posterior: post.clone(),
                },
            )
            .await;
        }
    }

    let baseline_drift = if tx.is_closed() {
        None
    } else {
        send_tokenizer_probe(p, None)
            .await
            .ok()
            .map(|end_obs| (end_obs.tokens - baseline_tokens).abs())
    };

    let final_posterior = posterior
        .ok_or_else(|| "Tokenizer probe collected insufficient observations.".to_owned())?;
    let mut verdict = compute_verdict(&final_posterior, &p.upstream_model)?;
    if verdict.claim.is_none() && p.upstream_model != p.model {
        verdict = compute_verdict(&final_posterior, &p.model)?;
    }

    Ok(TokenizerReport {
        posterior: final_posterior,
        verdict,
        probes_run,
        baseline_tokens,
        baseline_drift,
        latency_ms: elapsed_ms(started),
    })
}

async fn sample_single_challenge(
    p: &PreparedVerify,
    idx: usize,
    prompt: &str,
    expected_count: u64,
) -> (usize, ChallengeSampleSummary, Output) {
    let started = Instant::now();
    let body = build_text_probe_body(p.primary_surface, &p.model, prompt, None);
    let res = invoke_pinned(&p.ctx, p.primary_surface, &body).await;
    let latency_ms = elapsed_ms(started);
    match res {
        Ok(resp) => {
            let (parsed_numbers, minimum_numbers, accepted) =
                evaluate_challenge_text(&resp.text, expected_count);
            let summary = ChallengeSampleSummary {
                index: idx,
                expected_count,
                parsed_numbers,
                minimum_numbers,
                accepted,
                latency_ms,
                response_model: resp.response_model,
                error: None,
            };
            let output = Output {
                text: if accepted { resp.text } else { String::new() },
                expected_count,
            };
            (idx, summary, output)
        }
        Err(err) => {
            let (_, minimum_numbers, _) = evaluate_challenge_text("", expected_count);
            let summary = ChallengeSampleSummary {
                index: idx,
                expected_count,
                parsed_numbers: 0,
                minimum_numbers,
                accepted: false,
                latency_ms,
                response_model: None,
                error: Some(err),
            };
            (
                idx,
                summary,
                Output {
                    text: String::new(),
                    expected_count,
                },
            )
        }
    }
}

async fn run_fingerprint_check(
    p: &PreparedVerify,
    tx: &mpsc::Sender<Result<Event, Infallible>>,
) -> Result<FingerprintReport, String> {
    let _ = emit(
        tx,
        &VerifyEvent::Stage {
            stage: "fingerprint".into(),
            status: "loading_bank".into(),
            detail: Some("Loading ModelTrace reference bank".into()),
            completed: Some(0),
            total: Some(3),
        },
    )
    .await;
    let detector = load_detector(false).await?;
    let challenges = generate_challenges()?;

    let _ = emit(
        tx,
        &VerifyEvent::Stage {
            stage: "fingerprint".into(),
            status: "running".into(),
            detail: Some("Running 3 behavioral fingerprint challenges".into()),
            completed: Some(0),
            total: Some(challenges.len()),
        },
    )
    .await;

    let mut tasks = FuturesUnordered::new();
    for (idx, challenge) in challenges.iter().enumerate() {
        tasks.push(sample_single_challenge(
            p,
            idx,
            &challenge.prompt,
            challenge.expected_count,
        ));
    }

    let mut summaries: Vec<Option<ChallengeSampleSummary>> = vec![None; challenges.len()];
    let mut outputs: Vec<Option<Output>> = vec![None; challenges.len()];
    let mut completed = 0usize;

    while let Some((idx, summary, output)) = tasks.next().await {
        completed += 1;
        let _ = emit(
            tx,
            &VerifyEvent::FingerprintSample {
                sample: summary.clone(),
            },
        )
        .await;
        let _ = emit(
            tx,
            &VerifyEvent::Stage {
                stage: "fingerprint".into(),
                status: "running".into(),
                detail: None,
                completed: Some(completed),
                total: Some(challenges.len()),
            },
        )
        .await;
        summaries[idx] = Some(summary);
        outputs[idx] = Some(output);
    }

    let ordered_summaries: Vec<ChallengeSampleSummary> = summaries.into_iter().flatten().collect();
    let ordered_outputs: Vec<Output> = outputs.into_iter().flatten().collect();

    if ordered_summaries.iter().all(|s| !s.accepted) {
        let first_err = ordered_summaries
            .iter()
            .find_map(|s| s.error.clone())
            .unwrap_or_else(|| {
                "All 3 behavioral challenges failed or produced too few numbers.".into()
            });
        return Err(first_err);
    }

    let mut report = score_outputs(&detector, &p.upstream_model, &ordered_outputs, ordered_summaries)?;
    if !report.declared_in_bank && p.upstream_model != p.model {
        let alt = score_outputs(&detector, &p.model, &ordered_outputs, report.samples.clone())?;
        if alt.declared_in_bank
            || (report.declared_family.is_none() && alt.declared_family.is_some())
        {
            report = alt;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_tool_call_nonce_echo_strictly() {
        let ok = ParsedResponse {
            tool_calls: vec![wire::ParsedToolCall {
                name: TOOL_ECHO_NAME.into(),
                arguments: r#"{"nonce":"okapi-abcd1234"}"#.into(),
            }],
            ..Default::default()
        };
        let pass = evaluate_tool_call_response("okapi-abcd1234", &ok, 120);
        assert_eq!(pass.status, CheckStatus::Pass);
        assert!(pass.tool_called && pass.nonce_matched && pass.arguments_valid_json);

        let wrong_nonce = evaluate_tool_call_response("okapi-99999999", &ok, 120);
        assert_eq!(wrong_nonce.status, CheckStatus::Fail);
        assert!(wrong_nonce.tool_called && wrong_nonce.arguments_valid_json && !wrong_nonce.nonce_matched);

        let no_call = evaluate_tool_call_response("okapi-abcd1234", &ParsedResponse::default(), 120);
        assert_eq!(no_call.status, CheckStatus::Fail);
        assert!(!no_call.tool_called);
    }

    #[test]
    fn resolves_thinking_modes_for_claude_generations() {
        assert_eq!(resolve_anthropic_thinking_mode("claude-opus-4-6"), Some("adaptive"));
        assert_eq!(resolve_anthropic_thinking_mode("claude-sonnet-4-5-20250929"), Some("enabled"));
        assert_eq!(resolve_anthropic_thinking_mode("claude-3-haiku-20240307"), None);
    }
}
