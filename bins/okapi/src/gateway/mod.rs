//! gateway 角色：数据面（鉴权、限流、预扣/结算、SSE 透传）。

pub mod audio;
pub mod auth;
pub mod bootstrap;
pub mod chat;
pub mod clients;
pub mod custom_pass;
pub mod dashboard;
pub mod embeddings;
pub mod error;
pub mod estimate;
pub mod images;
pub mod models;
pub mod pricing_loader;
pub mod realtime;
pub mod rule_inputs;
pub mod sched_redis;
pub mod scheduler;
pub mod state;
pub mod videos;

use crate::config::Config;
use axum::Router;
use axum::routing::{get, post};
use okapi_ledger::BalanceLedger;
use okapi_pricing::PriceBookHandle;
use okapi_providers::{AnthropicUpstream, GeminiUpstream, OpenAiUpstream, PassUpstream};
use state::AppState;
use std::sync::Arc;
use std::time::Duration;
use tower_http::trace::TraceLayer;

/// 装配 gateway 共享状态（bin 启动与集成测试共用）。
pub async fn build_state(
    database_url: &str,
    redis_url: &str,
    node: &str,
    clickhouse_url: Option<&str>,
    nats_url: Option<&str>,
) -> anyhow::Result<AppState> {
    let pg = okapi_store::connect_pg(database_url).await?;
    okapi_store::run_migrations(&pg).await?;
    let redis = okapi_store::connect_redis(redis_url).await?;
    let ledger = BalanceLedger::new(redis.clone());
    let sched = sched_redis::SchedulerRedis::new(redis);

    let book = pricing_loader::load_pricebook(&pg).await?;
    tracing::info!(epoch = book.epoch(), "PriceBook 已装载");

    let nats = match nats_url {
        Some(url) => match async_nats::connect(url).await {
            Ok(client) => {
                tracing::info!("NATS 已连接（epoch 广播/事件总线可用）");
                Some(client)
            }
            Err(err) => {
                tracing::warn!(error = %err, "NATS 连接失败，回退单机形态（轮询/直连）");
                None
            }
        },
        None => None,
    };

    let ch = match clickhouse_url {
        Some(url) => Some(
            okapi_store::ChClient::new(url, "okapi")
                .map_err(|e| anyhow::anyhow!("clickhouse client: {e}"))?,
        ),
        None => None,
    };

    Ok(AppState {
        pg,
        ledger,
        sched,
        pricebook: Arc::new(PriceBookHandle::new(book)),
        model_cache: moka::future::Cache::builder()
            .time_to_live(Duration::from_mins(1))
            .max_capacity(100_000)
            .build(),
        cand_cache: moka::future::Cache::builder()
            .time_to_live(Duration::from_secs(5))
            .max_capacity(100_000)
            .build(),
        upstream: OpenAiUpstream::new().map_err(|e| anyhow::anyhow!("upstream client: {e}"))?,
        anthropic: AnthropicUpstream::new()
            .map_err(|e| anyhow::anyhow!("anthropic client: {e}"))?,
        gemini: GeminiUpstream::new().map_err(|e| anyhow::anyhow!("gemini client: {e}"))?,
        pass: PassUpstream::new().map_err(|e| anyhow::anyhow!("pass client: {e}"))?,
        node: Arc::from(node),
        ch,
        nats,
        master_key: {
            let key = std::env::var("OKAPI_MASTER_KEY").ok();
            okapi_store::credential::warn_if_unprotected(key.as_deref());
            key.map(Arc::from)
        },
        settings_cache: moka::future::Cache::builder()
            .max_capacity(256)
            .time_to_live(std::time::Duration::from_mins(1))
            .build(),
        // 容量 = pool 的一半：留一半连接给点查/鉴权回源等前台路径
        settle_gate: Arc::new(tokio::sync::Semaphore::new(
            std::env::var("OKAPI_PG_POOL")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(16)
                .div_ceil(2),
        )),
        in_flight: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        surge_reported_at: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        channel_cost_cache: moka::future::Cache::builder()
            .max_capacity(4096)
            .time_to_live(std::time::Duration::from_mins(1))
            .build(),
    })
}

