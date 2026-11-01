//! 出口代理的增删改查与流量。
use super::super::admin::{audit, guard_scoped};
use super::super::query::{PageQuery, Query};
use super::{
    ensure_max_concurrency, ensure_max_keys, ensure_name, ensure_note, ensure_proxy, ensure_status,
    not_found, owner_filter, parse_url, store_endpoint,
};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use crate::kernel::extract::Path;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_providers::http::ProxyEndpoint;
use okapi_store::egress;
use okapi_store::egress_traffic;
use serde::Deserialize;
use serde_json::{Value, json};

/// GET /admin/proxies：`?q=` 匹配名称 / 主机。只回掩码后的地址。
pub async fn list_proxies(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let page = egress::list_proxies(
        &state.pg,
        owner_filter(scope, &actor),
        q.keyword(),
        q.slice(),
    )
    .await?;
    let now = chrono::Utc::now();
    let data: Vec<Value> = page
        .data
        .into_iter()
        .map(|p| {
            let masked = ProxyEndpoint {
                scheme: p.scheme.clone(),
                host: p.host.clone(),
                port: u16::try_from(p.port).unwrap_or_default(),
                username: p.username.clone(),
                // 列表不解密：有用户名就按「可能带密码」显示掩码
                has_password: p.username.is_some(),
            }
            .masked();
            let cooling = p.cooldown_until.is_some_and(|t| t > now);
            let mut v = serde_json::to_value(&p).unwrap_or_default();
            v["url_masked"] = json!(masked);
            v["cooling"] = json!(cooling);
            v
        })
        .collect();
    Ok(Json(json!({ "data": data, "total": page.total })))
}

#[derive(Deserialize)]
pub struct TrafficQuery {
    /// `1h` / `24h`（缺省）/ `7d`。
    #[serde(default)]
    pub range: Option<String>,
}

/// GET /admin/proxies/traffic：各代理在范围内的上下行字节与请求数，按范围自动分桶
/// （1h 按分钟、24h 按 5 分钟、7d 按小时）。没有流量的代理不出现。
pub async fn proxy_traffic(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TrafficQuery>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let range = egress_traffic::Range::parse(q.range.as_deref())
        .ok_or_else(|| AppError::bad_request().with_param("range"))?;
    let Some(ch) = state.ch.as_ref() else {
        return Err(AppError::new(
            StatusCode::NOT_IMPLEMENTED,
            codes::STATS_DISABLED,
        ));
    };
    let owned = match owner_filter(scope, &actor) {
        Some(owner) => Some(egress_traffic::owned_proxy_ids(&state.pg, owner).await?),
        None => None,
    };
    let points = egress_traffic::series(ch, range, owned.as_deref()).await?;
    let mut proxies: Vec<Value> = Vec::new();
    for point in points {
        let entry = json!({
            "t": point.t,
            "up_bytes": point.up_bytes,
            "down_bytes": point.down_bytes,
            "requests": point.requests,
        });
        match proxies.last_mut() {
            Some(last) if last["proxy_id"] == point.proxy_id => {
                for key in ["up_bytes", "down_bytes", "requests"] {
                    last[key] =
                        json!(last[key].as_u64().unwrap_or(0) + entry[key].as_u64().unwrap_or(0));
                }
                if let Some(list) = last["points"].as_array_mut() {
                    list.push(entry);
                }
            }
            _ => proxies.push(json!({
                "proxy_id": point.proxy_id,
                "up_bytes": point.up_bytes,
                "down_bytes": point.down_bytes,
                "requests": point.requests,
                "points": [entry],
            })),
        }
    }
    Ok(Json(json!({
        "range": q.range.as_deref().unwrap_or("24h"),
        "bucket_secs": range.bucket_secs(),
        "span_secs": range.span_secs(),
        "proxies": proxies,
    })))
}

