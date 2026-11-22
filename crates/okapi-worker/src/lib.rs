//! worker 角色：异步面（IMPLEMENTATION §2.1）。
//!
//! M2 第一批（无 CH/NATS 依赖的正确性核心）：
//! - 悬置预扣清理：超时未结算的 Redis 预扣按 deadline 释放并记事件；
//! - 三方对账 reconciler：billing_events 重放 ↔ Redis 热余额（含在途）↔ users.balance_micro；
//! - 分区维护：提前创建下月分区（docs/database.md §0）；
//! - 渠道 key 冷却恢复：cooldown 到期自动回 active。
//!
//! chsink（outbox → ClickHouse）已接入（单机直连形态）；NATS 传输在后续批次拆分。
//!
//! 本文件只放入口 `run`（拉起各后台任务与维护时钟）；维护调度在 maintenance，悬置预扣清理在
//! reservations，定时对账的告警在 reconcile（对账与漂移修复本身在 `kernel::reconcile` / `kernel::repair`，网关与控制台也用），
//! 分区、到期、订阅与冷却恢复在 housekeeping。

pub mod channel_balance;
pub mod channel_probe;
pub mod chsink;
mod delivery;
pub mod egress_probe;
mod housekeeping;
pub mod legacy_speech;
mod maintenance;
pub mod margin_breaker;
pub mod nats_relay;
pub mod notify;
pub mod oauth_refresh;
mod reconcile;
mod reservations;
mod supervision;
pub mod user_alerts;

pub use self::housekeeping::{
    ExpiredBalance, drop_expired_partitions, ensure_next_month_partitions, expire_balances,
    recover_cooled_keys, subscriptions_tick,
};
use self::maintenance::{
    ChSink, JsSink, MaintenanceContext, MaintenanceJob, run_maintenance, run_recovery,
};
pub use self::reconcile::reconcile_and_notify;
pub use self::reservations::{SweptReservation, sweep_expired_reservations};

use okapi_kernel::config::Config;
use okapi_ledger::BalanceLedger;
use std::time::Duration;

const SWEEP_INTERVAL: Duration = Duration::from_mins(1);
const RECONCILE_INTERVAL: Duration = Duration::from_mins(5);
const PARTITION_INTERVAL: Duration = Duration::from_hours(6);
const COOLDOWN_INTERVAL: Duration = Duration::from_secs(30);
const BALANCE_EXPIRY_INTERVAL: Duration = Duration::from_mins(5);
/// 人工队列（统计死信、结算隔离区）巡检：非零即告警。
const MANUAL_QUEUE_INTERVAL: Duration = Duration::from_mins(5);
/// 订阅滚窗 / 到期扫描周期（IMPLEMENTATION §11.28：间隙内 reserve 自动落钱包，保守方向）。
const SUBSCRIPTION_INTERVAL: Duration = Duration::from_mins(1);
/// 负毛利熔断评估周期（IMPLEMENTATION §11.34；立方体按小时聚合，更密没有意义）。
const MARGIN_BREAKER_INTERVAL: Duration = Duration::from_mins(5);
/// 对账分页大小。
const RECONCILE_BATCH: i64 = 1000;
/// 全量对账周期：其余各轮只查上一轮以来有账本事件的用户（见 [`reconcile_active_balances`]）。
const RECONCILE_FULL_INTERVAL: Duration = Duration::from_hours(24);
/// 增量轮的回看余量：上一轮起点再往前推这么多，覆盖事务提交晚于 created_at 的事件。
const RECONCILE_OVERLAP: Duration = Duration::from_mins(10);
/// 订阅每轮处理上限（到点的订阅按 window_end 升序）。
const SUBSCRIPTION_BATCH: i64 = 500;

