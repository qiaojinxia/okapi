//! 代理测活：按代理或按地址探一次目标。
use super::super::admin::{audit, guard_scoped};
use super::{ensure_proxy, not_found, parse_url};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_kernel::extract::Json as ExtractJson;
use okapi_kernel::extract::Path;
use okapi_store::egress;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, Default)]
pub struct ProbeReq {
    /// 探测地址；缺省 Cloudflare trace（一次拿到出口 IP 与国家）。
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Deserialize)]
pub struct ProbeUrlReq {
    pub url: String,
    #[serde(default)]
    pub target: Option<String>,
}

pub(super) async fn probe_target(
    state: &AppState,
    target: Option<&str>,
) -> Result<String, AppError> {
    let target = target
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(okapi_providers::egress_probe::DEFAULT_TARGET);
    // 探测地址是管理员给的：与上游地址同一道 SSRF 闸（代理自身不过闸，见 §11.30）
    okapi_kernel::ssrf::validate_api_base(&state.pg, target).await?;
    Ok(target.to_owned())
}

/// 错误摘要里不能带出代理密码。
pub(super) fn scrub(detail: &str, proxy_url: &str) -> String {
    let mut clean = detail.replace(proxy_url, "[proxy]");
    if let Ok(url) = reqwest::Url::parse(proxy_url)
        && let Some(password) = url.password().filter(|p| !p.is_empty())
    {
        clean = clean.replace(password, "***");
    }
    clean
}

pub(super) async fn run_probe(
    state: &AppState,
    proxy_url: &str,
    target: &str,
) -> (Value, ProbeOutcome) {
    match okapi_providers::egress_probe::probe(
        state.upstreams.openai.http(),
        Some(proxy_url),
        target,
    )
    .await
    {
        Ok(result) => (
            json!({
                "ok": true, "target": target, "status": result.status,
                "latency_ms": result.latency_ms, "exit_ip": result.exit_ip,
                "country": result.country,
            }),
            ProbeOutcome::Ok(result),
        ),
        Err(error) => {
            let detail = scrub(&error.detail, proxy_url);
            (
                json!({ "ok": false, "target": target, "error_code": error.code, "error": detail }),
                ProbeOutcome::Failed(error.code, detail),
            )
        }
    }
}

pub(super) enum ProbeOutcome {
    Ok(okapi_providers::egress_probe::ProbeResult),
    Failed(&'static str, String),
}

/// POST /admin/proxies/{id}/test：经该代理请求探测地址，记出口 IP / 国家 / 延迟。
/// 成功即视为人工确认恢复（清熔断）；失败只记录。
pub async fn test_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    body: Option<Json<ProbeReq>>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_proxy(&state, id, &actor, scope).await?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let target = probe_target(&state, req.target.as_deref()).await?;
    let url = egress::proxy_url(&state.pg, id, state.master_key.as_deref())
        .await?
        .ok_or_else(not_found)?;
    let (result, outcome) = run_probe(&state, &url, &target).await;
    let record = match &outcome {
        ProbeOutcome::Ok(r) => egress::ProbeRecord {
            ok: true,
            exit_ip: r.exit_ip.as_deref(),
            exit_country: r.country.as_deref(),
            latency_ms: i32::try_from(r.latency_ms).ok(),
            error: None,
        },
        ProbeOutcome::Failed(code, detail) => egress::ProbeRecord {
            ok: false,
            exit_ip: None,
            exit_country: None,
            latency_ms: None,
            error: Some(if detail.is_empty() { code } else { detail }),
        },
    };
    let mut result = result;
    if let Some(change) = egress::record_probe(&state.pg, id, record, true).await? {
        // 出口 IP 变了：固定分配在它上面的账号都随之换了 IP，测试结果里明说
        result["exit_ip_changed"] = json!(change);
    }
    if matches!(outcome, ProbeOutcome::Ok(_)) {
        // 恢复了：让缓存里被熔断滤掉的候选立刻回来
        state.invalidate_routing_caches();
    }
    audit(
        &state,
        &actor,
        "proxy.test",
        &id.to_string(),
        result.clone(),
    )
    .await;
    Ok(Json(result))
}

/// POST /admin/proxies/test：保存前先试一个地址（不落库）。
pub async fn test_proxy_url(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<ProbeUrlReq>,
) -> Result<Json<Value>, AppError> {
    let (_actor, _) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let (url, _) = parse_url(&req.url)?;
    let target = probe_target(&state, req.target.as_deref()).await?;
    let (result, _) = run_probe(&state, &url, &target).await;
    Ok(Json(result))
}
