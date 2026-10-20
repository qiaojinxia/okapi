//! 审计兜底（对照 new-api `middleware/audit.go`）：管理端写接口成功、处理器却没写审计时，补一条
//! 通用记录，新接口不再全靠开发者记得调 `admin::audit`。
//!
//! 权限守卫记下操作者与它查的权限点，`admin::audit` 等写审计的地方记下「已写过」；请求结束后
//! 仍未写过、且守卫查的不是只读权限（`*.read`，那些 POST 是查询）时，补写动作 `api.<方法>`、
//! 对象为路由模板的一行。只看 `/admin/` 下的 POST / PUT / PATCH / DELETE。
use crate::gateway::state::AppState;
use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use serde_json::json;
use std::cell::RefCell;

#[derive(Default)]
struct Pending {
    actor: Option<i64>,
    writes: bool,
    audited: bool,
}

tokio::task_local! {
    static PENDING: RefCell<Pending>;
}

/// 权限守卫通过时调用：记下操作者，以及这次查的是不是写类权限。
pub(super) fn note_actor(actor_user_id: i64, permission: &str) {
    let _ = PENDING.try_with(|pending| {
        let mut pending = pending.borrow_mut();
        pending.actor = Some(actor_user_id);
        pending.writes |= !matches!(permission.rsplit_once('.'), Some((_, "read")));
    });
}

/// 处理器自己写了审计：不再补。
pub(super) fn note_audited() {
    let _ = PENDING.try_with(|pending| pending.borrow_mut().audited = true);
}

pub(super) async fn layer(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let write_method = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|route| route.as_str().to_owned())
        .filter(|route| write_method && route.starts_with("/admin/"));
    let Some(route) = route else {
        return next.run(request).await;
    };
    let method = request.method().as_str().to_ascii_lowercase();
    let path = request.uri().path().to_owned();
    let (response, pending) = PENDING
        .scope(RefCell::new(Pending::default()), async move {
            let response = next.run(request).await;
            (response, PENDING.with(RefCell::take))
        })
        .await;
    if response.status().is_success()
        && pending.writes
        && !pending.audited
        && let Some(actor) = pending.actor
        && let Err(err) = okapi_store::admin::record_audit(
            &state.pg,
            &format!("admin:{actor}"),
            &format!("api.{method}"),
            &route,
            json!({"path": path, "status": response.status().as_u16()}),
        )
        .await
    {
        tracing::error!(error = %err, route, "兜底审计写入失败");
    }
    response
}
