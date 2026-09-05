//! worker 角色：异步面（IMPLEMENTATION §2.1）。
//!
//! M2 第一批（无 CH/NATS 依赖的正确性核心）：
//! - 悬置预扣清理：超时未结算的 Redis 预扣按 deadline 释放并记事件；
//! - 三方对账 reconciler：billing_events 重放 ↔ Redis 热余额（含在途）↔ users.balance_micro；
//! - 分区维护：提前创建下月分区（docs/database.md §0）；
//! - 渠道 key 冷却恢复：cooldown 到期自动回 active。
//!
//! chsink（outbox → ClickHouse）已接入（单机直连形态）；NATS 传输在后续批次拆分。

pub mod chsink;
pub mod nats_relay;
pub mod notify;

use crate::config::Config;
use okapi_ledger::BalanceLedger;
use sqlx::PgPool;
use std::time::Duration;

const SWEEP_INTERVAL: Duration = Duration::from_mins(1);
const RECONCILE_INTERVAL: Duration = Duration::from_mins(5);
const PARTITION_INTERVAL: Duration = Duration::from_hours(6);
const COOLDOWN_INTERVAL: Duration = Duration::from_secs(30);
const BALANCE_EXPIRY_INTERVAL: Duration = Duration::from_mins(5);
/// 对账每轮抽样的用户数上限。
const RECONCILE_BATCH: i64 = 1000;

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
    let ledger = BalanceLedger::new(redis);

    let ch = if let Some(url) = &cfg.clickhouse_url {
        let client = okapi_store::ChClient::new(url, "okapi")
            .map_err(|e| anyhow::anyhow!("clickhouse client: {e}"))?;
        if client.ping().await {
            client
                .ensure_schema()
                .await
                .map_err(|e| anyhow::anyhow!("clickhouse schema: {e}"))?;
            tracing::info!("chsink 就绪（outbox → ClickHouse）");
            Some(client)
        } else {
            tracing::warn!("ClickHouse 不可达，chsink 停用（统计接口 fail-closed）");
            None
        }
    } else {
        tracing::info!("未配置 OKAPI_CLICKHOUSE_URL，chsink 停用");
        None
    };

    let js = connect_jetstream(cfg.nats_url.as_deref()).await;

    tracing::info!("okapi worker 启动（relay/chsink/sweep/reconcile/partition/cooldown）");

    let mut chsink_tick = tokio::time::interval(Duration::from_secs(1));
    let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
    let mut reconcile = tokio::time::interval(RECONCILE_INTERVAL);
    let mut partition = tokio::time::interval(PARTITION_INTERVAL);
    let mut cooldown = tokio::time::interval(COOLDOWN_INTERVAL);
    let mut balance_expiry = tokio::time::interval(BALANCE_EXPIRY_INTERVAL);

    loop {
        tokio::select! {
            _ = chsink_tick.tick() => transport_tick(&pg, js.as_ref(), ch.as_ref()).await,
            _ = sweep.tick() => {
                match sweep_expired_reservations(&pg, &ledger, chrono::Utc::now()).await {
                    Ok(swept) if !swept.is_empty() => {
                        tracing::warn!(count = swept.len(), "悬置预扣已清理");
                    }
                    Ok(_) => {}
                    Err(err) => tracing::error!(error = %err, "悬置预扣清理失败"),
                }
            }
            _ = reconcile.tick() => {
                match reconcile_balances(&pg, &ledger, RECONCILE_BATCH).await {
                    Ok(drifts) if !drifts.is_empty() => {
                        for d in &drifts {
                            tracing::error!(
                                user_id = d.user_id,
                                events_sum = d.events_sum_micro,
                                redis_effective = d.redis_effective_micro,
                                pg_snapshot = d.pg_snapshot_micro,
                                "对账差异（不会自愈：按账本修复走 /admin/reconciliation/repair）"
                            );
                        }
                        let users: Vec<i64> = drifts.iter().take(20).map(|d| d.user_id).collect();
                        notifier
                            .dispatch(
                                "drift",
                                &serde_json::json!({ "count": drifts.len(), "user_ids": users }),
                            )
                            .await;
                    }
                    Ok(_) => tracing::debug!("对账零差异"),
                    Err(err) => tracing::error!(error = %err, "对账失败"),
                }
            }
            _ = partition.tick() => {
                match ensure_next_month_partitions(&pg, chrono::Utc::now()).await {
                    Ok(created) if !created.is_empty() => {
                        tracing::info!(?created, "已创建下月分区");
                    }
                    Ok(_) => {}
                    Err(err) => tracing::error!(error = %err, "分区维护失败"),
                }
                match drop_expired_partitions(&pg, chrono::Utc::now()).await {
                    Ok(dropped) if !dropped.is_empty() => {
                        tracing::warn!(?dropped, "保留策略已裁剪超期分区");
                    }
                    Ok(_) => {}
                    Err(err) => tracing::error!(error = %err, "保留策略裁剪失败"),
                }
            }
            _ = cooldown.tick() => {
                match recover_cooled_keys(&pg).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(recovered = n, "渠道 key 冷却到期恢复"),
                    Err(err) => tracing::error!(error = %err, "冷却恢复失败"),
                }
                if let Ok(cooling) = notify::count_cooling_keys(&pg).await
                    && cooling > 0
                {
                    notifier
                        .dispatch("channel_cooldown", &serde_json::json!({ "count": cooling }))
                        .await;
                }
            }
            _ = balance_expiry.tick() => {
                match expire_balances(&pg, &ledger, chrono::Utc::now()).await {
                    Ok(expired) if !expired.is_empty() => {
                        tracing::warn!(count = expired.len(), "余额有效期到期已清零");
                    }
                    Ok(_) => {}
                    Err(err) => tracing::error!(error = %err, "余额有效期清零失败"),
                }
                match notify::scan_balance_low(&pg).await {
                    Ok(low) if !low.is_empty() => {
                        let users: Vec<_> = low
                            .iter()
                            .map(|(id, bal)| serde_json::json!({ "user_id": id, "balance_micro": bal }))
                            .collect();
                        notifier
                            .dispatch("balance_low", &serde_json::json!({ "users": users }))
                            .await;
                    }
                    Ok(_) => {}
                    Err(err) => tracing::error!(error = %err, "余额低扫描失败"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("worker 收到退出信号");
                return Ok(());
            }
        }
    }
}

