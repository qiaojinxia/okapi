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
//! reservations，对账与漂移修复在 reconcile / repair，分区、到期、订阅与冷却恢复在 housekeeping。

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
mod repair;
mod reservations;
mod supervision;

pub use self::housekeeping::{
    ExpiredBalance, drop_expired_partitions, ensure_next_month_partitions, expire_balances,
    recover_cooled_keys, subscriptions_tick,
};
use self::maintenance::{
    ChSink, MaintenanceContext, MaintenanceJob, run_maintenance, run_recovery,
};
use self::reconcile::redis_effective;
pub use self::reconcile::{
    BalanceDrift, reconcile_active_balances, reconcile_and_notify, reconcile_balances,
};
use self::repair::SETTLE_WINDOW;
pub use self::repair::{BalanceRepair, repair_balance, repair_drifted, stable_drift};
pub use self::reservations::{SweptReservation, sweep_expired_reservations};

use crate::config::Config;
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

/// 连接 NATS 并确保 JetStream 拓扑；失败回退单机直连形态（None）。
async fn connect_jetstream(nats_url: Option<&str>) -> Option<async_nats::jetstream::Context> {
    let url = nats_url?;
    match async_nats::connect(url).await {
        Ok(client) => match nats_relay::ensure_topology(&client).await {
            Ok(ctx) => {
                tracing::info!("NATS relay 就绪（outbox → JetStream → chsink）");
                Some(ctx)
            }
            Err(err) => {
                tracing::warn!(error = %err, "JetStream 拓扑创建失败，回退直连形态");
                None
            }
        },
        Err(err) => {
            tracing::warn!(error = %err, "NATS 连接失败，回退直连形态");
            None
        }
    }
}

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

// 六个周期任务 tick 的选择循环，拆分损害调度全貌可读性
#[allow(clippy::too_many_lines)]
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let pg = okapi_store::connect_pg(&cfg.database_url).await?;
    okapi_store::run_migrations(&pg).await?;
    let redis = okapi_store::connect_redis(&cfg.redis_url).await?;
    let notifier = notify::Notifier::new(pg.clone(), redis.clone());
    let ledger = BalanceLedger::new(redis.clone());

    let ch = if let Some(url) = &cfg.clickhouse_url {
        let client = okapi_store::ChClient::new(url, "okapi")
            .map_err(|e| anyhow::anyhow!("clickhouse client: {e}"))?;
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

    let js = connect_jetstream(cfg.nats_url.as_deref()).await;
    let image_state = crate::gateway::build_state_with_resources(
        pg.clone(),
        redis.clone(),
        &cfg.node,
        None,
        None,
    )
    .await?;
    // worker 的原生批量 / 图片任务同样直连上游：策略改了不能要重启才生效
    crate::gateway::spawn_ssrf_policy_sync(image_state.clone());
    let (image_stop, image_stopped) = tokio::sync::watch::channel(false);
    let native_batch_worker =
        tokio::spawn(supervision::run("image_batches", image_stopped.clone(), {
            let state = image_state.clone();
            let stop = image_stopped.clone();
            move || crate::gateway::images::batches::run_worker(state.clone(), stop.clone())
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
        move || crate::gateway::images::tasks::run_worker(image_state.clone(), stop.clone())
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

    // 运维监控趋势采样（§11.43）：每分钟一个点，保留 24 小时
    let ops_sampler = tokio::spawn(supervision::run("ops_sampler", image_stopped.clone(), {
        let probes = crate::ops::probes::Probes {
            pg: pg.clone(),
            redis: redis.clone(),
            // 探针拿配置好的客户端，不论启动时连没连上：连不上要报"异常"而不是"未配置"
            ch: ch.as_ref().map(|sink| sink.client.clone()),
            nats: js.as_ref().map(async_nats::jetstream::Context::client),
        };
        let node = cfg.node.clone();
        let stop = image_stopped.clone();
        move || crate::ops::sampler::run(probes.clone(), node.clone(), stop.clone())
    }));

    tracing::info!(
        "okapi worker 启动（relay/chsink/sweep/reconcile/partition/cooldown/subscriptions/margin_breaker）"
    );

    let maintenance = std::sync::Arc::new(MaintenanceContext {
        pg,
        ledger,
        redis,
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
    crate::shutdown::signal().await;
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
    tracing::info!("worker 已下线");
    Ok(())
}
