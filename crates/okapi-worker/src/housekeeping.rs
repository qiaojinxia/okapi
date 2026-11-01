//! 例行维护：分区建删、余额到期、订阅滚窗、渠道 key 冷却恢复。
use super::SUBSCRIPTION_BATCH;
use okapi_ledger::BalanceLedger;
use sqlx::PgPool;

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
             FOR VALUES FROM ('{ny}-{nm:02}-01 00:00:00+00') TO ('{ey}-{em:02}-01 00:00:00+00')"
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
    let mut expired = Vec::new();
    let mut cursor = 0i64;
    loop {
        let user_ids = sqlx::query_scalar!("SELECT id FROM users WHERE balance_expires_at IS NOT NULL AND balance_expires_at < $1 AND deleted_at IS NULL AND id>$2 ORDER BY id LIMIT 1000",now,cursor).fetch_all(pg).await?;
        if user_ids.is_empty() {
            break;
        }
        for user_id in user_ids {
            cursor = user_id;
            // 与 sweep 同一口径：一个用户清不掉（锁超时、账本同步冲突）只跳过他，不中止整轮——
            // 否则下一轮又从头卡在同一个人身上，id 更大的用户的到期余额一直可花
            let drained = match okapi_ledger::operations::expire(pg, ledger, user_id, now).await {
                Ok(drained) => drained,
                Err(error) => {
                    tracing::error!(user_id, %error, "balance expiry failed for this user; skipped this round");
                    continue;
                }
            };
            if !drained.is_zero() {
                expired.push(ExpiredBalance {
                    user_id,
                    drained_micro: drained.as_micros(),
                });
            }
        }
    }
    Ok(expired)
}

/// 订阅滚窗 / 到期一轮（IMPLEMENTATION §11.28）。有订阅收回了分组就 INCR `auth:ver`
/// 让网关鉴权缓存失效（与 console 的 `auth_flush` 同一把键；worker 不持有 scheduler）。
pub async fn subscriptions_tick(
    pg: &PgPool,
    ledger: &BalanceLedger,
    redis: &fred::clients::Client,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<okapi_ledger::subscriptions::TickReport> {
    use fred::interfaces::KeysInterface as _;
    let report = okapi_ledger::subscriptions::tick(pg, ledger, now, SUBSCRIPTION_BATCH).await?;
    if report.group_changed {
        let bumped: Result<i64, _> = redis.incr("auth:ver").await;
        if let Err(err) = bumped {
            tracing::warn!(error = %err, "订阅到期收组后 auth:ver 失效失败（依赖 60s TTL 兜底）");
        }
    }
    Ok(report)
}

/// 数据保留策略（#1790-1）：settings.retention_months（缺省 0=永久保留），
/// DROP 超期的 PG 月分区（billing_records/billing_events/audit_logs 的 `_yYYYYmMM` 命名分区；
/// Financial facts are carried in the same transaction before DROP.
/// DEFAULT partitions remain, with expired rows archived/pruned in bounded batches; ClickHouse TTL is independent.
pub async fn drop_expired_partitions(
    pg: &PgPool,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<String>> {
    Ok(okapi_store::history::prune(pg, now).await?)
}

/// 冷却到期自动恢复 active（§3.4）：cooling(2)/rate_limited(3)/quota_exhausted(4)。
/// banned(5)/invalid(6) 仅人工恢复。
/// cooling 恢复保留连续失败计数（半开）：恢复后很快再失败才能逐轮翻倍退避，
/// 下一次成功会把它清零（`channels::clear_key_failures`）；限流 / 配额冷却与 5xx 无关，照旧清零。
pub async fn recover_cooled_keys(pg: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r"
        UPDATE channel_keys
        SET status = 1,
            failed_count = CASE WHEN status = 2 THEN failed_count ELSE 0 END,
            updated_at = now()
        WHERE status IN (2, 3, 4) AND cooldown_until IS NOT NULL AND cooldown_until < now()
        ",
    )
    .execute(pg)
    .await?;
    Ok(result.rows_affected())
}