/// 已清理的悬置预扣。
#[derive(Debug)]
pub struct SweptReservation {
    pub user_id: i64,
    pub request_id: uuid::Uuid,
    /// refund 路径 = 释放金额；commit 补偿路径 = 补扣后的净释放（reserved − actual）。
    pub released_micro: i64,
    /// "refund" | "commit_replayed"。
    pub action: &'static str,
}

/// 悬置预扣清理（docs/database.md §5）：deadline 已过的在途预扣，
/// **按 billing_records 终态决定补偿方向**。
///
/// - 终态 committed（PG 结算成功、Redis commit 丢失）→ 重放 commit 补扣实际金额；
/// - 终态 refunded/failed 或无记录（请求死亡）→ 全额 refund。
///
/// 场景：gateway 崩溃/断电/Redis 抖动导致 reserve 后未终结。幂等。
// 终态判定 + 两种补偿路径的完整语义放同一视野更可读
#[allow(clippy::too_many_lines)]
pub async fn sweep_expired_reservations(
    pg: &PgPool,
    ledger: &BalanceLedger,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<SweptReservation>> {
    // 用户全集驱动（开发/中小规模足够；大规模换 Redis SCAN，见 docs/database.md §5）
    let user_ids = sqlx::query_scalar!(r#"SELECT id FROM users WHERE deleted_at IS NULL"#)
        .fetch_all(pg)
        .await?;

    let now_ms = now.timestamp_millis();
    let mut swept = Vec::new();
    for user_id in user_ids {
        for reservation in ledger.list_reservations(user_id).await? {
            if reservation.deadline_ms >= now_ms {
                continue;
            }
            let terminal = sqlx::query!(
                r#"
                SELECT status, amount_micro FROM billing_records
                WHERE request_id = $1
                ORDER BY created_at DESC
                LIMIT 1
                "#,
                reservation.request_id
            )
            .fetch_optional(pg)
            .await?;

            if let Some(rec) = &terminal
                && rec.status == 20
            {
                // PG 已结算：重放 Redis commit（补扣 actual，多退少补）；
                // commit 事件已在结算事务中写过，此处不重复记账。
                let outcome = ledger
                    .commit(
                        user_id,
                        reservation.api_key_id,
                        reservation.request_id,
                        okapi_domain::Money::from_micros(rec.amount_micro),
                    )
                    .await?;
                if let okapi_ledger::CommitOutcome::Committed { refund_delta, .. } = outcome {
                    swept.push(SweptReservation {
                        user_id,
                        request_id: reservation.request_id,
                        released_micro: refund_delta.as_micros(),
                        action: "commit_replayed",
                    });
                }
                continue;
            }

            let (released, _balance) = ledger
                .refund(user_id, reservation.api_key_id, reservation.request_id)
                .await?;
            if released.is_zero() {
                continue; // 竞争：已被正常终结
            }
            sqlx::query!(
                r#"
                INSERT INTO billing_events (user_id, request_id, event_type, delta_micro, payload, actor)
                VALUES ($1, $2, 'refund', 0, $3, 'system:worker')
                "#,
                user_id,
                reservation.request_id,
                serde_json::json!({
                    "reason": "reservation_expired",
                    "released_micro": released.as_micros(),
                })
            )
            .execute(pg)
            .await?;
            swept.push(SweptReservation {
                user_id,
                request_id: reservation.request_id,
                released_micro: released.as_micros(),
                action: "refund",
            });
        }
    }
    Ok(swept)
}

/// 对账差异行。
#[derive(Debug)]
pub struct BalanceDrift {
    pub user_id: i64,
    /// 真理源：billing_events 重放。
    pub events_sum_micro: i64,
    /// Redis 有效余额 = avail + 在途预扣（消除在途噪声后应等于事件和）。
    pub redis_effective_micro: i64,
    /// users.balance_micro 快照列。
    pub pg_snapshot_micro: i64,
}

/// 三方对账（docs/database.md §5）：返回不一致的用户。
pub async fn reconcile_balances(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
) -> anyhow::Result<Vec<BalanceDrift>> {
    let rows = sqlx::query!(
        r#"
        SELECT u.id AS user_id,
               u.balance_micro,
               COALESCE(e.sum_delta, 0)::bigint AS "events_sum!"
        FROM users u
        LEFT JOIN (
            SELECT user_id, SUM(delta_micro) AS sum_delta
            FROM billing_events
            GROUP BY user_id
        ) e ON e.user_id = u.id
        WHERE u.deleted_at IS NULL
        ORDER BY u.id
        LIMIT $1
        "#,
        limit
    )
    .fetch_all(pg)
    .await?;

    let mut drifts = Vec::new();
    for row in rows {
        let avail = ledger.balance(row.user_id).await?.as_micros();
        let inflight: i64 = ledger
            .list_reservations(row.user_id)
            .await?
            .iter()
            .map(|r| r.amount.as_micros())
            .sum();
        let redis_effective = avail.saturating_add(inflight);
        if redis_effective != row.events_sum || row.balance_micro != row.events_sum {
            drifts.push(BalanceDrift {
                user_id: row.user_id,
                events_sum_micro: row.events_sum,
                redis_effective_micro: redis_effective,
                pg_snapshot_micro: row.balance_micro,
            });
        }
    }
    Ok(drifts)
}

/// 一个用户的对账修复结果。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct BalanceRepair {
    pub user_id: i64,
    /// 账本权威值（`billing_events` 求和）。
    pub events_sum_micro: i64,
    pub redis_before_micro: i64,
    pub redis_after_micro: i64,
    /// 被保留的在途预扣合计（`avail + 在途 == 账本` 才是对账不变式）。
    pub inflight_micro: i64,
    pub pg_snapshot_before_micro: i64,
}

/// 按账本重建单个用户的热余额与展示快照。返回 None = 用户不存在/已删除。
///
/// 先写 Redis 再写 PG 快照：Redis 是**唯一**决定能不能扣费的那份数据（`reserve.lua`
/// 只读它、不回源 PG），中途失败的话宁可留一个滞后的展示快照，也不要留一个仍然
/// 拒服务的热账本。两步都幂等，重跑即可收敛。
pub async fn repair_balance(
    pg: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
) -> anyhow::Result<Option<BalanceRepair>> {
    let Some(row) = sqlx::query!(
        r#"
        SELECT u.balance_micro,
               COALESCE((SELECT SUM(delta_micro) FROM billing_events e WHERE e.user_id = u.id), 0)::bigint
                   AS "events_sum!"
        FROM users u WHERE u.id = $1 AND u.deleted_at IS NULL
        "#,
        user_id
    )
    .fetch_optional(pg)
    .await?
    else {
        return Ok(None);
    };
    let target = okapi_domain::Money::from_micros(row.events_sum);
    let outcome = ledger.repair(user_id, target).await?;
    sqlx::query!(
        r#"UPDATE users SET balance_micro = $2, updated_at = now() WHERE id = $1"#,
        user_id,
        row.events_sum
    )
    .execute(pg)
    .await?;
    Ok(Some(BalanceRepair {
        user_id,
        events_sum_micro: row.events_sum,
        redis_before_micro: outcome.before.as_micros(),
        redis_after_micro: outcome.after.as_micros(),
        inflight_micro: outcome.inflight.as_micros(),
        pg_snapshot_before_micro: row.balance_micro,
    }))
}

/// 结算窗口：Redis commit 与 PG 事件落库不在同一瞬间（结算是响应返回后的后台任务，
/// 还要过 `settle_gate` 信号量），窗口内 Redis 比账本**低一笔**——对账会把它看成漂移。
///
/// 这对修复是致命的：照着尚未记入那笔扣费的账本去重建，等于把正在结算的钱退回去。
/// 所以修复前采两次样，只有两次完全一致才动手。
const SETTLE_WINDOW: Duration = Duration::from_millis(1500);

/// 漂移是否稳定（不是结算窗口的瞬时态）。稳定则返回该用户的账本权威值。
pub async fn stable_drift(
    pg: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
) -> anyhow::Result<Option<i64>> {
    let sample = |uid: i64| async move {
        let row = sqlx::query!(
            r#"
            SELECT COALESCE((SELECT SUM(delta_micro) FROM billing_events e WHERE e.user_id = $1), 0)::bigint
                       AS "events_sum!"
            FROM users u WHERE u.id = $1 AND u.deleted_at IS NULL
            "#,
            uid
        )
        .fetch_optional(pg)
        .await?;
        let Some(row) = row else {
            return Ok::<Option<(i64, i64)>, anyhow::Error>(None);
        };
        let avail = ledger.balance(uid).await?.as_micros();
        let inflight: i64 = ledger
            .list_reservations(uid)
            .await?
            .iter()
            .map(|r| r.amount.as_micros())
            .sum();
        Ok(Some((row.events_sum, avail.saturating_add(inflight))))
    };

    let Some(first) = sample(user_id).await? else {
        return Ok(None);
    };
    tokio::time::sleep(SETTLE_WINDOW).await;
    let Some(second) = sample(user_id).await? else {
        return Ok(None);
    };
    Ok((first == second).then_some(second.0))
}

/// 批量修复当前扫出的全部漂移用户。
///
/// 两遍对账中间隔一个结算窗口，只修**两遍都在漂且数字没变**的用户——有流量时总有请求
/// 正处在「Redis 已扣、PG 事件还没落」的中间态，一遍扫描分不清它和真丢数据。
pub async fn repair_drifted(
    pg: &PgPool,
    ledger: &BalanceLedger,
    limit: i64,
) -> anyhow::Result<Vec<BalanceRepair>> {
    let first = reconcile_balances(pg, ledger, limit).await?;
    if first.is_empty() {
        return Ok(Vec::new());
    }
    tokio::time::sleep(SETTLE_WINDOW).await;
    let second = reconcile_balances(pg, ledger, limit).await?;
    let stable: std::collections::HashMap<i64, &BalanceDrift> =
        second.iter().map(|d| (d.user_id, d)).collect();

    let mut out = Vec::new();
    for d in &first {
        // 两遍数字完全一致才算真漂移；结算中的账户两遍必然不同，跳过
        let Some(again) = stable.get(&d.user_id) else {
            continue;
        };
        if again.events_sum_micro != d.events_sum_micro
            || again.redis_effective_micro != d.redis_effective_micro
        {
            continue;
        }
        if let Some(fixed) = repair_balance(pg, ledger, d.user_id).await? {
            out.push(fixed);
        }
    }
    Ok(out)
}

/// 提前创建下月分区（本月数据走当月/DEFAULT 分区；未来行仅来自时钟漂移，忽略）。
pub async fn ensure_next_month_partitions(
    pg: &PgPool,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<String>> {
    use chrono::Datelike;
    let (y, m) = (now.year(), now.month());
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let (ey, em) = if nm == 12 { (ny + 1, 1) } else { (ny, nm + 1) };

    let mut created = Vec::new();
    for table in ["billing_records", "billing_events", "audit_logs"] {
        let name = format!("{table}_y{ny}m{nm:02}");
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM pg_class WHERE relname = $1) AS "exists!""#,
            name
        )
        .fetch_one(pg)
        .await?;
        if exists {
            continue;
        }
        // DDL 无法参数化；标识符与日期均为内部生成（无注入面），显式声明 SqlSafe
        let ddl = format!(
            "CREATE TABLE IF NOT EXISTS {name} PARTITION OF {table} \
             FOR VALUES FROM ('{ny}-{nm:02}-01') TO ('{ey}-{em:02}-01')"
        );
        sqlx::query(sqlx::AssertSqlSafe(ddl)).execute(pg).await?;
        created.push(name);
    }
    Ok(created)
}

