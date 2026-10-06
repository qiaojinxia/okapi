//! 出口代理管理面（IMPLEMENTATION §11.41）：代理 / 代理组 CRUD、测试、固定分配手动调整、
//! 全局默认出口、渠道出口绑定。
//!
//! 约定同渠道：读 `channel.read`、写 `channel.write`，均继承 own/all 属主范围——own 范围的
//! 管理员只看得见、绑得上、改得了自己的代理与组（否则能借别人的出口，还能看到别人的代理密码）。
//! 全局默认出口影响全站渠道，要求 all 范围。代理 URL 含认证信息：落库封信封，接口只回掩码。

use super::admin::{audit, ensure_channel_owner, guard_scoped};
use super::query::{PageQuery, Query};
use crate::gateway::error::AppError;
use crate::gateway::extract::Json as ExtractJson;
use crate::gateway::extract::Path;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use okapi_api::{codes, permissions};
use okapi_providers::http::ProxyEndpoint;
use okapi_store::AuthedKey;
use okapi_store::auth::PermScope;
use okapi_store::egress::{self, Binding, GroupMember};
use serde::Deserialize;
use serde_json::{Value, json};

fn not_found() -> AppError {
    AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
}

fn forbidden_owner() -> AppError {
    AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED).with_param("owner")
}

/// own 范围只认自己名下的资源；all 范围全可见。
fn visible(scope: PermScope, actor: &AuthedKey, owner: Option<i64>) -> bool {
    scope == PermScope::All || owner == Some(actor.actor_user_id())
}

fn owner_filter(scope: PermScope, actor: &AuthedKey) -> Option<i64> {
    match scope {
        PermScope::Own => Some(actor.actor_user_id()),
        PermScope::All | PermScope::Denied => None,
    }
}

async fn ensure_proxy(
    state: &AppState,
    id: i64,
    actor: &AuthedKey,
    scope: PermScope,
) -> Result<(), AppError> {
    let owner = egress::proxy_owner(&state.pg, id)
        .await?
        .ok_or_else(not_found)?;
    if visible(scope, actor, owner) {
        Ok(())
    } else {
        Err(forbidden_owner())
    }
}

async fn ensure_group(
    state: &AppState,
    code: &str,
    actor: &AuthedKey,
    scope: PermScope,
) -> Result<(), AppError> {
    let owner = egress::group_owner(&state.pg, code)
        .await?
        .ok_or_else(not_found)?;
    if visible(scope, actor, owner) {
        Ok(())
    } else {
        Err(forbidden_owner())
    }
}

/// 绑定目标必须存在且对操作者可见（建渠道、改绑定、批量绑定共用）。
pub(super) async fn validate_binding(
    state: &AppState,
    binding: &Binding,
    actor: &AuthedKey,
    scope: PermScope,
) -> Result<(), AppError> {
    match binding {
        Binding::Inherit | Binding::Direct => Ok(()),
        Binding::Proxy { proxy_id } => ensure_proxy(state, *proxy_id, actor, scope).await,
        Binding::Group { group_code } => ensure_group(state, group_code, actor, scope).await,
    }
}

/// 控制面按 key 解析出口（测活、拉模型、余额、换码 / 刷新）。出口不可用 → 409
/// `egress_unavailable`：绝不退回直连。
pub(super) async fn key_proxy(state: &AppState, key_id: i64) -> Result<Option<String>, AppError> {
    let resolved = egress::resolve_for_key(&state.pg, key_id, state.master_key.as_deref())
        .await?
        .ok_or_else(not_found)?;
    Ok(resolved.proxy_url()?)
}

// ---- 代理 ----

/// 代理 URL：http / https / socks5 / socks5h，不带路径；返回规范化前的原文与解析出的非密字段。
fn parse_url(raw: &str) -> Result<(String, ProxyEndpoint), AppError> {
    let url = raw.trim();
    let endpoint =
        ProxyEndpoint::parse(url).ok_or_else(|| AppError::bad_request().with_param("url"))?;
    Ok((url.to_owned(), endpoint))
}

fn store_endpoint(endpoint: &ProxyEndpoint) -> egress::Endpoint<'_> {
    egress::Endpoint {
        scheme: &endpoint.scheme,
        host: &endpoint.host,
        port: i32::from(endpoint.port),
        username: endpoint.username.as_deref(),
    }
}

