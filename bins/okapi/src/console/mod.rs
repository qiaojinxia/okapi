//! console 角色：控制面（IMPLEMENTATION §2.1）。
//!
//! M2 第一批：管理鉴权（role ≥ admin）、渠道/模型管理、定价 epoch 发布、
//! 用户入账、对账查询；写路径全量审计。细粒度权限点（admin_roles）、
//! 用户门户 API 与 MCP 端点在后续批次接入。

pub mod admin;
pub mod auth_web;
pub mod mcp;
pub mod oauth;
pub mod pay;
pub mod portal;
pub mod setup;
pub mod ssrf;
pub mod stats;
pub mod teams;

use crate::config::Config;
use crate::gateway::{self, state::AppState};
use axum::Router;
use axum::extract::Request;
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let state = gateway::build_state(
        &cfg.database_url,
        &cfg.redis_url,
        &cfg.node,
        cfg.clickhouse_url.as_deref(),
        cfg.nats_url.as_deref(),
    )
    .await?;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(cfg.console_bind).await?;
    tracing::info!(bind = %cfg.console_bind, "okapi console 启动");
    // with_connect_info：关键接口限流在 CDN 头缺省时回退直连 socket IP
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

/// 组装路由（集成测试直接复用）。
pub fn router(state: AppState) -> Router {
    Router::new()
        .route(
            "/admin/channels",
            post(admin::create_channel).get(admin::list_channels),
        )
        .route(
            "/admin/channels/{id}/status",
            post(admin::set_channel_status),
        )
        .route(
            "/admin/channels/{id}/groups",
            post(admin::set_channel_groups),
        )
        .route("/admin/channels/{id}/test", post(admin::test_channel))
        .route(
            "/admin/channels/{id}/fetch-models",
            get(admin::fetch_channel_models),
        )
        .route("/admin/models", post(admin::upsert_model))
        .route("/admin/groups", post(admin::upsert_group))
        .route("/admin/users/{id}/groups", post(admin::set_user_groups))
        .route("/admin/settings", post(admin::set_setting))
        .route("/admin/settings/{key}", get(admin::get_setting))
        .route("/admin/leaderboard", get(admin::leaderboard))
        .route("/admin/stats/channels", get(stats::channels))
        .route("/admin/stats/models", get(stats::models))
        .route("/admin/stats/margin", get(stats::margin))
        .route("/admin/pricing/publish", post(admin::publish_pricing))
        .route(
            "/admin/pricing/import-newapi",
            post(admin::import_newapi_pricing),
        )
        .route(
            "/admin/pricing/rules",
            post(admin::upsert_pricing_rule).get(admin::list_pricing_rules),
        )
        .route(
            "/admin/pricing/rules/{rule_code}",
            axum::routing::delete(admin::delete_pricing_rule),
        )
        .route("/admin/users/{id}/credit", post(admin::credit_user))
        .route(
            "/admin/users/{id}/balance-expiry",
            post(admin::set_balance_expiry),
        )
        .route("/admin/users", get(admin::list_users))
        .route(
            "/admin/roles",
            post(admin::create_role).get(admin::list_roles),
        )
        .route("/admin/users/{id}/role", post(admin::assign_role))
        .route("/admin/users/{id}/overview", get(admin::user_overview))
        .route("/admin/billing/refund", post(admin::refund_by_request))
        .route("/admin/redemptions", post(admin::create_redemptions))
        .route("/admin/plans", post(admin::upsert_plan))
        .route("/admin/cache/flush", post(admin::cache_flush))
        .route("/admin/reconciliation", get(admin::reconciliation))
        .route("/api/me", get(portal::me))
        .route("/api/me/usage", get(portal::usage))
        .route("/api/me/keys", get(portal::keys))
        .route("/api/me/logs", get(portal::logs))
        .route("/api/me/redeem", post(portal::redeem))
        .route("/api/me/aff", get(portal::aff))
        .route("/api/me/topup", post(pay::topup))
        .route("/api/teams", post(teams::create_team))
        .route("/api/teams/{id}/members", post(teams::upsert_member))
        .route("/api/teams/{id}/keys", post(teams::create_team_key))
        .route("/api/teams/{id}/usage", get(teams::team_usage))
        .route("/pay/callback/epay", get(pay::epay_callback))
        .route("/pay/callback/stripe", post(pay::stripe_webhook))
        .route("/api/pricing", get(portal::public_pricing))
        .route("/api/setup/status", get(setup::status))
        .route("/api/setup", post(setup::run))
        .route("/auth/register", post(auth_web::register))
        .route("/auth/login", post(auth_web::login))
        .route("/auth/logout", post(auth_web::logout))
        .route("/auth/totp/enroll", post(auth_web::totp_enroll))
        .route("/auth/totp/confirm", post(auth_web::totp_confirm))
        .route("/auth/keys", post(auth_web::create_key))
        .route("/auth/oauth-providers", get(oauth::list_providers))
        .route("/auth/oauth/{provider}", get(oauth::start))
        .route("/auth/oauth/{provider}/callback", get(oauth::callback))
        .route("/mcp", post(mcp::endpoint))
        .route("/healthz", get(|| async { "ok" }))
        .merge(spa_router())
        .layer(axum::middleware::from_fn(spa_navigation))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// SPA 与管理 API 同挂 `/admin/*`，路径同名时（如 `/admin/channels`）axum 的路由
/// 匹配优先于 fallback——浏览器直接访问或刷新会拿到 API 的 401/405 而不是应用。
///
/// 故对**浏览器顶层导航**（GET + `Accept: text/html`）先返回 index.html 交给前端路由，
/// 而 `fetch`/curl 等（`Accept: */*` 或 `application/json`）完全不受影响，API 语义不变。
/// 豁免必须由服务端处理的浏览器导航：OAuth 起跳/回调、支付回跳，以及探针与 MCP。
async fn spa_navigation(req: Request, next: Next) -> Response {
    const SERVER_HANDLED: [&str; 4] = ["/auth/", "/pay/", "/mcp", "/healthz"];

    let is_navigation = req.method() == Method::GET
        && req
            .headers()
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"));
    let path = req.uri().path();
    // 末段带扩展名视为静态资源（浏览器请求资源不会声明 text/html，此处仅兜底手工访问）
    let is_asset = path.rsplit('/').next().is_some_and(|seg| seg.contains('.'));

    if is_navigation
        && !is_asset
        && !SERVER_HANDLED.iter().any(|p| path.starts_with(p))
        && let Some(html) = spa_index_bytes().await
    {
        return ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    }
    next.run(req).await
}

/// 导航兜底用的 index.html，与 `spa_router` 取同一份产物。
#[cfg(not(feature = "embed-web"))]
async fn spa_index_bytes() -> Option<Vec<u8>> {
    let dir = std::env::var("OKAPI_WEB_DIR").unwrap_or_else(|_| "frontend/dist".to_owned());
    tokio::fs::read(format!("{dir}/index.html")).await.ok()
}

#[cfg(feature = "embed-web")]
async fn spa_index_bytes() -> Option<Vec<u8>> {
    WebAssets::get("index.html").map(|file| file.data.to_vec())
}

/// 前端 SPA 静态托管：
/// - `embed-web` feature（发布形态）：dist 编译期嵌入二进制（rust-embed）；
/// - 缺省（开发形态）：磁盘目录（OKAPI_WEB_DIR 缺省 ./frontend/dist）。
#[cfg(not(feature = "embed-web"))]
fn spa_router() -> Router<crate::gateway::state::AppState> {
    let dir = std::env::var("OKAPI_WEB_DIR").unwrap_or_else(|_| "frontend/dist".to_owned());
    let index = format!("{dir}/index.html");
    let service = tower_http::services::ServeDir::new(&dir)
        .fallback(tower_http::services::ServeFile::new(index));
    Router::new().fallback_service(service)
}

#[cfg(feature = "embed-web")]
#[derive(rust_embed::Embed)]
#[folder = "$CARGO_MANIFEST_DIR/../../frontend/dist/"]
struct WebAssets;

#[cfg(feature = "embed-web")]
fn spa_router() -> Router<crate::gateway::state::AppState> {
    use axum::http::{HeaderValue, StatusCode, Uri, header};
    use axum::response::IntoResponse;

    async fn serve(uri: Uri) -> axum::response::Response {
        let path = uri.path().trim_start_matches('/');
        let candidate = if path.is_empty() { "index.html" } else { path };
        // 静态资源直出；未命中回退 index.html（SPA 路由）
        let (name, file) = match WebAssets::get(candidate) {
            Some(file) => (candidate, file),
            None => match WebAssets::get("index.html") {
                Some(file) => ("index.html", file),
                None => return StatusCode::NOT_FOUND.into_response(),
            },
        };
        let mime = mime_guess::from_path(name).first_or_octet_stream();
        let mut resp = file.data.into_response();
        if let Ok(value) = HeaderValue::from_str(mime.as_ref()) {
            resp.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        resp
    }
    Router::new().fallback(serve)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("console 收到退出信号");
}
