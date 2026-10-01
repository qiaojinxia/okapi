use crate::error::StoreError;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// 建立 PG 连接池。
///
/// 连接池大小：`OKAPI_PG_POOL` 覆写（缺省 16；8vCPU 单实例生产建议 32+，
/// 结算写入并发由 gateway 侧信号量钳制为本值一半，见 `AppState::settle_write`）。
///
/// **多副本必须自己算这笔账**：每个 pod 各开一套池，总连接数 = Σ(副本数 × 本值)。
/// 按 deploy/k8s 的 gateway HPA 上限 10 + console 2 + worker 1，走缺省 16 就是 208 条，
/// 而 PostgreSQL 缺省 `max_connections=100`——扩到一半开始报连接耗尽，且现象是
/// `acquire_timeout` 超时而不是"连不上"，很难一眼归因。所以启动即把生效值打进日志，
/// 让"这个 pod 到底占了几条"在 pod 日志里可查；部署侧的取值见 deploy/k8s/okapi.yaml。
pub async fn connect_pg(database_url: &str) -> Result<PgPool, StoreError> {
    let max = std::env::var("OKAPI_PG_POOL")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(16);
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
        "PG 连接池已建立（多副本部署：总连接数 = Σ 副本数 × 本值，须 ≤ PG max_connections）"
    );
    Ok(pool)
}

/// 运行嵌入式迁移（migrations/ 目录随二进制打包，部署零外部工具）。
pub async fn run_migrations(pool: &PgPool) -> Result<(), StoreError> {
    // Partition DDL and storage defaults remain UTC, independently of calendar reporting.
    let mut connection = pool.acquire().await?;
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
