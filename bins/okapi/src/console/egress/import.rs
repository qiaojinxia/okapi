//! 批量导入代理（每行一个 URL）。
use super::super::admin::{audit, guard_scoped};
use super::{ensure_group, ensure_max_concurrency, ensure_max_keys, owner_filter, store_endpoint};
use crate::gateway::state::AppState;
use crate::kernel::error::AppError;
use crate::kernel::extract::Json as ExtractJson;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use okapi_api::permissions;
use okapi_providers::http::ProxyEndpoint;
use okapi_store::egress;
use serde::Deserialize;
use serde_json::{Value, json};

// ---- 批量导入 ----

pub(super) const IMPORT_MAX_LINES: usize = 1000;
pub(super) const SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];

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
pub(super) fn encode_userinfo(raw: &str) -> String {
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
pub(super) fn import_line(line: &str, scheme: &str) -> Option<String> {
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