fn ensure_name(name: &str) -> Result<&str, AppError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 128 {
        return Err(AppError::bad_request().with_param("name"));
    }
    Ok(name)
}

fn ensure_note(note: Option<&str>) -> Result<Option<&str>, AppError> {
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > 255) {
        return Err(AppError::bad_request().with_param("note"));
    }
    Ok(note)
}

fn ensure_max_keys(value: Option<i32>) -> Result<(), AppError> {
    if value.is_some_and(|cap| cap <= 0) {
        return Err(AppError::bad_request().with_param("max_keys"));
    }
    Ok(())
}

fn ensure_max_concurrency(value: Option<i32>) -> Result<(), AppError> {
    if value.is_some_and(|cap| cap <= 0) {
        return Err(AppError::bad_request().with_param("max_concurrency"));
    }
    Ok(())
}

fn ensure_status(value: Option<i16>) -> Result<(), AppError> {
    if value.is_some_and(|s| !matches!(s, 1 | 2)) {
        return Err(AppError::bad_request().with_param("status"));
    }
    Ok(())
}

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
    #[serde(default, deserialize_with = "super::double_option")]
    pub max_keys: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::double_option")]
    pub max_concurrency: Option<Option<i32>>,
    #[serde(default, deserialize_with = "super::double_option")]
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

async fn probe_target(state: &AppState, target: Option<&str>) -> Result<String, AppError> {
    let target = target
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(okapi_providers::egress_probe::DEFAULT_TARGET);
    // 探测地址是管理员给的：与上游地址同一道 SSRF 闸（代理自身不过闸，见 §11.30）
    super::ssrf::validate_api_base(state, target).await?;
    Ok(target.to_owned())
}

/// 错误摘要里不能带出代理密码。
fn scrub(detail: &str, proxy_url: &str) -> String {
    let mut clean = detail.replace(proxy_url, "[proxy]");
    if let Ok(url) = reqwest::Url::parse(proxy_url)
        && let Some(password) = url.password().filter(|p| !p.is_empty())
    {
        clean = clean.replace(password, "***");
    }
    clean
}

