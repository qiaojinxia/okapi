//! sqlx creates and removes an isolated, empty test database.
use sqlx::PgPool;

/// 单机模式三个角色同时启动：都跑迁移，排队后各自成功，结束时不留咨询锁。
#[sqlx::test(migrations = false)]
async fn concurrent_starts_queue_and_release_the_gate(pool: PgPool) {
    let (a, b, c) = tokio::join!(
        okapi_store::run_migrations(&pool),
        okapi_store::run_migrations(&pool),
        okapi_store::run_migrations(&pool),
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(applied > 0);
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' \
         AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(held, 0);
}

/// 门锁被别的会话一直占着：排队有上限，超时明确报 `migration_gate_timeout`，而不是无声地永远挂着；
/// 锁放开后照常迁移。
#[sqlx::test(migrations = false)]
async fn a_stuck_migration_gate_times_out_instead_of_hanging(pool: PgPool) {
    let mut holder = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(okapi_store::pg::MIGRATION_GATE)
        .execute(&mut *holder)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let error =
        okapi_store::pg::run_migrations_within(&pool, std::time::Duration::from_millis(400))
            .await
            .unwrap_err();
    assert!(
        matches!(error, okapi_store::StoreError::MigrationGateTimeout(_)),
        "{error}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(okapi_store::pg::MIGRATION_GATE)
        .execute(&mut *holder)
        .await
        .unwrap();
    okapi_store::run_migrations(&pool).await.unwrap();
}
