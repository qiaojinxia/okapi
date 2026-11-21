//! gateway 角色：数据面（鉴权、限流、预扣/结算、SSE 透传）。
//!
//! 共享运行时底座（`AppState`、凭证/账号控制、选路缓存、结算重试、异步媒体执行器）已下沉至
//! `okapi-engine` 并在此原路径再导出；本 crate 只保留数据面 HTTP/WS 协议处理器与路由表。

pub mod audio;
pub mod bootstrap;
pub mod chat;
pub mod custom_pass;
pub mod dashboard;
pub mod embeddings;
pub mod estimate;
pub mod models;
pub mod realtime;
pub(crate) mod reservation;
pub mod routing_prefs;
pub mod token_count;

pub use okapi_engine::{
    SETTLE_DRAIN_CAP, account_control, auth, bridge, build_ch_optional, channel_balance,
    channel_probe, channel_test, connect_nats_optional, credentials, diagnostics, dialect,
    flow_control_for_pool, images, ingress, oauth_cred, openai_dialect, refresh_pricebook_if_newer,
    request_policy, rule_inputs, scheduler, settlement_retry, spawn_epoch_poller,
    spawn_epoch_subscriber, spawn_routing_invalidate_subscriber, spawn_ssrf_policy_sync,
    spawn_state_sync, state, stream_policy, sync_ssrf_policy, user_turns, videos,
};
pub(crate) use okapi_engine::{
    admission, candidates, execution_plan, extensions, failure, key_gate, key_health, server_tools,
    upstream_cost, upstream_failure,
};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use okapi_kernel::config::Config;
use okapi_kernel::error::AppError;
use okapi_providers::model_parameters::ParameterProfile;
use state::AppState;
use std::sync::Arc;
use std::time::Duration;
use tower_http::classify::ServerErrorsFailureClass;
use tower_http::trace::TraceLayer;

struct GatewayDataPlane;

#[async_trait::async_trait]
impl bridge::DataPlaneBridge for GatewayDataPlane {
    async fn chat_completions(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response {
        chat::chat_completions(State(state), headers, body).await
    }

    async fn responses(&self, state: AppState, headers: HeaderMap, body: Bytes) -> Response {
        chat::responses(State(state), headers, body).await
    }

    async fn playground_parameters(
        &self,
        state: &AppState,
        headers: &HeaderMap,
        model: &str,
    ) -> Result<ParameterProfile, AppError> {
        chat::playground_parameters(state, headers, model).await
    }
}

/// 构造指向本 crate 数据面处理器的进程内桥接器（供 console 的 Playground 与渠道试用调用）。
#[must_use]
pub fn data_plane_bridge() -> Arc<dyn bridge::DataPlaneBridge> {
    Arc::new(GatewayDataPlane)
}

/// 装配带数据面桥接器的共享状态（bin 启动与集成测试共用）。
pub async fn build_state(
    database_url: &str,
    redis_url: &str,
    node: &str,
    clickhouse_url: Option<&str>,
    nats_url: Option<&str>,
) -> anyhow::Result<AppState> {
    estimate::warm_up();
    Ok(
        okapi_engine::build_state(database_url, redis_url, node, clickhouse_url, nats_url)
            .await?
            .with_data_plane(data_plane_bridge()),
    )
}

/// Assemble services around shared connections with the in-process data-plane bridge attached.
pub async fn build_state_with_resources(
    pg: sqlx::PgPool,
    redis: fred::clients::Client,
    node: &str,
    clickhouse_url: Option<&str>,
    nats_url: Option<&str>,
) -> anyhow::Result<AppState> {
    estimate::warm_up();
    Ok(
        okapi_engine::build_state_with_resources(pg, redis, node, clickhouse_url, nats_url)
            .await?
            .with_data_plane(data_plane_bridge()),
    )
}

/// 使用已建好的共享客户端装配带数据面桥接器的 [`AppState`]。
pub async fn build_state_with_clients(
    pg: sqlx::PgPool,
    pg_pool_max: u32,
    redis: fred::clients::Client,
    node: &str,
    ch: Option<okapi_store::ChClient>,
    nats: Option<async_nats::Client>,
) -> anyhow::Result<AppState> {
    estimate::warm_up();
    Ok(
        okapi_engine::build_state_with_clients(pg, pg_pool_max, redis, node, ch, nats)
            .await?
            .with_data_plane(data_plane_bridge()),
    )
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let state = build_state(
        &cfg.database_url,
        &cfg.redis_url,
        &cfg.node,
        cfg.clickhouse_url.as_deref(),
        cfg.nats_url.as_deref(),
    )
    .await?;
    run_with_state(cfg, state, true, okapi_kernel::shutdown::signal()).await
}

/// Run the gateway HTTP server on an already-assembled `AppState` (shared in `okapi all`).
pub async fn run_with_state<F>(
    cfg: Config,
    state: AppState,
    spawn_sync: bool,
    stop: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    // §6.5 生产护栏：release 构建启用单用户模式须显式二次确认
    if cfg.single_user_mode
        && !cfg!(debug_assertions)
        && okapi_store::env_config::flag("OKAPI_SINGLE_USER_CONFIRM") != Some(true)
    {
        anyhow::bail!(
            "single_user_mode 在 release 构建需同时设 OKAPI_SINGLE_USER_CONFIRM=true（root key 会进日志，误开公网代价高）"
        );
    }
    if cfg.single_user_mode {
        bootstrap::ensure_single_user(&state).await?;
    }
    if spawn_sync {
        spawn_state_sync(&state);
    }

    let settlements = state.settlements.clone();
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, "okapi gateway 启动");
    // with_connect_info：直连（无 CDN 头）时 key 级 IP 白名单与 client_ip 列都要用到对端地址
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(stop)
    .await?;
    // 连接排完 ≠ 账已落：流式/非流式 chat 的结算在响应之后的后台任务里，
    // 这里等它们归零再退出，否则最后一批请求只剩 Redis 预扣、要靠对账才能收口
    // Realtime 会话在升级后脱离了 HTTP 排水：通知它们收尾，结算同样计入下面的等待
    settlements.drain();
    settlements.wait_idle(SETTLE_DRAIN_CAP).await;
    tracing::info!("okapi gateway 已下线");
    Ok(())
}