#[derive(Deserialize)]
pub struct CreateProxyReq {
    /// 缺省 = `host:port`。
    #[serde(default)]
    pub name: Option<String>,
    pub url: String,
    #[serde(default)]
    pub max_keys: Option<i32>,
    /// 经该代理同时在途的上游请求上限（跨 key、跨副本）；缺省不限。
    #[serde(default)]
    pub max_concurrency: Option<i32>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub status: Option<i16>,
}

/// POST /admin/proxies。
pub async fn create_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<CreateProxyReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, _) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let (url, endpoint) = parse_url(&req.url)?;
    let default_name = format!("{}:{}", endpoint.host, endpoint.port);
    let name = ensure_name(req.name.as_deref().unwrap_or(&default_name))?;
    ensure_max_keys(req.max_keys)?;
    ensure_max_concurrency(req.max_concurrency)?;
    ensure_status(req.status)?;
    let note = ensure_note(req.note.as_deref())?;
    let id = egress::create_proxy(
        &state.pg,
        &egress::NewProxy {
            name,
            url: &url,
            endpoint: store_endpoint(&endpoint),
            max_keys: req.max_keys,
            max_concurrency: req.max_concurrency,
            note,
            status: req.status.unwrap_or(1),
            owner_id: Some(actor.actor_user_id()),
        },
        state.master_key.as_deref(),
    )
    .await?;
    audit(
        &state,
        &actor,
        "proxy.create",
        &id.to_string(),
        json!({ "name": name, "url": endpoint.masked(), "max_keys": req.max_keys }),
    )
    .await;
    Ok(Json(json!({ "id": id, "url_masked": endpoint.masked() })))
}

#[derive(Deserialize)]
// 三态补丁字段，豁免理由同 console::double_option
#[allow(clippy::option_option)]
pub struct PatchProxyReq {
    #[serde(default)]
    pub name: Option<String>,
    /// 给了就整条换（含认证）；不给 = 不动（前端不回显密码，留空即保留）。
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub max_keys: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub max_concurrency: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::super::double_option")]
    pub note: Option<Option<String>>,
    #[serde(default)]
    pub status: Option<i16>,
}

/// PATCH /admin/proxies/{id}。换地址或重新启用会清熔断。
pub async fn update_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(req): ExtractJson<PatchProxyReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_proxy(&state, id, &actor, scope).await?;
    let name = req.name.as_deref().map(ensure_name).transpose()?;
    let url = req
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(parse_url)
        .transpose()?;
    ensure_max_keys(req.max_keys.flatten())?;
    ensure_max_concurrency(req.max_concurrency.flatten())?;
    ensure_status(req.status)?;
    let note = match &req.note {
        Some(note) => Some(ensure_note(note.as_deref())?),
        None => None,
    };
    let report = egress::update_proxy(
        &state.pg,
        id,
        &egress::ProxyPatch {
            name,
            url: url
                .as_ref()
                .map(|(raw, endpoint)| (raw.as_str(), store_endpoint(endpoint))),
            max_keys: req.max_keys,
            max_concurrency: req.max_concurrency,
            note,
            status: req.status,
        },
        state.master_key.as_deref(),
    )
    .await?
    .ok_or_else(not_found)?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "proxy.update",
        &id.to_string(),
        json!({
            "name": name,
            "url": url.as_ref().map(|(_, endpoint)| endpoint.masked()),
            "max_keys": req.max_keys,
            "status": req.status,
            "assignment": report,
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "assignment": report })))
}

/// DELETE /admin/proxies/{id}。被渠道直接绑定 / 是全局默认 → 409；固定分配到它的 key 改分。
pub async fn delete_proxy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_proxy(&state, id, &actor, scope).await?;
    if !egress::delete_proxy(&state.pg, id).await? {
        return Err(not_found());
    }
    state.invalidate_routing_caches();
    audit(&state, &actor, "proxy.delete", &id.to_string(), json!({})).await;
    Ok(Json(json!({ "ok": true })))
}