/// 事件传输一拍：NATS 形态走 relay + JS 消费者，单机形态直连 outbox → CH。
async fn transport_tick(
    pg: &sqlx::PgPool,
    js: Option<&async_nats::jetstream::Context>,
    ch: Option<&okapi_store::ChClient>,
) {
    match (js, ch) {
        // 多机形态：relay → JetStream → chsink 消费者
        (Some(ctx), maybe_ch) => {
            match nats_relay::relay_once(pg, ctx).await {
                Ok(0) => {}
                Ok(rows) => tracing::debug!(rows, "relay 批次完成"),
                Err(err) => tracing::error!(error = %err, "relay 失败"),
            }
            if let Some(client) = maybe_ch {
                match nats_relay::chsink_js_once(pg, ctx, client).await {
                    Ok(0) => {}
                    Ok(rows) => tracing::debug!(rows, "chsink(JS) 批次完成"),
                    Err(err) => tracing::error!(error = %err, "chsink(JS) 失败"),
                }
            }
        }
        // 单机直连形态（docs/database.md §4.2）
        (None, Some(client)) => match chsink::process_once(pg, client).await {
            Ok(0) => {}
            Ok(rows) => tracing::debug!(rows, "chsink 批次完成"),
            Err(err) => tracing::error!(error = %err, "chsink 处理失败"),
        },
        (None, None) => {}
    }
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let pg = okapi_store::connect_pg(&cfg.database_url).await?;
    okapi_store::run_migrations(&pg).await?;
    let redis = okapi_store::connect_redis(&cfg.redis_url).await?;
    let nats = okapi_engine::connect_nats_optional(cfg.nats_url.as_deref()).await;
    let ch = okapi_engine::build_ch_optional(cfg.clickhouse_url.as_deref())?;
    let image_state = okapi_engine::build_state_with_clients(
        pg,
        okapi_store::default_pg_pool_size(),
        redis.clone(),
        &cfg.node,
        ch,
        nats,
    )
    .await?;
    Box::pin(run_with_state(
        cfg,
        image_state,
        redis,
        true,
        okapi_kernel::shutdown::signal(),
    ))
    .await
}

