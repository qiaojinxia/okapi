use crate::error::StoreError;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// 建立 PG 连接池。
///
/// 连接池大小：`OKAPI_PG_POOL` 覆写（缺省 16；8vCPU 单实例生产建议 32+，
/// 结算写入并发由 gateway 侧信号量钳制为本值一半，见 `AppState::settle_write`）。
///
/// **多副本必须自己算这笔账**：每个 pod 各开一套池，总连接数 = Σ(副本数 × 本值)；`okapi all`
/// 一个进程里 gateway / console / worker 各调一次本函数，占 3 × 本值。
/// 按 deploy/k8s 的 gateway HPA 上限 10 + console 2 + worker 1，走缺省 16 就是 208 条，
/// 而 PostgreSQL 缺省 `max_connections=100`——扩到一半开始报连接耗尽，且现象是
/// `acquire_timeout` 超时而不是"连不上"，很难一眼归因。所以启动即把生效值打进日志，
/// 让"这个 pod 到底占了几条"在 pod 日志里可查；部署侧的取值见 deploy/k8s/okapi.yaml。
pub async fn connect_pg(database_url: &str) -> Result<PgPool, StoreError> {
    let max = crate::env_config::number("OKAPI_PG_POOL", 16u32, |v| *v > 0);
    let pool = PgPoolOptions::new()
        .max_connections(max)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|connection, _| {
            Box::pin(async move {
                let zone = crate::timezone::machine_timezone()
                    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
                sqlx::query!("SELECT set_config('TimeZone',$1,false)", zone)
                    .fetch_one(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(database_url)
        .await?;
    tracing::info!(
        max_connections = max,
        "PG 连接池已建立（多副本部署：总连接数 = Σ 副本数 × 本值，all 模式每进程 3 份，须 ≤ PG max_connections）"
    );
    Ok(pool)
}

/// 迁移排队用的会话级咨询锁（"OKMIGRAT"）。
pub const MIGRATION_GATE: i64 = 0x4F4B_4D49_4752_4154;
/// 排队等门锁的上限；超过就报 `migration_gate_timeout`。
const MIGRATION_GATE_MAX_WAIT: Duration = Duration::from_mins(30);
/// 排队期间多久记一条"还在等"。
const MIGRATION_GATE_NOTICE_EVERY: Duration = Duration::from_mins(1);

/// 运行嵌入式迁移（migrations/ 目录随二进制打包，部署零外部工具）。
///
/// 单机模式的三个角色、多副本同时启动时都会一起跑迁移。sqlx 用阻塞的 `pg_advisory_lock`
/// 排队，等过 1 秒就记一条慢语句告警，每次部署都混进运维告警里。这里先用 try-lock
/// 轮询排队，轮到时 sqlx 自己那把锁已无人争用。
pub async fn run_migrations(pool: &PgPool) -> Result<(), StoreError> {
    run_migrations_within(pool, MIGRATION_GATE_MAX_WAIT).await
}

/// 同 [`run_migrations`]，排队等门锁最多 `max_wait`。迁移本身通常几秒；等过一分钟还没轮到，
/// 说明持锁的进程在跑很长的迁移或已经卡死（长事务堵着 DDL 之类），这时每分钟记一条告警说明在等，
/// 超过上限则明确报错——让编排层重启并留下日志，而不是让所有副本无声地永远挂在启动里。
pub async fn run_migrations_within(pool: &PgPool, max_wait: Duration) -> Result<(), StoreError> {
    let mut connection = pool.acquire().await?;
    let started = tokio::time::Instant::now();
    let mut last_notice = started;
    while !sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
        .bind(MIGRATION_GATE)
        .fetch_one(&mut *connection)
        .await?
    {
        let waited = started.elapsed();
        if waited >= max_wait {
            return Err(StoreError::MigrationGateTimeout(waited.as_secs()));
        }
        if last_notice.elapsed() >= MIGRATION_GATE_NOTICE_EVERY {
            tracing::warn!(
                waited_secs = waited.as_secs(),
                "正在等另一个进程跑完数据库迁移（门锁一直被占）"
            );
            last_notice = tokio::time::Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let migrated = migrate_in_utc(&mut connection).await;
    let unlocked = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_GATE)
        .fetch_one(&mut *connection)
        .await;
    if !matches!(unlocked, Ok(true)) {
        // 会话锁随连接走：放不掉就关掉这条连接，别让持锁的连接回池把别的进程堵住
        connection.close_on_drop();
    }
    migrated?;
    unlocked?;
    Ok(())
}

async fn migrate_in_utc(connection: &mut sqlx::PgConnection) -> Result<(), StoreError> {
    // Partition DDL and storage defaults remain UTC, independently of calendar reporting.
    sqlx::query!("SELECT set_config('TimeZone',$1,false)", "UTC")
        .fetch_one(&mut *connection)
        .await?;
    let migrated = sqlx::migrate!("./migrations").run(&mut *connection).await;
    let zone = crate::timezone::machine_timezone()?;
    sqlx::query!("SELECT set_config('TimeZone',$1,false)", zone)
        .fetch_one(&mut *connection)
        .await?;
    migrated?;
    Ok(())
}
