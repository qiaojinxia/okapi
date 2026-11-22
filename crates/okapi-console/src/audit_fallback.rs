//! 审计请求上下文与兜底中间件：
//!
//! 1. 为全部 console API 请求建立 task-local 请求上下文（来源 IP、User-Agent、W3C `trace_id`、
//!    当前凭证 `actor_key_id`），使所有显式与兜底审计都能自动写入 `audit_logs.ip / user_agent / trace_id / actor_key_id`。
//! 2. 兜底写审计（对照 new-api `middleware/audit.go`）：`/admin/` 下写接口成功、处理器却没写审计时，
//!    补一条 `api.<方法>`（对象为路由模板）的通用记录。
//! 3. 越权留痕：`/admin/` 下权限守卫拒绝（403）且处理器未另写审计时，自动记录 `auth.permission_denied`，
//!    便于发现越权探测与角色误配。
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use okapi_engine::state::AppState;
use okapi_store::admin::{AUDIT_UA_MAX_CHARS, AuditMeta, truncate_chars};
use serde_json::json;
use std::cell::RefCell;
use std::net::{IpAddr, SocketAddr};

#[derive(Clone)]
struct DeniedAccess {
    actor_user_id: i64,
    actor_role: i16,
    actor_key_id: i64,
    permission: String,
}

#[derive(Default)]
struct Pending {
    ip: Option<IpAddr>,
    user_agent: Option<String>,
    trace_id: Option<String>,
    actor: Option<i64>,
    actor_key_id: Option<i64>,
    writes: bool,
    audited: bool,
    denied: Option<DeniedAccess>,
}

impl Pending {
    fn to_meta(&self, actor_key_id: Option<i64>) -> AuditMeta {
        AuditMeta {
            ip: self.ip,
            user_agent: self.user_agent.clone(),
            trace_id: self.trace_id.clone(),
            actor_key_id: actor_key_id.or(self.actor_key_id),
        }
    }
}

tokio::task_local! {
    static PENDING: RefCell<Pending>;
}

/// 读取当前 HTTP 请求的审计元数据（若不在请求上下文中则返回空元数据）。
pub(crate) fn current_meta(actor_key_id: Option<i64>) -> AuditMeta {
    PENDING
        .try_with(|pending| pending.borrow().to_meta(actor_key_id))
        .unwrap_or_else(|_| AuditMeta {
            actor_key_id,
            ..AuditMeta::default()
        })
}

/// 权限守卫通过时调用：记下操作者、凭证 key_id，以及这次查的是不是写类权限。
pub(crate) fn note_actor(actor_user_id: i64, actor_key_id: i64, permission: &str) {
    let _ = PENDING.try_with(|pending| {
        let mut pending = pending.borrow_mut();
        pending.actor = Some(actor_user_id);
        pending.actor_key_id = Some(actor_key_id);
        pending.writes |= !matches!(permission.rsplit_once('.'), Some((_, "read")));
    });
}

/// 权限守卫拒绝（403）时调用：记录越权尝试上下文，供中间件落盘 `auth.permission_denied`。
pub(crate) fn note_denied(
    actor_user_id: i64,
    actor_role: i16,
    actor_key_id: i64,
    permission: &str,
) {
    let _ = PENDING.try_with(|pending| {
        let mut pending = pending.borrow_mut();
        pending.actor = Some(actor_user_id);
        pending.actor_key_id = Some(actor_key_id);
        pending.denied = Some(DeniedAccess {
            actor_user_id,
            actor_role,
            actor_key_id,
            permission: permission.to_owned(),
        });
    });
}

/// 处理器自己写了审计：不再补。
pub(crate) fn note_audited() {
    let _ = PENDING.try_with(|pending| pending.borrow_mut().audited = true);
}

fn extract_request_meta(request: &Request) -> (Option<IpAddr>, Option<String>, Option<String>) {
    let ip = okapi_kernel::clients::client_ip(request.headers()).or_else(|| {
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
    });
    let user_agent = request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, AUDIT_UA_MAX_CHARS).to_owned());
    let trace_id = request
        .extensions()
        .get::<okapi_kernel::trace_context::TraceContext>()
        .map(|t| t.trace_id.clone());
    (ip, user_agent, trace_id)
}

pub(super) async fn layer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let (ip, user_agent, trace_id) = extract_request_meta(&request);
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path().to_owned(), |route| route.as_str().to_owned());
    let method = request.method().as_str().to_ascii_lowercase();
    let path = request.uri().path().to_owned();
    let is_admin_route = route.starts_with("/admin/");
    let is_admin_write = is_admin_route
        && matches!(
            *request.method(),
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        );

    let initial = Pending {
        ip,
        user_agent,
        trace_id,
        ..Pending::default()
    };
    let (response, pending) = PENDING
        .scope(RefCell::new(initial), async move {
            let response = next.run(request).await;
            (response, PENDING.with(RefCell::take))
        })
        .await;

    if is_admin_write
        && response.status().is_success()
        && pending.writes
        && !pending.audited
        && let Some(actor) = pending.actor
    {
        let meta = pending.to_meta(pending.actor_key_id);
        if let Err(err) = okapi_store::admin::record_audit_with_meta(
            &state.pg,
            &format!("admin:{actor}"),
            &format!("api.{method}"),
            &route,
            json!({"path": path, "status": response.status().as_u16()}),
            &meta,
        )
        .await
        {
            tracing::error!(error = %err, route, "兜底审计写入失败");
        }
    } else if is_admin_route
        && response.status() == StatusCode::FORBIDDEN
        && !pending.audited
        && let Some(denied) = &pending.denied
    {
        let prefix = if denied.actor_role >= 10 {
            "admin"
        } else {
            "user"
        };
        let meta = pending.to_meta(Some(denied.actor_key_id));
        if let Err(err) = okapi_store::admin::record_audit_with_meta(
            &state.pg,
            &format!("{prefix}:{}", denied.actor_user_id),
            "auth.permission_denied",
            &route,
            json!({
                "permission": denied.permission,
                "method": method,
                "path": path,
                "status": 403,
            }),
            &meta,
        )
        .await
        {
            tracing::error!(error = %err, route, "越权拒绝审计写入失败");
        }
    }

    response
}
