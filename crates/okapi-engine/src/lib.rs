//! okapi-engine：三角色（gateway / console / worker）共享的运行时底座。
//!
//! 承载 `AppState`、状态装配与后台广播同步、凭证管理与账号控制、选路缓存、结算重试、
//! 以及需要被数据面和后台 worker 共用的异步媒体执行器；不包含 HTTP 路由表，使
//! `okapi-gateway`、`okapi-console`、`okapi-worker` 成为平级兄弟 crate 并行编译。

pub mod account_control;
pub mod admission;
pub mod auth;
pub mod bridge;
pub mod candidates;
pub mod channel_balance;
pub mod channel_permit;
pub mod channel_probe;
pub mod channel_test;
pub mod credentials;
pub mod diagnostics;
pub mod dialect;
pub mod execution_plan;
pub mod extensions;
pub mod failure;
pub mod images;
pub mod ingress;
pub mod key_gate;
pub mod key_health;
pub mod oauth_cred;
pub mod openai_dialect;
pub mod request_policy;
pub mod rule_inputs;
pub mod scheduler;
pub mod server_tools;
pub mod settlement_retry;
pub mod state;
pub mod stream_policy;
pub mod upstream_cost;
pub mod upstream_failure;
pub mod user_turns;
pub mod videos;

use okapi_ledger::BalanceLedger;
use okapi_pricing::PriceBookHandle;
use okapi_providers::PassUpstream;
use state::AppState;
use std::sync::Arc;
use std::time::Duration;

/// 下线时等待后台结算的上限。结算本身只是 Redis commit + 一次 PG 事务（含三次退避重试
/// 共约 4s），30s 足够；超时留给对账修复而不是无限期拖住编排层的 terminationGracePeriod。
pub const SETTLE_DRAIN_CAP: Duration = Duration::from_secs(30);

/// 按 PG 连接池大小构造结算/准入流控（`settle_gate` 钳制为连接池一半，留一半给前台点查）。
pub fn flow_control_for_pool(pg_pool_max: u32) -> state::FlowControl {
    let pool = usize::try_from(pg_pool_max.max(1)).unwrap_or(usize::MAX);
    state::FlowControl {
        admission_turns: user_turns::UserTurns::default(),
        settlement_turns: user_turns::UserTurns::default(),
        // 容量 = pool 的一半：留一半连接给点查/鉴权回源等前台路径
        settle_gate: Arc::new(tokio::sync::Semaphore::new(pool.div_ceil(2))),
        settle_backlog: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        // 缺省 ≈ 档位一 PG 记账速率（约 1k TPS）× 30s 下线窗口（§12.2 / §14.3）
        settle_backlog_max: okapi_store::env_config::number(
            "OKAPI_SETTLE_BACKLOG_MAX",
            20_000usize,
            |_| true,
        ),
        settle_shedding: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        settle_batcher: Arc::new(state::SettlementBatcher::new()),
        #[cfg(test)]
        settlement_journal_tag: Arc::from(format!("retry-test-{}", uuid::Uuid::new_v4())),
    }
}

/// 可选连接 NATS；连接失败降级为单机轮询/直连，不阻断启动。
pub async fn connect_nats_optional(nats_url: Option<&str>) -> Option<async_nats::Client> {
    match nats_url {
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
    }
}

/// 可选初始化 ClickHouse HTTP 客户端。
pub fn build_ch_optional(
    clickhouse_url: Option<&str>,
) -> anyhow::Result<Option<okapi_store::ChClient>> {
    match clickhouse_url {
        Some(url) => Ok(Some(
            okapi_store::ChClient::new(url, "okapi")
                .map_err(|e| anyhow::anyhow!("clickhouse client: {e}"))?,
        )),
        None => Ok(None),
    }
}

/// 装配共享运行时状态（bin 启动与集成测试共用）。
pub async fn build_state(
    database_url: &str,
    redis_url: &str,
    node: &str,
    clickhouse_url: Option<&str>,
    nats_url: Option<&str>,
) -> anyhow::Result<AppState> {
    let pool_max = okapi_store::default_pg_pool_size();
    let pg = okapi_store::connect_pg_with_max(database_url, pool_max).await?;
    okapi_store::run_migrations(&pg).await?;
    let redis = okapi_store::connect_redis(redis_url).await?;
    let ch = build_ch_optional(clickhouse_url)?;
    let nats = connect_nats_optional(nats_url).await;
    build_state_with_clients(pg, pool_max, redis, node, ch, nats).await
}