/// PriceBook 热更兜底：PG 最新 epoch 比当前新则重载替换。
/// M2 的 NATS 广播是主通道，本轮询是丢广播时的 30s 自校验（DESIGN §3.3 失效路径）。
pub async fn refresh_pricebook_if_newer(state: &AppState) -> anyhow::Result<bool> {
    let latest = sqlx::query_scalar!(
        r#"SELECT COALESCE(MAX(epoch), 1)::bigint AS "epoch!" FROM pricing_epochs"#
    )
    .fetch_one(&state.pg)
    .await?;
    if latest <= state.pricebook.epoch() {
        return Ok(false);
    }
    let book = pricing_loader::load_pricebook(&state.pg).await?;
    Ok(state.pricebook.swap_if_newer(book))
}

/// routing.invalidate 广播订阅：任一实例改了渠道/池/模型/设置，全集群立即弃用旧缓存。
///
/// 没有这条通道时，其它 pod 只能等自己的 TTL 过期（候选集 5s、模型 60s、settings 60s）——
/// "禁用一条出问题的渠道"这种应急操作，管理员点完看着已生效，实际另外几个 pod 还会
/// 继续往那条渠道打将近一分钟。
pub fn spawn_routing_invalidate_subscriber(state: AppState) {
    let Some(client) = state.nats.clone() else {
        return;
    };
    // detach 说明：与进程同生命周期的订阅任务
    tokio::spawn(async move {
        use futures::StreamExt as _;
        let mut sub = match client.subscribe(state::ROUTING_INVALIDATE_SUBJECT).await {
            Ok(sub) => sub,
            Err(err) => {
                tracing::warn!(error = %err, "routing.invalidate 订阅失败（依赖 TTL 兜底）");
                return;
            }
        };
        while sub.next().await.is_some() {
            // 只清本地：清完再广播会打环
            state.invalidate_routing_caches_local();
            tracing::debug!("路由缓存已按广播失效");
        }
    });
}

/// pricing.epoch 广播订阅（主通道；30s 轮询为丢广播兜底，DESIGN §3.3）。
pub fn spawn_epoch_subscriber(state: AppState) {
    let Some(client) = state.nats.clone() else {
        return;
    };
    // detach 说明：与进程同生命周期的订阅任务
    tokio::spawn(async move {
        use futures::StreamExt as _;
        let mut sub = match client.subscribe("pricing.epoch").await {
            Ok(sub) => sub,
            Err(err) => {
                tracing::warn!(error = %err, "pricing.epoch 订阅失败（依赖轮询兜底）");
                return;
            }
        };
        while let Some(_msg) = sub.next().await {
            match refresh_pricebook_if_newer(&state).await {
                Ok(true) => {
                    tracing::info!(epoch = state.pricebook.epoch(), "PriceBook 广播热更");
                }
                Ok(false) => {}
                Err(err) => tracing::warn!(error = %err, "广播热更失败"),
            }
        }
    });
}

fn spawn_epoch_poller(state: AppState) {
    // detach 说明：与进程同生命周期的兜底轮询
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            match refresh_pricebook_if_newer(&state).await {
                Ok(true) => {
                    tracing::info!(epoch = state.pricebook.epoch(), "PriceBook 已热更");
                }
                Ok(false) => {}
                Err(err) => tracing::warn!(error = %err, "PriceBook 轮询失败"),
            }
        }
    });
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

    // §6.5 生产护栏：release 构建启用单用户模式须显式二次确认
    if cfg.single_user_mode
        && !cfg!(debug_assertions)
        && std::env::var("OKAPI_SINGLE_USER_CONFIRM").ok().as_deref() != Some("true")
    {
        anyhow::bail!(
            "single_user_mode 在 release 构建需同时设 OKAPI_SINGLE_USER_CONFIRM=true（root key 会进日志，误开公网代价高）"
        );
    }
    if cfg.single_user_mode {
        bootstrap::ensure_single_user(&state).await?;
    }
    spawn_epoch_poller(state.clone());
    spawn_epoch_subscriber(state.clone());
    spawn_routing_invalidate_subscriber(state.clone());

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, "okapi gateway 启动");
    // with_connect_info：直连（无 CDN 头）时 key 级 IP 白名单与 client_ip 列都要用到对端地址
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

/// 组装路由（集成测试直接复用）。
/// 请求体上限 32MB（网关不解压请求体，即为有效字节上限；防超大体/zip bomb，§3.7-8）。
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat::chat_completions))
        .route("/v1/messages", post(chat::messages))
        .route("/v1/responses", post(chat::responses))
        .route("/v1/embeddings", post(embeddings::embeddings))
        .route("/v1/images/generations", post(images::images))
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
            rule_inputs::track_in_flight,
        ))
        .layer(axum::middleware::from_fn(clients::stamp_peer_ip))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("收到退出信号，开始优雅下线");
}