async fn run_probe(state: &AppState, proxy_url: &str, target: &str) -> (Value, ProbeOutcome) {
    match okapi_providers::egress_probe::probe(state.upstream.http(), Some(proxy_url), target).await
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

enum ProbeOutcome {
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

// ---- 批量导入 ----

const IMPORT_MAX_LINES: usize = 1000;
const SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

#[derive(Deserialize)]
pub struct ImportReq {
    /// 每行一个代理：完整 URL，或代理商常见的 `host:port`、`host:port:user:pass`、
    /// `user:pass@host:port`。空行与 `#` 开头的行跳过。
    pub text: String,
    /// 行里没写协议时用的协议（缺省 socks5h：域名由代理解析）。
    #[serde(default)]
    pub default_scheme: Option<String>,
    #[serde(default)]
    pub max_keys: Option<i32>,
    #[serde(default)]
    pub max_concurrency: Option<i32>,
    /// 名称前缀：`<前缀>-<序号>`；缺省用 `host:port`。
    #[serde(default)]
    pub name_prefix: Option<String>,
    /// 导入后追加进这个代理组（成员 priority 0 / weight 1）。
    #[serde(default)]
    pub group_code: Option<String>,
}

/// userinfo 的百分号编码（只留 RFC 3986 unreserved），`host:port:user:pass` 里的原样凭证用。
fn encode_userinfo(raw: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// 一行 → 代理 URL。带协议的原样用；`host:port:user:pass`（第二段是端口；第四段起都算密码，
/// 凭证里可以有 `@`）按原文编码凭证；其余带 `@` 的视为 `user:pass@host:port`（userinfo 按已编码处理）；
/// `host:port`。认不出 = None。
fn import_line(line: &str, scheme: &str) -> Option<String> {
    let line = line.trim();
    if line.contains("://") {
        return Some(line.to_owned());
    }
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if let [host, port, user, pass] = parts.as_slice()
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(format!(
            "{scheme}://{}:{}@{host}:{port}",
            encode_userinfo(user),
            encode_userinfo(pass)
        ));
    }
    if line.contains('@') {
        return Some(format!("{scheme}://{line}"));
    }
    match parts.as_slice() {
        [host, port] => Some(format!("{scheme}://{host}:{port}")),
        _ => None,
    }
}

/// POST /admin/proxies/import：逐行解析，与已有代理按 (协议, 主机, 端口, 用户名) 查重，
/// 一个事务里建完；可选追加进代理组。回执逐行说明建了哪条、跳过哪条及原因。
#[allow(clippy::too_many_lines)] // 校验 → 逐行解析 → 落库 → 入组的线性流程放同一视野
pub async fn import_proxies(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<ImportReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let scheme = req.default_scheme.as_deref().unwrap_or("socks5h");
    if !SCHEMES.contains(&scheme) {
        return Err(AppError::bad_request().with_param("default_scheme"));
    }
    ensure_max_keys(req.max_keys)?;
    ensure_max_concurrency(req.max_concurrency)?;
    let prefix = req
        .name_prefix
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    if prefix.is_some_and(|p| p.chars().count() > 100) {
        return Err(AppError::bad_request().with_param("name_prefix"));
    }
    let group = req
        .group_code
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty());
    if let Some(code) = group {
        ensure_group(&state, code, &actor, scope).await?;
    }
    let lines: Vec<(usize, &str)> = req
        .text
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if lines.is_empty() || lines.len() > IMPORT_MAX_LINES {
        return Err(AppError::bad_request().with_param("text"));
    }

    let mut seen = egress::endpoint_keys(&state.pg, owner_filter(scope, &actor)).await?;
    let mut accepted: Vec<(usize, String, ProxyEndpoint, String)> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for (line_no, line) in lines {
        let Some((url, endpoint)) = import_line(line, scheme)
            .and_then(|url| ProxyEndpoint::parse(&url).map(|endpoint| (url, endpoint)))
        else {
            skipped.push(json!({"line": line_no, "reason": "invalid"}));
            continue;
        };
        let key = (
            endpoint.scheme.clone(),
            endpoint.host.clone(),
            i32::from(endpoint.port),
            endpoint.username.clone(),
        );
        if !seen.insert(key) {
            skipped.push(json!({"line": line_no, "reason": "duplicate"}));
            continue;
        }
        let name: String = match prefix {
            Some(prefix) => format!("{prefix}-{}", accepted.len() + 1),
            None => format!("{}:{}", endpoint.host, endpoint.port),
        }
        .chars()
        .take(128)
        .collect();
        accepted.push((line_no, url, endpoint, name));
    }

    let ids = if accepted.is_empty() {
        Vec::new()
    } else {
        let inputs: Vec<egress::NewProxy<'_>> = accepted
            .iter()
            .map(|(_, url, endpoint, name)| egress::NewProxy {
                name: name.as_str(),
                url,
                endpoint: store_endpoint(endpoint),
                max_keys: req.max_keys,
                max_concurrency: req.max_concurrency,
                note: None,
                status: 1,
                owner_id: Some(actor.actor_user_id()),
            })
            .collect();
        egress::create_proxies(&state.pg, &inputs, state.master_key.as_deref()).await?
    };
    let report = match group {
        Some(code) if !ids.is_empty() => {
            Some(egress::add_group_members(&state.pg, code, &ids).await?)
        }
        _ => None,
    };
    let created: Vec<Value> = accepted
        .iter()
        .zip(&ids)
        .map(|((line, _, endpoint, name), id)| {
            json!({"line": line, "id": id, "name": name, "url_masked": endpoint.masked()})
        })
        .collect();
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "proxy.import",
        &format!("{}", ids.len()),
        json!({"created": ids, "skipped": skipped.len(), "group_code": group,
                "assignment": report}),
    )
    .await;
    Ok(Json(
        json!({ "created": created, "skipped": skipped, "assignment": report }),
    ))
}

// ---- 代理组 ----

const GROUP_MODES: [&str; 2] = ["pinned", "rotate"];

fn ensure_group_code(code: &str) -> Result<&str, AppError> {
    let code = code.trim();
    let ok = (1..=32).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(code)
    } else {
        Err(AppError::bad_request().with_param("code"))
    }
}

