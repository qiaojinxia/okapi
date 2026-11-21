//! 请求策略（`settings.request_policy`）在一次请求里的取用：中间件把此刻生效的策略放进任务本地，
//! 重试决策（`upstream_failure`）与尝试次数上限在需要时取，不必把策略一路当参数传过十几个入口。
//!
//! 没有作用域的地方（测试、worker、控制台进程内直接调数据面处理器的试用台）取到的是缺省策略——也就是现行行为。
use super::state::AppState;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use okapi_kernel::policies::RequestPolicy;
use std::future::Future;
use std::sync::{Arc, LazyLock};

tokio::task_local! { static CURRENT: Arc<RequestPolicy>; }

static DEFAULT: LazyLock<Arc<RequestPolicy>> = LazyLock::new(|| Arc::new(RequestPolicy::default()));

/// 当前请求生效的策略；不在作用域里就是缺省。
#[must_use]
pub fn current() -> Arc<RequestPolicy> {
    CURRENT
        .try_with(Arc::clone)
        .unwrap_or_else(|_| Arc::clone(&DEFAULT))
}

/// 一个请求最多试几个渠道 / key。
#[must_use]
pub fn max_attempts() -> usize {
    usize::from(current().max_attempts)
}

/// 在这个策略下跑一段异步代码（中间件与测试用）。
pub async fn with<T>(policy: Arc<RequestPolicy>, future: impl Future<Output = T>) -> T {
    CURRENT.scope(policy, future).await
}

/// 数据面路由的中间件：每个请求开头取一次策略（10s 进程缓存），整个请求期间有效。
pub async fn scope(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let policy = state.request_policy().await;
    with(policy, next.run(request)).await
}
