//! 维护任务的调度：每个任务一个时钟（多副本下通过 Redis 互斥锁 + 周期占位协调）、
//! 单机直连 CH / 懒恢复 JetStream 写入器，以及启动时的一次性恢复。
use super::{
    RECONCILE_BATCH, RECONCILE_FULL_INTERVAL, RECONCILE_OVERLAP, SWEEP_INTERVAL,
    drop_expired_partitions, ensure_next_month_partitions, expire_balances, legacy_speech,
    load_conversation_archive_policy, margin_breaker, nats_relay, notify,
    prune_conversation_archives, reconcile_and_notify, recover_cooled_keys, subscriptions_tick,
    sweep_expired_reservations, transport_tick,
};
use okapi_kernel::sched_redis::{MaintenanceLease, ReconcileCheckpoint, SchedulerRedis};
use okapi_ledger::BalanceLedger;
use sqlx::PgPool;
use std::time::Duration;

pub(crate) const MAINT_LOCK_TTL_SECS: i64 = 120;
pub(super) const JS_RETRY_INTERVAL: Duration = Duration::from_secs(15);

pub(super) struct MaintenanceContext {
    pub(super) pg: PgPool,
    pub(super) ledger: BalanceLedger,
    pub(super) redis: fred::clients::Client,
    pub(super) sched: SchedulerRedis,
    pub(super) node: String,
    pub(super) nats: Option<async_nats::Client>,
    pub(super) notifier: notify::Notifier,
    pub(super) js: Option<JsSink>,
    pub(super) ch: Option<ChSink>,
    pub(super) reconcile: std::sync::Mutex<ReconcileClock>,
    pub(super) last_archive_cleanup: std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>,
}

/// 启动时 NATS JetStream 可能尚未就绪；与 [`ChSink`] 同口径，首次失败后按周期自动补建拓扑，
/// 避免永久困在单机直连模式。同时缓存 `PullConsumer` 句柄，避免每秒重复调用 `get_stream` / `create_consumer`。
pub(super) struct JsSink {
    client: async_nats::Client,
    ctx: tokio::sync::RwLock<Option<async_nats::jetstream::Context>>,
    consumer: tokio::sync::RwLock<Option<nats_relay::CachedConsumer>>,
    next_try: std::sync::Mutex<std::time::Instant>,
}

impl JsSink {
    pub(super) fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            ctx: tokio::sync::RwLock::new(None),
            consumer: tokio::sync::RwLock::new(None),
            next_try: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    pub(super) async fn ready(&self) -> Option<async_nats::jetstream::Context> {
        if let Some(ctx) = self.ctx.read().await.as_ref() {
            return Some(ctx.clone());
        }
        {
            let mut next = self
                .next_try
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = std::time::Instant::now();
            if now < *next {
                return None;
            }
            *next = now + JS_RETRY_INTERVAL;
        }
        match nats_relay::ensure_topology(&self.client).await {
            Ok(ctx) => {
                *self.ctx.write().await = Some(ctx.clone());
                tracing::info!("NATS relay 就绪（outbox → JetStream → chsink）");
                Some(ctx)
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    retry_in_secs = JS_RETRY_INTERVAL.as_secs(),
                    "JetStream 拓扑尚未就绪，暂走直连并稍后重试"
                );
                None
            }
        }
    }

    pub(super) async fn consumer(
        &self,
        ctx: &async_nats::jetstream::Context,
    ) -> anyhow::Result<nats_relay::CachedConsumer> {
        if let Some(cached) = self.consumer.read().await.as_ref() {
            return Ok(cached.clone());
        }
        let fresh = nats_relay::ensure_consumer(ctx).await?;
        *self.consumer.write().await = Some(fresh.clone());
        Ok(fresh)
    }

    pub(super) async fn invalidate_consumer(&self) {
        *self.consumer.write().await = None;
    }
}

/// 启动时 ClickHouse 可能还没起来（整机重启时各容器同时拉起）。只在启动那一刻 ping 一次的话，
/// chsink 会一直停到下次重启，outbox 里的事件始终进不了统计。这里改成没就绪就隔一段再试，
/// 首次接上时补建表结构。
pub(super) struct ChSink {
    pub(super) client: okapi_store::ChClient,
    pub(super) ready: std::sync::atomic::AtomicBool,
    pub(super) next_try: std::sync::Mutex<std::time::Instant>,
}

pub(super) const CH_RETRY_INTERVAL: Duration = Duration::from_secs(15);