/// GET /admin/proxy-groups。
pub async fn list_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let page = egress::list_groups(&state.pg, owner_filter(scope, &actor), q.slice()).await?;
    Ok(Json(json!({ "data": page.data, "total": page.total })))
}

#[derive(Deserialize)]
pub struct UpsertGroupReq {
    pub code: String,
    #[serde(default)]
    pub name: Option<String>,
    pub mode: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub members: Vec<GroupMember>,
}

/// POST /admin/proxy-groups：新建或覆盖（成员整组替换），同事务对账固定分配。
pub async fn upsert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(req): ExtractJson<UpsertGroupReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    let code = ensure_group_code(&req.code)?;
    if !GROUP_MODES.contains(&req.mode.as_str()) {
        return Err(AppError::bad_request().with_param("mode"));
    }
    let exists = egress::group_owner(&state.pg, code).await?;
    if let Some(owner) = exists
        && !visible(scope, &actor, owner)
    {
        return Err(forbidden_owner());
    }
    let name = ensure_name(req.name.as_deref().unwrap_or(code))?;
    let description = ensure_note(req.description.as_deref())?;
    let mut seen = std::collections::HashSet::new();
    for member in &req.members {
        if !seen.insert(member.proxy_id) {
            return Err(AppError::bad_request().with_param("members"));
        }
        if member.weight <= 0 || !(-1000..=1000).contains(&member.priority) {
            return Err(AppError::bad_request().with_param("members"));
        }
        ensure_proxy(&state, member.proxy_id, &actor, scope).await?;
    }
    let report = egress::upsert_group(
        &state.pg,
        &egress::GroupInput {
            code,
            name,
            mode: &req.mode,
            description,
            owner_id: Some(actor.actor_user_id()),
            members: &req.members,
        },
    )
    .await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "proxy_group.upsert",
        code,
        json!({ "mode": req.mode, "members": req.members, "assignment": report }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "assignment": report })))
}

/// DELETE /admin/proxy-groups/{code}。被渠道直接绑定 / 是全局默认 → 409。
pub async fn delete_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_group(&state, &code, &actor, scope).await?;
    if !egress::delete_group(&state.pg, &code).await? {
        return Err(not_found());
    }
    state.invalidate_routing_caches();
    audit(&state, &actor, "proxy_group.delete", &code, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

/// GET /admin/proxy-groups/{code}/assignments：有效出口是该组的全部 key 及其分配
/// （含经全局默认继承的），供「哪个账号在哪个 IP 上」一览与手动调整。
pub async fn group_assignments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    ensure_group(&state, &code, &actor, scope).await?;
    let mut rows = egress::group_assignments(&state.pg, &code).await?;
    if scope != PermScope::All {
        // own 范围：只列自己渠道下的 key
        let mine = okapi_store::admin::list_channels(
            &state.pg,
            okapi_store::admin::ChannelFilter {
                owner: Some(actor.actor_user_id()),
                ..Default::default()
            },
            okapi_store::listing::Slice::ALL,
        )
        .await?;
        let ids: std::collections::HashSet<i64> = mine.page.data.iter().map(|c| c.id).collect();
        rows.retain(|r| ids.contains(&r.channel_id));
    }
    Ok(Json(json!({ "data": rows })))
}

#[derive(Deserialize)]
pub struct AssignReq {
    pub key_id: i64,
    pub proxy_id: i64,
}