/// Assemble services around shared connections, including worker endpoint execution.
/// The caller has already applied migrations; this never opens another storage pool.
pub async fn build_state_with_resources(
    pg: sqlx::PgPool,
    redis: fred::clients::Client,
    node: &str,
    clickhouse_url: Option<&str>,
    nats_url: Option<&str>,
) -> anyhow::Result<AppState> {
    let ch = build_ch_optional(clickhouse_url)?;
    let nats = connect_nats_optional(nats_url).await;
    build_state_with_clients(
        pg,
        okapi_store::default_pg_pool_size(),
        redis,
        node,
        ch,
        nats,
    )
    .await
}

/// 使用已建好的共享 PG / Redis / CH / NATS 客户端装配 [`AppState`]（`Role::All` 与 `Role::Worker` 共用）。
#[allow(clippy::too_many_lines)] // Service graph construction lists resource/cache fields together.
pub async fn build_state_with_clients(
    pg: sqlx::PgPool,
    pg_pool_max: u32,
    redis: fred::clients::Client,
    node: &str,
    ch: Option<okapi_store::ChClient>,
    nats: Option<async_nats::Client>,
) -> anyhow::Result<AppState> {
    let ledger = BalanceLedger::new(redis.clone());
    let sched = okapi_kernel::sched_redis::SchedulerRedis::new(redis);
    let in_flight = okapi_kernel::inflight::InFlightGauge::new(sched.clone(), node);

    let book = okapi_kernel::pricing_loader::load_pricebook(&pg).await?;
    tracing::info!(epoch = book.epoch(), "PriceBook 已装载");

    let resources = okapi_providers::inference::Resources::new()
        .map_err(|e| anyhow::anyhow!("provider resources: {e}"))?;
    let inference = okapi_providers::inference::Registry::builtin(&resources)
        .map_err(|e| anyhow::anyhow!("provider registration: {e}"))?;
    let upstream = resources.openai.clone();
    let state = AppState {
        core: okapi_kernel::state::CoreState {
            pg,
            ledger,
            sched,
            pricebook: Arc::new(PriceBookHandle::new(book)),
            node: Arc::from(node),
            ch,
            stats_query_cache: okapi_kernel::state::build_query_cache(),
            nats,
            master_key: {
                let key = okapi_kernel::config::master_key_from_env();
                okapi_store::credential::warn_if_unprotected(key.as_deref());
                key.map(Arc::from)
            },
            settings_cache: moka::future::Cache::builder()
                .max_capacity(256)
                .time_to_live(std::time::Duration::from_mins(1))
                .build(),
            in_flight,
            settlements: okapi_kernel::shutdown::Pending::default(),
        },
        upstreams: state::Upstreams {
            azure: okapi_providers::AzureUpstream::new(upstream.clone()),
            openai: upstream,
            inference,
            anthropic: resources.anthropic,
            bedrock: resources.bedrock,
            vertex: resources.vertex,
            pass: PassUpstream::new().map_err(|e| anyhow::anyhow!("pass client: {e}"))?,
            refresh_gate: Arc::default(),
        },
        routing: state::RoutingCaches {
            model_cache: moka::future::Cache::builder()
                .time_to_live(Duration::from_mins(1))
                .max_capacity(100_000)
                .build(),
            cand_cache: moka::future::Cache::builder()
                .time_to_live(Duration::from_secs(5))
                .max_capacity(100_000)
                .build(),
            channel_cost_cache: moka::future::Cache::builder()
                .max_capacity(4096)
                .time_to_live(std::time::Duration::from_mins(1))
                .build(),
            channel_control_cache: moka::future::Cache::builder()
                .max_capacity(4096)
                .time_to_live(Duration::from_secs(5))
                .build(),
            margin_cache: moka::future::Cache::builder()
                .max_capacity(1)
                .time_to_live(Duration::from_secs(10))
                .build(),
            request_policy: moka::future::Cache::builder()
                .max_capacity(1)
                .time_to_live(Duration::from_secs(10))
                .build(),
        },
        health: state::HealthMarks {
            key_failures: moka::future::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(Duration::from_hours(24))
                .build(),
            egress_failures: moka::future::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_hours(24))
                .build(),
            egress_verifying: moka::future::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_mins(1))
                .build(),
        },
        flow: flow_control_for_pool(pg_pool_max),
        media: state::Media {
            image_download_gate: Arc::new(tokio::sync::Semaphore::new(4)),
            batch_submit_gate: Arc::new(tokio::sync::Semaphore::new(2)),
            image_storage: Arc::new(images::tasks::objects::Storage::from_env()?),
        },
        data_plane: None,
    };
    // 出站连接的 SSRF 校验从第一个请求起就按库里的策略走，而不是先严格 / 先宽松再等第一次轮询
    sync_ssrf_policy(&state).await;
    Ok(state)
}

