//! 控制面鉴权与同源 CSRF 防护：
//! - 携带 API key 头（`Authorization: Bearer` / `x-api-key` / `x-goog-api-key`）时沿用内核鉴权；
//! - 仅携带 `HttpOnly` `okapi_session` Cookie 时直接从 Redis 会话与 PG 加载用户身份，不再要求
//!   前端把持久化 API key 存入 `localStorage`；
//! - 对纯 Cookie 鉴权的状态变更请求（`POST`/`PUT`/`PATCH`/`DELETE`）校验 `Sec-Fetch-Site` 与
//!   `Origin` 同源性，阻断跨站请求伪造（CSRF）。

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use okapi_api::codes;
use okapi_engine::state::AppState;
use okapi_kernel::auth::session_id;
use okapi_kernel::error::AppError;
use okapi_store::AuthedKey;
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn has_api_key_header(headers: &HeaderMap) -> bool {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .is_some_and(|v| !v.is_empty());
    bearer
        || headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().is_empty())
        || headers
            .get("x-goog-api-key")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().is_empty())
}

/// 控制面统一鉴权：优先走显式 API key；未带 key 时接受有效的 `HttpOnly` `okapi_session` Cookie。
pub(crate) async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<AuthedKey>, AppError> {
    if has_api_key_header(headers) {
        return okapi_kernel::auth::authenticate(state, headers).await;
    }
    let Some(sid) = session_id(headers) else {
        return okapi_kernel::auth::authenticate(state, headers).await;
    };
    let user_id = super::auth_web::session_user(state, &sid).await?;
    let hash = hex::encode(Sha256::digest(sid.as_bytes()));
    let cache_key = format!("sess:{hash}");
    let authed = if let Some(hit) = state.sched.auth_get(&cache_key).await {
        okapi_kernel::metrics::global().record_auth_cache(true);
        Arc::new(hit)
    } else {
        okapi_kernel::metrics::global().record_auth_cache(false);
        let mut resolved = None;
        for _ in 0..3 {
            let version = state.sched.auth_version().await;
            let found =
                okapi_store::auth::find_user_session_actor(&state.pg, user_id, hash.clone())
                    .await?;
            let Some(found) = found else {
                return Err(AppError::unauthorized(codes::SESSION_EXPIRED));
            };
            if let Some(version) = version {
                if state.sched.auth_version().await != Some(version) {
                    continue;
                }
                state.sched.auth_set(&cache_key, &found, version).await;
            }
            resolved = Some(Arc::new(found));
            break;
        }
        resolved.ok_or_else(|| AppError::new(StatusCode::SERVICE_UNAVAILABLE, codes::OVERLOADED))?
    };
    if authed.user_status != 1 {
        return Err(AppError::unauthorized(codes::KEY_DISABLED));
    }
    Ok(authed)
}

/// 浏览器 Cookie 写操作同源检查（CSRF 防护）。
///
/// 仅在状态变更方法（`POST`/`PUT`/`PATCH`/`DELETE`）且请求以 `okapi_session` Cookie 鉴权
/// （未携带 API key 头）时生效：
/// - 拒绝 `Sec-Fetch-Site: cross-site`；
/// - 若存在 `Origin` 头，拒绝与当前 `Host` 不一致的跨源请求。
pub(crate) async fn csrf_guard(request: Request<Body>, next: Next) -> Response {
    if is_csrf_violation(request.method(), request.uri().path(), request.headers()) {
        return AppError::new(StatusCode::FORBIDDEN, codes::PERMISSION_DENIED)
            .with_param("csrf_origin")
            .into_response();
    }
    next.run(request).await
}

fn is_csrf_violation(method: &Method, path: &str, headers: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    // 公开第三方支付回调不走浏览器会话鉴权
    if path.starts_with("/pay/callback/") {
        return false;
    }
    if has_api_key_header(headers) || session_id(headers).is_none() {
        return false;
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|site| site.eq_ignore_ascii_case("cross-site"))
    {
        return true;
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
            return true;
        };
        if !origin_matches_host(origin, host) {
            return true;
        }
    }
    false
}

fn origin_matches_host(origin: &str, host: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(origin) else {
        return false;
    };
    let Some(origin_host) = parsed.host_str() else {
        return false;
    };
    let origin_authority = match parsed.port() {
        Some(port) => format!("{origin_host}:{port}"),
        None => origin_host.to_owned(),
    };
    let normalized_host = host.trim();
    if origin_authority.eq_ignore_ascii_case(normalized_host) {
        return true;
    }
    // 处理 Host 显式带默认端口（:80 / :443）而 Url::port() 省略默认端口的情况
    match (parsed.scheme(), parsed.port()) {
        ("http", None) => format!("{origin_host}:80").eq_ignore_ascii_case(normalized_host),
        ("https", None) => format!("{origin_host}:443").eq_ignore_ascii_case(normalized_host),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn csrf_guard_blocks_cross_site_cookie_mutations_and_allows_same_origin() {
        let cross_fetch = headers_with(&[
            ("cookie", "okapi_session=abc"),
            ("sec-fetch-site", "cross-site"),
            ("host", "api.example.com"),
        ]);
        assert!(is_csrf_violation(
            &Method::POST,
            "/api/me/keys",
            &cross_fetch
        ));
        assert!(!is_csrf_violation(
            &Method::GET,
            "/api/me/keys",
            &cross_fetch
        ));

        let cross_origin = headers_with(&[
            ("cookie", "okapi_session=abc"),
            ("origin", "https://evil.example"),
            ("host", "api.example.com"),
        ]);
        assert!(is_csrf_violation(
            &Method::DELETE,
            "/api/me/keys/1",
            &cross_origin
        ));

        let same_origin = headers_with(&[
            ("cookie", "okapi_session=abc"),
            ("sec-fetch-site", "same-origin"),
            ("origin", "https://api.example.com"),
            ("host", "api.example.com"),
        ]);
        assert!(!is_csrf_violation(
            &Method::POST,
            "/api/me/keys",
            &same_origin
        ));

        let same_default_port = headers_with(&[
            ("cookie", "okapi_session=abc"),
            ("origin", "https://api.example.com"),
            ("host", "api.example.com:443"),
        ]);
        assert!(!is_csrf_violation(
            &Method::PATCH,
            "/api/me/keys/1",
            &same_default_port
        ));

        let with_bearer = headers_with(&[
            ("cookie", "okapi_session=abc"),
            ("authorization", "Bearer sk-okapi-test"),
            ("origin", "https://evil.example"),
            ("host", "api.example.com"),
        ]);
        assert!(!is_csrf_violation(
            &Method::POST,
            "/api/me/keys",
            &with_bearer
        ));

        let webhook = headers_with(&[
            ("cookie", "okapi_session=abc"),
            ("origin", "https://stripe.com"),
            ("host", "api.example.com"),
        ]);
        assert!(!is_csrf_violation(
            &Method::POST,
            "/pay/callback/stripe",
            &webhook
        ));
    }
}