/// POST /admin/proxy-groups/{code}/assignments：把一把 key 手动改分到组内另一个代理。
pub async fn assign_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    ExtractJson(req): ExtractJson<AssignReq>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_group(&state, &code, &actor, scope).await?;
    ensure_proxy(&state, req.proxy_id, &actor, scope).await?;
    let channel_id: i64 = sqlx::query_scalar("SELECT channel_id FROM channel_keys WHERE id = $1")
        .bind(req.key_id)
        .fetch_optional(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?
        .ok_or_else(not_found)?;
    ensure_channel_owner(&state, channel_id, &actor, scope).await?;
    egress::assign_key(&state.pg, req.key_id, req.proxy_id).await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "proxy_group.assign",
        &code,
        json!({ "key_id": req.key_id, "proxy_id": req.proxy_id }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---- 全局默认出口 ----

/// GET /admin/egress/default。
pub async fn get_default(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    guard_scoped(&state, &headers, permissions::CHANNEL_READ).await?;
    let binding = egress::default_binding(&state.pg).await?;
    Ok(Json(json!({ "egress": binding })))
}

/// PUT /admin/egress/default：继承默认的渠道随之换出口（固定分配同事务对账）。要求 all 范围。
pub async fn set_default(
    State(state): State<AppState>,
    headers: HeaderMap,
    ExtractJson(binding): ExtractJson<Binding>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    if scope != PermScope::All {
        return Err(
            AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
                .with_param("egress_default_requires_all_scope"),
        );
    }
    if binding == Binding::Inherit {
        return Err(AppError::bad_request().with_param("mode"));
    }
    validate_binding(&state, &binding, &actor, scope).await?;
    let report = egress::set_default_binding(&state.pg, &binding, actor.actor_user_id()).await?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "egress.set_default",
        egress::DEFAULT_SETTING,
        json!({ "egress": binding, "assignment": report }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "assignment": report })))
}

// ---- 渠道绑定 ----

/// POST /admin/channels/{id}/egress：`{"mode":"inherit"|"direct"|"proxy"|"group", ...}`。
pub async fn set_channel_egress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ExtractJson(binding): ExtractJson<Binding>,
) -> Result<Json<Value>, AppError> {
    let (actor, scope) = guard_scoped(&state, &headers, permissions::CHANNEL_WRITE).await?;
    ensure_channel_owner(&state, id, &actor, scope).await?;
    validate_binding(&state, &binding, &actor, scope).await?;
    let report = egress::set_channel_binding(&state.pg, id, &binding)
        .await?
        .ok_or_else(not_found)?;
    state.invalidate_routing_caches();
    audit(
        &state,
        &actor,
        "channel.set_egress",
        &id.to_string(),
        json!({ "egress": binding, "assignment": report }),
    )
    .await;
    Ok(Json(json!({ "ok": true, "assignment": report })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_codes_are_short_identifiers() {
        for ok in ["hk", "jp-1", "us_east.2", &"a".repeat(32)] {
            assert!(ensure_group_code(ok).is_ok(), "{ok}");
        }
        for bad in ["", "has space", "中文", &"a".repeat(33), "a/b"] {
            assert!(ensure_group_code(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn import_lines_accept_vendor_formats() {
        let parse = |line: &str| {
            import_line(line, "socks5h")
                .and_then(|url| ProxyEndpoint::parse(&url))
                .map(|e| (e.scheme, e.host, e.port, e.username, e.has_password))
        };
        assert_eq!(
            parse("http://u:p@1.2.3.4:8080"),
            Some((
                "http".into(),
                "1.2.3.4".into(),
                8080,
                Some("u".into()),
                true
            ))
        );
        assert_eq!(
            parse("1.2.3.4:1080"),
            Some(("socks5h".into(), "1.2.3.4".into(), 1080, None, false))
        );
        // 代理商格式：密码里的 @ : / 都要编码，第四段起整体算密码
        let url = import_line("gate.example.com:7000:user-zone-us:p@ss:w/rd", "http").unwrap();
        assert_eq!(
            url,
            "http://user-zone-us:p%40ss%3Aw%2Frd@gate.example.com:7000"
        );
        assert_eq!(
            parse("user:pass@10.0.0.1:3128"),
            Some((
                "socks5h".into(),
                "10.0.0.1".into(),
                3128,
                Some("user".into()),
                true
            ))
        );
        // 用户名本身带 @（邮箱式账号）也按冒号格式认
        assert_eq!(
            import_line("h.example.com:9000:me@corp.com:pw", "http").unwrap(),
            "http://me%40corp.com:pw@h.example.com:9000"
        );
        for bad in ["just-a-host", "1.2.3.4:notaport", "a:b:c", "ftp://x:21"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn probe_errors_never_echo_the_proxy_password() {
        let url = "http://user:s3cret@10.0.0.1:8080";
        let detail = format!("error trying to connect: {url}: auth s3cret rejected");
        let clean = scrub(&detail, url);
        assert!(!clean.contains("s3cret"), "{clean}");
        assert!(clean.contains("[proxy]"));
    }
}
