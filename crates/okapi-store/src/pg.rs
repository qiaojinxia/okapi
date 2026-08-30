use crate::error::StoreError;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// 建立 PG 连接池。
/// 连接池大小：`OKAPI_PG_POOL` 覆写（缺省 16；8vCPU 生产建议 32+，
/// 结算写入并发由 gateway 侧信号量钳制为本值一半，见 AppState::settle_write）。
pub async fn connect_pg(database_url: &str) -> Result<PgPool, StoreError> {
    let max = std::env::var("OKAPI_PG_POOL")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(16);
    let pool = PgPoolOptions::new()
        .max_connections(max)
        .acquire_timeout(Duration::from_secs(5))
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// 运行嵌入式迁移（migrations/ 目录随二进制打包，部署零外部工具）。
pub async fn run_migrations(pool: &PgPool) -> Result<(), StoreError> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