fn log_response_failure(class: &ServerErrorsFailureClass, latency: Duration) {
    // 503 有独立的过载/渠道信号，逐请求 ERROR 会在过载时放大日志压力。
    if matches!(class, ServerErrorsFailureClass::StatusCode(s)
        if *s == axum::http::StatusCode::SERVICE_UNAVAILABLE)
    {
        return;
    }
    tracing::error!(classification = %class, latency = ?latency, "response failed");
}

/// 组装路由（集成测试直接复用）。
/// 请求体上限 32MB（网关不解压请求体，即为有效字节上限；防超大体/zip bomb，§3.7-8）。
// Keep the route table and middleware ordering together for review.
#[allow(clippy::too_many_lines)]
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat::chat_completions))
        .route(
            "/v1/messages/count_tokens",
            post(chat::messages_count_tokens),
        )
        .route("/v1/messages", post(chat::messages))
        .route(
            "/v1/responses",
            post(chat::responses).get(chat::websocket::upgrade),
        )
        .route("/v1/responses/compact", post(chat::responses_compact))
        .route(
            "/v1/responses/input_tokens",
            post(token_count::responses_input_tokens),
        )
        // Gemini 原生入口：`{model_action}` = `gemini-2.5-pro:generateContent`（冒号在段内）
        .route("/v1beta/models/{model_action}", post(chat::gemini_generate))
        .route("/v1beta/models", get(models::list_models_gemini))
        .route("/v1/embeddings", post(embeddings::embeddings))
        .route("/v1/images/generations", post(images::images))
        .route(
            "/v1/images/batches",
            get(images::batches::list)
                .post(images::batches::create)
                .layer(axum::extract::DefaultBodyLimit::max(128 * 1024 * 1024)),
        )
        .route("/v1/images/batches/models", get(images::batches::models))
        .route(
            "/v1/images/batches/{batch_id}",
            get(images::batches::get).delete(images::batches::delete),
        )
        .route(
            "/v1/images/batches/{batch_id}/cancel",
            post(images::batches::cancel),
        )
        .route(
            "/v1/images/batches/{batch_id}/items",
            get(images::batches::items),
        )
        .route(
            "/v1/images/batches/{batch_id}/download",
            get(images::batches::download),
        )
        .route(
            "/v1/images/batches/{batch_id}/content/{slot}",
            get(images::batches::content),
        )
        .route("/v1/images/edits", post(images::edits))
        .route(
            "/v1/images/generations/async",
            post(images::tasks::generations),
        )
        .route("/v1/images/edits/async", post(images::tasks::edits))
        .route("/v1/images/tasks/{task_id}", get(images::tasks::get))
        .route(
            "/v1/images/tasks/{task_id}/cancel",
            post(images::tasks::cancel),
        )
        .route(
            "/v1/images/tasks/{task_id}/content/{index}",
            get(images::tasks::content),
        )
        .route(
            "/images/generations/async",
            post(images::tasks::generations),
        )
        .route("/images/edits/async", post(images::tasks::edits))
        .route("/images/tasks/{task_id}", get(images::tasks::get))
        .route("/v1/rerank", post(embeddings::rerank))
        .route("/v1/realtime", get(realtime::realtime))
        .route("/v1/videos", post(videos::create))
        .route("/v1/videos/{task_id}", get(videos::get_task))
        .route("/v1/videos/{task_id}/content", get(videos::get_content))
        .route("/v1/audio/speech", post(audio::speech))
        .route("/v1/audio/transcriptions", post(audio::transcriptions))
        .route("/v1/audio/translations", post(audio::translations))
        .route(
            "/v1/dashboard/billing/subscription",
            get(dashboard::subscription),
        )
        .route("/v1/dashboard/billing/usage", get(dashboard::usage))
        .route(
            "/pass/{channel_id}/{*path}",
            axum::routing::any(custom_pass::custom_pass),
        )
        .route("/v1/models", get(models::list_models))
        .route("/healthz", get(|| async { "ok" }))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            request_policy::scope,
        ))
        .layer(axum::middleware::from_fn(diagnostics::scope))
        .layer(axum::middleware::from_fn(
            okapi_kernel::security_headers::apply,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            rule_inputs::track_in_flight,
        ))
        .layer(axum::middleware::from_fn(
            okapi_kernel::clients::stamp_peer_ip,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(okapi_kernel::clients::request_span)
                .on_failure(
                    |class: ServerErrorsFailureClass, latency: Duration, _span: &tracing::Span| {
                        log_response_failure(&class, latency);
                    },
                ),
        )
        .with_state(state)
}