impl ChSink {
    pub(super) fn new(client: okapi_store::ChClient) -> Self {
        Self {
            client,
            ready: std::sync::atomic::AtomicBool::new(false),
            next_try: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    /// 就绪则返回客户端；未就绪时到了重试时间才去 ping，其余 tick 直接返回 None。
    pub(super) async fn ready(&self) -> Option<&okapi_store::ChClient> {
        use std::sync::atomic::Ordering;
        if self.ready.load(Ordering::Acquire) {
            return Some(&self.client);
        }
        {
            let mut next = self
                .next_try
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = std::time::Instant::now();
            if now < *next {
                return None;
            }
            *next = now + CH_RETRY_INTERVAL;
        }
        if !self.client.ping().await {
            return None;
        }
        if let Err(error) = self.client.ensure_schema().await {
            tracing::error!(%error, "ClickHouse 表结构初始化失败，稍后重试");
            return None;
        }
        self.ready.store(true, Ordering::Release);
        tracing::info!("chsink 就绪（outbox → ClickHouse）");
        Some(&self.client)
    }
}

/// 对账节奏：首次与之后每 [`RECONCILE_FULL_INTERVAL`] 一轮做全量，其余增量。
/// 多副本间通过 Redis `maint:clock:{reconcile}` 同步进度，本机字段作为 Redis 暂不可用时的兜底。
#[derive(Default)]
pub(super) struct ReconcileClock {
    pub(super) last_full: Option<chrono::DateTime<chrono::Utc>>,
    /// 上一轮（无论全量增量）成功时的起点。
    pub(super) last_pass: Option<chrono::DateTime<chrono::Utc>>,
}

impl ReconcileClock {
    fn hydrate(&mut self, remote: Option<ReconcileCheckpoint>) {
        let Some(cp) = remote else {
            return;
        };
        self.last_full = merge_latest_utc(self.last_full, cp.last_full_unix);
        self.last_pass = merge_latest_utc(self.last_pass, cp.last_pass_unix);
    }

    /// None = 这一轮做全量；Some(t) = 只查 t 之后有事件的用户。
    fn plan(&self, now: chrono::DateTime<chrono::Utc>) -> Option<chrono::DateTime<chrono::Utc>> {
        let full_interval =
            chrono::Duration::from_std(RECONCILE_FULL_INTERVAL).unwrap_or_default();
        let full_due = self
            .last_full
            .is_none_or(|at| now.signed_duration_since(at) >= full_interval);
        if full_due {
            return None;
        }
        let overlap = chrono::Duration::from_std(RECONCILE_OVERLAP).unwrap_or_default();
        self.last_pass.map(|at| at - overlap)
    }

    fn record(&mut self, started: chrono::DateTime<chrono::Utc>, is_full: bool) {
        self.last_pass = Some(self.last_pass.map_or(started, |prev| prev.max(started)));
        if is_full {
            self.last_full = Some(self.last_full.map_or(started, |prev| prev.max(started)));
        }
    }
}

fn merge_latest_utc(
    local: Option<chrono::DateTime<chrono::Utc>>,
    remote_unix: Option<i64>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let remote = remote_unix.and_then(|secs| chrono::DateTime::from_timestamp(secs, 0));
    match (local, remote) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

fn archive_cleanup_due(
    last_run: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    interval_hours: u32,
) -> bool {
    let min_gap = chrono::Duration::hours(i64::from(interval_hours));
    last_run.is_none_or(|prev| now.signed_duration_since(prev) >= min_gap)
}

#[allow(non_camel_case_types)]
#[derive(Clone, Copy)]
pub(super) enum MaintenanceJob {
    chsink_tick,
    unit_calibration,
    subscriptions,
    sweep,
    reconcile,
    partition,
    cooldown,
    margin,
    balance_expiry,
    manual_queues,
    conversation_archive,
}

impl MaintenanceJob {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::chsink_tick => "chsink",
            Self::unit_calibration => "unit_calibration",
            Self::subscriptions => "subscriptions",
            Self::sweep => "sweep",
            Self::reconcile => "reconcile",
            Self::partition => "partition",
            Self::cooldown => "cooldown",
            Self::margin => "margin",
            Self::balance_expiry => "balance_expiry",
            Self::manual_queues => "manual_queues",
            Self::conversation_archive => "conversation_archive",
        }
    }
    /// `chsink` 已由 `FOR UPDATE SKIP LOCKED` 与 JetStream 持久消费者原生支持多副本并发，无需单主互斥。
    pub(super) const fn needs_lease(self) -> bool {
        !matches!(self, Self::chsink_tick)
    }
}

/// 周期占位 TTL：取间隔的 80%（夹在 2s..=3600s），避免刚跑完几百毫秒就被另一副本重复触发。
pub(crate) fn maintenance_round_ttl_secs(interval: Duration) -> i64 {
    let secs = interval.as_secs().saturating_mul(4) / 5;
    i64::try_from(secs.clamp(2, 3600)).unwrap_or(3600)
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// 在 Redis 分布式维护租约保护下执行一轮后台任务（含自动心跳续租与 CAS 释放）。
/// 返回 `true` 表示收到了停机信号，调用方应立即退出循环。
pub(crate) fn run_leased<'a, F>(
    sched: &'a SchedulerRedis,
    node: &'a str,
    job: &'static str,
    lock_ttl_secs: i64,
    round_ttl_secs: i64,
    stop: &'a mut tokio::sync::watch::Receiver<bool>,
    work: F,
) -> impl std::future::Future<Output = bool> + Send + 'a
where
    F: std::future::Future<Output = ()> + Send + 'a,
{
    let mut work = Box::pin(work);
    async move {
        if *stop.borrow() {
            return true;
        }
        let (token, outcome) = match sched
            .maintenance_claim(job, node, lock_ttl_secs, round_ttl_secs)
            .await
        {
            MaintenanceLease::Acquired(token) => (Some(token), "ran"),
            MaintenanceLease::RedisUnavailable => (None, "redis_unavailable"),
            MaintenanceLease::Skipped => {
                okapi_kernel::metrics::global().record_worker_maintenance(job, "skipped");
                tracing::trace!(job, "maintenance skipped; held by another replica");
                return false;
            }
        };
        okapi_kernel::metrics::global().record_worker_maintenance(job, outcome);
        let heartbeat_secs = u64::try_from((lock_ttl_secs / 3).max(5)).unwrap_or(5);
        let heartbeat = token.as_ref().map(|token| {
            let sched = sched.clone();
            let token = token.clone();
            AbortOnDrop(tokio::spawn(async move {
                let mut renew = tokio::time::interval(Duration::from_secs(heartbeat_secs));
                renew.tick().await;
                loop {
                    renew.tick().await;
                    if !sched.maintenance_extend(job, &token, lock_ttl_secs).await {
                        tracing::warn!(job, "maintenance lease heartbeat lost");
                        return;
                    }
                }
            }))
        });
        let stopped = tokio::select! {
            _ = stop.changed() => true,
            () = &mut work => false,
        };
        drop(heartbeat);
        if let Some(token) = token {
            sched.maintenance_release(job, &token).await;
        }
        stopped
    }
}

pub(super) async fn run_maintenance(
    ctx: std::sync::Arc<MaintenanceContext>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    job: MaintenanceJob,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() {
            return;
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tick.tick() => {}
        }
        if !job.needs_lease() {
            tokio::select! {
                _ = stop.changed() => return,
                () = maintenance_once(&ctx, job) => {}
            }
            continue;
        }
        let round_ttl = maintenance_round_ttl_secs(interval);
        if run_leased(
            &ctx.sched,
            &ctx.node,
            job.name(),
            MAINT_LOCK_TTL_SECS,
            round_ttl,
            &mut stop,
            maintenance_once(&ctx, job),
        )
        .await
        {
            return;
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(super) async fn maintenance_once(ctx: &MaintenanceContext, job: MaintenanceJob) {
    let MaintenanceContext {
        pg,
        ledger,
        redis,
        sched,
        node: _,
        nats,
        notifier,
        js,
        ch,
        reconcile,
        last_archive_cleanup,
    } = ctx;
    let ch = match ch {
        Some(sink) => sink.ready().await,
        None => None,
    };
    match job {
        MaintenanceJob::chsink_tick => {
            transport_tick(pg, js.as_ref(), ch).await;
        }
        MaintenanceJob::unit_calibration => {
            if let Some(ch) = ch {
                match legacy_speech::process_once(ch, 500).await {
                    Ok(0) => {}
                    Ok(rows) => tracing::debug!(rows, "historical speech unit calibration batch"),
                    Err(error) => {
                        tracing::error!(%error,"historical speech unit calibration deferred");
                    }
                }
            }
        }
        MaintenanceJob::subscriptions => {
            match subscriptions_tick(pg, ledger, redis, chrono::Utc::now()).await {
                Ok(r) if r.rolled + r.expired + r.failed > 0 => {
                    tracing::info!(
                        rolled = r.rolled,
                        expired = r.expired,
                        failed = r.failed,
                        "订阅滚窗 / 到期处理"
                    );
                }
                Ok(_) => {}
                Err(err) => tracing::error!(error = %err, "订阅滚窗 / 到期处理失败"),
            }
        }
        MaintenanceJob::sweep => {
            // 预扣截止时间按 Redis TIME 写入，判过期也用它，不用 worker 本机时钟
            let now = match ledger.server_now().await {
                Ok(now) => now,
                Err(err) => {
                    tracing::error!(error = %err, "悬置预扣清理跳过：读取 Redis 时钟失败");
                    return;
                }
            };
            match sweep_expired_reservations(pg, ledger, now).await {
                Ok(swept) if !swept.is_empty() => {
                    tracing::warn!(count = swept.len(), "悬置预扣已清理");
                }
                Ok(_) => {}
                Err(err) => tracing::error!(error = %err, "悬置预扣清理失败"),
            }
        }
        MaintenanceJob::reconcile => {
            let started = chrono::Utc::now();
            let remote = sched.maint_reconcile_clock_get().await;
            let since = {
                let mut clock = reconcile
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                clock.hydrate(remote);
                clock.plan(started)
            };
            match reconcile_and_notify(pg, ledger, RECONCILE_BATCH, since, notifier).await {
                Ok(_) => {
                    let is_full = since.is_none();
                    reconcile
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record(started, is_full);
                    sched
                        .maint_reconcile_clock_set(started.timestamp(), is_full)
                        .await;
                }
                Err(err) => tracing::error!(error = %err, "对账失败"),
            }
        }
        MaintenanceJob::partition => {
            match ensure_next_month_partitions(pg, chrono::Utc::now()).await {
                Ok(created) if !created.is_empty() => {
                    tracing::info!(?created, "已创建下月分区");
                }
                Ok(_) => {}
                Err(err) => tracing::error!(error = %err, "分区维护失败"),
            }
            match drop_expired_partitions(pg, chrono::Utc::now()).await {
                Ok(dropped) if !dropped.is_empty() => {
                    tracing::warn!(?dropped, "保留策略已裁剪超期分区");
                }
                Ok(_) => {}
                Err(err) => tracing::error!(error = %err, "保留策略裁剪失败"),
            }
        }
        MaintenanceJob::cooldown => {
            if let Err(error) = okapi_store::channel_usage::prune(pg).await {
                tracing::warn!(%error,"channel usage window pruning failed");
            }
            match recover_cooled_keys(pg).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(recovered = n, "渠道 key 冷却到期恢复"),
                Err(err) => tracing::error!(error = %err, "冷却恢复失败"),
            }
            if let Err(err) = notify::channel_cooldown_and_notify(pg, notifier).await {
                tracing::error!(error = %err, "冷却告警失败");
            }
        }
        MaintenanceJob::margin => {
            match margin_breaker::evaluate_and_notify(pg, ch, redis, chrono::Utc::now(), notifier)
                .await
            {
                Ok(report) if !report.tripped.is_empty() || report.pruned > 0 => {
                    if let Some(client) = nats
                        && let Err(err) = client
                            .publish(
                                okapi_engine::state::ROUTING_INVALIDATE_SUBJECT,
                                okapi_engine::state::CacheInvalidation::Margin
                                    .to_wire()
                                    .into(),
                            )
                            .await
                    {
                        tracing::debug!(error = %err, "margin 变更广播失败（依赖 10s TTL 兜底）");
                    }
                }
                Ok(_) => {}
                Err(err) => tracing::error!(error = %err, "负毛利熔断评估失败"),
            }
        }
        MaintenanceJob::balance_expiry => {
            match expire_balances(pg, ledger, chrono::Utc::now()).await {
                Ok(expired) if !expired.is_empty() => {
                    tracing::warn!(count = expired.len(), "余额有效期到期已清零");
                }
                Ok(_) => {}
                Err(err) => tracing::error!(error = %err, "余额有效期清零失败"),
            }
            if let Err(err) = notify::balance_low_and_notify(pg, notifier).await {
                tracing::error!(error = %err, "余额低扫描失败");
            }
            if let Err(err) = crate::user_alerts::evaluate_and_notify(pg, &ctx.redis).await {
                tracing::error!(error = %err, "用户级告警评估失败");
            }
        }
        MaintenanceJob::manual_queues => {
            match notify::manual_queues_and_notify(pg, &ctx.redis, notifier).await {
                Ok((dlq, quarantined)) if dlq > 0 || quarantined > 0 => {
                    tracing::warn!(dlq, quarantined, "人工处理队列非空");
                }
                Ok(_) => {}
                Err(err) => tracing::error!(error = %err, "人工处理队列巡检失败"),
            }
        }
        MaintenanceJob::conversation_archive => {
            let policy = match load_conversation_archive_policy(pg).await {
                Ok(p) => p,
                Err(err) => {
                    tracing::error!(error = %err, "读取对话存档保留策略失败");
                    return;
                }
            };
            if policy.retention_days == 0 {
                return;
            }
            let now = chrono::Utc::now();
            let remote_ts = sched.maint_timestamp_get("conversation_archive").await;
            let due = {
                let mut local = last_archive_cleanup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *local = merge_latest_utc(*local, remote_ts);
                archive_cleanup_due(*local, now, policy.cleanup_interval_hours)
            };
            if !due {
                return;
            }
            match prune_conversation_archives(pg, ch, now).await {
                Ok(report) => {
                    *last_archive_cleanup
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(now);
                    let ttl_secs = i64::from(policy.cleanup_interval_hours)
                        .saturating_mul(7200)
                        .max(7 * 24 * 3600);
                    sched
                        .maint_timestamp_set("conversation_archive", now.timestamp(), ttl_secs)
                        .await;
                    if report.pg_deleted > 0 || report.ch_deleted_requested {
                        tracing::info!(
                            pg_deleted = report.pg_deleted,
                            ch_deleted_requested = report.ch_deleted_requested,
                            retention_days = policy.retention_days,
                            "对话存档定时清理已执行"
                        );
                    }
                }
                Err(err) => tracing::error!(error = %err, "对话存档定时清理失败"),
            }
        }
    }
}

// Provider latency and a PG outage must not block the other maintenance clocks.
pub(super) async fn run_recovery(
    state: okapi_engine::state::AppState,
    mut stop: tokio::sync::watch::Receiver<bool>,
    video: bool,
) {
    let mut tick = tokio::time::interval(if video {
        SWEEP_INTERVAL
    } else {
        Duration::from_secs(1)
    });
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _=stop.changed()=>return,
            _=tick.tick()=>{
                let work=async {
                    if video {okapi_engine::videos::poll_pending(&state).await}
                    else {okapi_engine::settlement_retry::recover(&state).await.map(|_|())}
                };
                tokio::select! {
                    _=stop.changed()=>return,
                    result=work=>{if let Err(error)=result {tracing::error!(%error,video,"background recovery deferred");}}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_round_ttl_and_lease_classification() {
        assert!(!MaintenanceJob::chsink_tick.needs_lease());
        assert!(MaintenanceJob::reconcile.needs_lease());
        assert!(MaintenanceJob::partition.needs_lease());
        assert_eq!(maintenance_round_ttl_secs(Duration::from_secs(1)), 2);
        assert_eq!(maintenance_round_ttl_secs(Duration::from_secs(30)), 24);
        assert_eq!(maintenance_round_ttl_secs(Duration::from_mins(5)), 240);
        assert_eq!(maintenance_round_ttl_secs(Duration::from_hours(6)), 3600);
    }

    #[test]
    fn reconcile_clock_hydrates_remote_checkpoint_across_replicas() {
        let base = chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("valid ts");
        let mut replica_b = ReconcileClock::default();
        assert!(
            replica_b.plan(base).is_none(),
            "fresh clock with no remote checkpoint runs full reconcile"
        );

        // Replica A finished a full pass 1h ago and an incremental pass 5m ago.
        let full_at = base - chrono::Duration::hours(1);
        let pass_at = base - chrono::Duration::minutes(5);
        replica_b.hydrate(Some(ReconcileCheckpoint {
            last_full_unix: Some(full_at.timestamp()),
            last_pass_unix: Some(pass_at.timestamp()),
        }));
        let since = replica_b
            .plan(base)
            .expect("replica B should inherit incremental window instead of repeating full pass");
        assert_eq!(
            since,
            pass_at - chrono::Duration::from_std(RECONCILE_OVERLAP).unwrap_or_default()
        );

        // Once 24h have elapsed since the remote full pass, a full pass is due again.
        let next_day = full_at + chrono::Duration::hours(24);
        assert!(replica_b.plan(next_day).is_none());
    }

    #[test]
    fn archive_cleanup_throttle_respects_remote_timestamp() {
        let now = chrono::DateTime::from_timestamp(1_700_010_000, 0).expect("valid ts");
        let two_hours_ago = (now - chrono::Duration::hours(2)).timestamp();
        let merged = merge_latest_utc(None, Some(two_hours_ago));
        assert!(!archive_cleanup_due(merged, now, 6));
        assert!(archive_cleanup_due(merged, now, 2));
    }
}