/// 已过期清零的用户余额。
#[derive(Debug)]
pub struct ExpiredBalance {
    pub user_id: i64,
    pub drained_micro: i64,
}

/// 余额有效期到期清零（#1790-6）：balance_expires_at 已过的用户，
/// 原子取出可用余额 → 事件留痕（event_type='expire'，delta 为负）→ 重置 NULL 防重扫。
/// 在途预扣不动（按各自结算/退款路径终结）；充值不自动延期（延期策略列 backlog）。
pub async fn expire_balances(
    pg: &PgPool,
    ledger: &BalanceLedger,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<ExpiredBalance>> {
    let user_ids = sqlx::query_scalar!(
        r#"
        SELECT id FROM users
        WHERE balance_expires_at IS NOT NULL AND balance_expires_at < $1 AND deleted_at IS NULL
        "#,
        now
    )
    .fetch_all(pg)
    .await?;

    let mut expired = Vec::new();
    for user_id in user_ids {
        let drained = ledger.drain(user_id).await?;
        if !drained.is_zero() {
            okapi_ledger::pg::record_credit(
                pg,
                user_id,
                okapi_domain::Money::from_micros(drained.as_micros().saturating_neg()),
                "expire",
                "system:worker",
                serde_json::json!({ "reason": "balance_expired" }),
            )
            .await?;
            expired.push(ExpiredBalance {
                user_id,
                drained_micro: drained.as_micros(),
            });
        }
        // 无论清了多少都重置，防止每轮重扫（清零与重置间崩溃 → 下轮 drain=0 只走这步）
        sqlx::query!(
            r#"UPDATE users SET balance_expires_at = NULL, updated_at = now() WHERE id = $1"#,
            user_id
        )
        .execute(pg)
        .await?;
    }
    Ok(expired)
}

/// 数据保留策略（#1790-1）：settings.retention_months（缺省 0=永久保留），
/// DROP 超期的 PG 月分区（billing_records/billing_events/audit_logs 的 `_yYYYYmMM` 命名分区；
/// DEFAULT 分区不动）。CH 侧 TTL（180d）独立，调整走管理员 ALTER（backlog）。
pub async fn drop_expired_partitions(
    pg: &PgPool,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<String>> {
    use chrono::Datelike;
    let months = sqlx::query_scalar!(
        r#"SELECT (value #>> '{}')::bigint AS "v!" FROM settings WHERE key = 'retention_months'"#
    )
    .fetch_optional(pg)
    .await?
    .unwrap_or(0);
    if months <= 0 {
        return Ok(Vec::new());
    }
    // 截止月（含当月往前推 months 个月之前的分区全删）
    let total = i64::from(now.year()) * 12 + i64::from(now.month()) - 1 - months;
    let (cut_y, cut_m) = (total.div_euclid(12), total.rem_euclid(12) + 1);

    let rows = sqlx::query_scalar!(
        r#"SELECT relname AS "name!" FROM pg_class
           WHERE relname ~ '^(billing_records|billing_events|audit_logs)_y\d{4}m\d{2}$'"#
    )
    .fetch_all(pg)
    .await?;
    let mut dropped = Vec::new();
    for name in rows {
        let Some(pos) = name.rfind("_y") else {
            continue;
        };
        let tail = &name[pos + 2..];
        let Some((y, m)) = tail.split_once('m') else {
            continue;
        };
        let (Ok(y), Ok(m)) = (y.parse::<i64>(), m.parse::<i64>()) else {
            continue;
        };
        if y * 12 + m - 1 < cut_y * 12 + cut_m {
            // 标识符为内部命名模式匹配产物（无注入面）
            let ddl = format!("DROP TABLE IF EXISTS {name}");
            sqlx::query(sqlx::AssertSqlSafe(ddl)).execute(pg).await?;
            dropped.push(name);
        }
    }
    Ok(dropped)
}

/// 冷却到期自动恢复 active（§3.4）：cooling(2)/rate_limited(3)/quota_exhausted(4)。
/// banned(5)/invalid(6) 仅人工恢复。
pub async fn recover_cooled_keys(pg: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query!(
        r#"
        UPDATE channel_keys
        SET status = 1, failed_count = 0, updated_at = now()
        WHERE status IN (2, 3, 4) AND cooldown_until IS NOT NULL AND cooldown_until < now()
        "#
    )
    .execute(pg)
    .await?;
    Ok(result.rows_affected())
}
