//! 出口代理管理面（IMPLEMENTATION §11.41）：代理 / 代理组 CRUD、测试、固定分配手动调整、
//! 全局默认出口、渠道出口绑定。
//!
//! 约定同渠道：读 `channel.read`、写 `channel.write`，均继承 own/all 属主范围——own 范围的
//! 管理员只看得见、绑得上、改得了自己的代理与组（否则能借别人的出口，还能看到别人的代理密码）。
//! 全局默认出口影响全站渠道，要求 all 范围。代理 URL 含认证信息：落库封信封，接口只回掩码。
//!
//! 本文件放属主范围与字段校验；代理 CRUD 在 proxies，测活在 probe，批量导入在 import，
//! 分组 / 绑定 / 缺省出口在 groups。

use axum::http::StatusCode;
use okapi_api::codes;
use okapi_gateway::state::AppState;
use okapi_kernel::error::AppError;
use okapi_providers::http::ProxyEndpoint;
use okapi_store::AuthedKey;
use okapi_store::auth::PermScope;
use okapi_store::egress::{self, Binding};

mod groups;
mod import;
mod probe;
mod proxies;

#[cfg(test)]
use self::groups::ensure_group_code;
pub use self::groups::{
    AssignReq, UpsertGroupReq, assign_key, delete_group, get_default, group_assignments,
    list_groups, set_channel_egress, set_default, upsert_group,
};
#[cfg(test)]
use self::import::import_line;
pub use self::import::{ImportReq, import_proxies};
#[cfg(test)]
use self::probe::scrub;
pub use self::probe::{ProbeReq, ProbeUrlReq, test_proxy, test_proxy_url};
pub use self::proxies::{
    CreateProxyReq, PatchProxyReq, TrafficQuery, create_proxy, delete_proxy, list_proxies,
    proxy_traffic, update_proxy,
};

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