/// 把 `settings.ssrf_policy.allow_private` 及扩展运行时设置同步给出站连接（`okapi_providers` 进程级配置）。
/// 读不到（PG 暂不可用）就沿用上一次的值，不往宽松或严格任一边猜。
pub async fn sync_ssrf_policy(state: &AppState) {
    match state.try_setting_cached("ssrf_policy").await {
        Ok(policy) => {
            let allow = policy
                .as_ref()
                .as_ref()
                .and_then(|value| value.get("allow_private"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            okapi_providers::ssrf::set_allow_private(allow);
        }
        Err(error) => {
            tracing::warn!(
                error_code = error.code,
                "ssrf_policy 读取失败，沿用上一次的出站校验策略"
            );
        }
    }
    for &key in okapi_providers::profiles::RUNTIME_SETTING_KEYS {
        if let Ok(value) = state.try_setting_cached(key).await {
            okapi_providers::profiles::apply_runtime_setting(key, value.as_ref().as_ref());
        }
    }
}

/// 管理员改了 `ssrf_policy` 之后，各进程最迟一个 settings 缓存 TTL（60s）加一个轮询周期内跟上。
pub fn spawn_ssrf_policy_sync(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.tick().await; // 首个 tick 立即完成：构造时已经同步过一次
        loop {
            tick.tick().await;
            sync_ssrf_policy(&state).await;
        }
    });
}

/// PriceBook 热更兜底：PG 最新 epoch 比当前新则重载替换。
/// M2 的 NATS 广播是主通道，本轮询是丢广播时的 30s 自校验（DESIGN §3.3 失效路径）。
pub async fn refresh_pricebook_if_newer(state: &AppState) -> anyhow::Result<bool> {
    // 空表读作 0，与 `load_pricing_source_rows` 一致：首次发布（epoch 1）才判得出"更新"
    let latest = sqlx::query_scalar!(
        r#"SELECT COALESCE(MAX(epoch), 0)::bigint AS "epoch!" FROM pricing_epochs"#
    )
    .fetch_one(&state.pg)
    .await?;
    if latest <= state.pricebook.epoch() {
        return Ok(false);
    }
    let book = okapi_kernel::pricing_loader::load_pricebook(&state.pg).await?;
    Ok(state.pricebook.swap_if_newer(book))
}

/// routing.invalidate 广播订阅：任一实例改了渠道/池/模型/设置，全集群立即弃用旧缓存。
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
            sync_ssrf_policy(&state).await;
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

/// 30s 自校验轮询：广播丢失时的兜底（DESIGN §3.3）。
pub fn spawn_epoch_poller(state: AppState) {
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

/// 启动共享状态的后台同步（epoch 轮询/广播、路由缓存失效广播、SSRF 与扩展运行时配置同步）。
pub fn spawn_state_sync(state: &AppState) {
    spawn_epoch_poller(state.clone());
    spawn_epoch_subscriber(state.clone());
    spawn_routing_invalidate_subscriber(state.clone());
    spawn_ssrf_policy_sync(state.clone());
}