/// Run the background worker on an already-assembled `AppState` and Redis client (shared in `okapi all`).
// 六个周期任务 tick 的选择循环，拆分损害调度全貌可读性
#[allow(clippy::too_many_lines)]
pub async fn run_with_state<F>(
    cfg: Config,
    image_state: okapi_engine::state::AppState,
    redis: fred::clients::Client,
    spawn_sync: bool,
    stop: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send,
{
    let pg = image_state.pg.clone();
    let notifier = notify::Notifier::new(pg.clone(), redis.clone());
    let ledger = BalanceLedger::new(redis.clone());

    let ch = if let Some(client) = image_state.ch.clone() {
        let sink = ChSink::new(client);
        if sink.ready().await.is_none() {
            tracing::warn!(
                "ClickHouse 暂不可达，chsink 待命（统计接口 fail-closed），就绪后自动接上"
            );
        }
        Some(sink)
    } else {
        tracing::info!("未配置 OKAPI_CLICKHOUSE_URL，chsink 停用");
        None
    };

    let js = if let Some(client) = image_state.nats.clone() {
        let sink = JsSink::new(client);
        let _ = sink.ready().await;
        Some(sink)
    } else {
        None
    };
    if spawn_sync {
        // Keep PriceBook, routing caches and SSRF/runtime settings fresh in standalone worker mode.
        okapi_engine::spawn_state_sync(&image_state);
    }
    let (image_stop, image_stopped) = tokio::sync::watch::channel(false);
    let native_batch_worker =
        tokio::spawn(supervision::run("image_batches", image_stopped.clone(), {
            let state = image_state.clone();
            let stop = image_stopped.clone();
            move || okapi_engine::images::batches::run_worker(state.clone(), stop.clone())
        }));
    let recovery_workers: Vec<_> = [false, true]
        .into_iter()
        .map(|video| {
            tokio::spawn(supervision::run(
                if video {
                    "video_recovery"
                } else {
                    "settlement_recovery"
                },
                image_stopped.clone(),
                {
                    let state = image_state.clone();
                    let stop = image_stopped.clone();
                    move || run_recovery(state.clone(), stop.clone(), video)
                },
            ))
        })
        .collect();
    let image_worker = tokio::spawn(supervision::run("image_tasks", image_stopped.clone(), {
        // Image and credential workers share the same assembled state and credential gates.
        let image_state = image_state.clone();
        let stop = image_stopped.clone();
        move || okapi_engine::images::tasks::run_worker(image_state.clone(), stop.clone())
    }));

    let credential_worker =
        tokio::spawn(supervision::run("oauth_refresh", image_stopped.clone(), {
            let state = image_state.clone();
            let stop = image_stopped.clone();
            move || oauth_refresh::run(state.clone(), stop.clone())
        }));
    // 出口代理后台探测（§11.41）：出口 IP 变化 / 在用代理不可达告警
    let egress_worker = tokio::spawn(supervision::run("egress_probe", image_stopped.clone(), {
        let state = image_state.clone();
        let notifier = notifier.clone();
        let stop = image_stopped.clone();
        move || egress_probe::run(state.clone(), notifier.clone(), stop.clone())
    }));

    // 渠道 key 的后台探测与自动恢复（§11.73）：缺省关，站长在 channel_probe_policy 里打开
    let channel_probe_worker =
        tokio::spawn(supervision::run("channel_probe", image_stopped.clone(), {
            let state = image_state.clone();
            let stop = image_stopped.clone();
            move || channel_probe::run(state.clone(), stop.clone())
        }));

    // 渠道上游余额的定时刷新与低余额处置（§11.73）：缺省关，站长在 channel_balance_policy 里打开
    let channel_balance_worker = tokio::spawn(supervision::run(
        "channel_balance",
        image_stopped.clone(),
        {
            let state = image_state.clone();
            let notifier = notifier.clone();
            let stop = image_stopped.clone();
            move || channel_balance::run(state.clone(), notifier.clone(), stop.clone())
        },
    ));

    // 运维监控趋势采样（§11.43）：每分钟一个点，保留 24 小时
    let ops_sampler = tokio::spawn(supervision::run("ops_sampler", image_stopped.clone(), {
        let probes = okapi_kernel::ops::probes::Probes {
            pg: pg.clone(),
            redis: redis.clone(),
            // 探针拿配置好的客户端，不论启动时连没连上：连不上要报"异常"而不是"未配置"
            ch: ch.as_ref().map(|sink| sink.client.clone()),
            nats: image_state.nats.clone(),
        };
        let node = cfg.node.clone();
        let stop = image_stopped.clone();
        move || okapi_kernel::ops::sampler::run(probes.clone(), node.clone(), stop.clone())
    }));

    tracing::info!(
        "okapi worker 启动（relay/chsink/sweep/reconcile/partition/cooldown/subscriptions/margin_breaker）"
    );

    let maintenance = std::sync::Arc::new(MaintenanceContext {
        pg,
        ledger,
        redis,
        sched: image_state.sched.clone(),
        node: cfg.node.clone(),
        nats: image_state.nats.clone(),
        notifier,
        js,
        ch,
        reconcile: std::sync::Mutex::default(),
    });
    let mut workers = recovery_workers;
    workers.extend([
        native_batch_worker,
        image_worker,
        credential_worker,
        egress_worker,
        channel_probe_worker,
        channel_balance_worker,
        ops_sampler,
    ]);
    for (job, interval) in [
        (MaintenanceJob::chsink_tick, Duration::from_secs(1)),
        (MaintenanceJob::unit_calibration, Duration::from_secs(10)),
        (MaintenanceJob::subscriptions, SUBSCRIPTION_INTERVAL),
        (MaintenanceJob::sweep, SWEEP_INTERVAL),
        (MaintenanceJob::reconcile, RECONCILE_INTERVAL),
        (MaintenanceJob::partition, PARTITION_INTERVAL),
        (MaintenanceJob::cooldown, COOLDOWN_INTERVAL),
        (MaintenanceJob::margin, MARGIN_BREAKER_INTERVAL),
        (MaintenanceJob::balance_expiry, BALANCE_EXPIRY_INTERVAL),
        (MaintenanceJob::manual_queues, MANUAL_QUEUE_INTERVAL),
    ] {
        workers.push(tokio::spawn(supervision::run(
            job.name(),
            image_stop.subscribe(),
            {
                let ctx = maintenance.clone();
                let stop = image_stop.subscribe();
                move || run_maintenance(ctx.clone(), stop.clone(), job, interval)
            },
        )));
    }
    stop.await;
    let _ = image_stop.send(true);
    // Cancel-safe maintenance transactions roll back on stop. Other workers get a bounded drain.
    if tokio::time::timeout(
        Duration::from_secs(30),
        futures::future::join_all(workers.iter_mut()),
    )
    .await
    .is_err()
    {
        for worker in &workers {
            worker.abort();
        }
        futures::future::join_all(workers).await;
    }
    image_state.settlements.drain();
    image_state
        .settlements
        .wait_idle(okapi_engine::SETTLE_DRAIN_CAP)
        .await;
    tracing::info!("worker 已下线");
    Ok(())
}
