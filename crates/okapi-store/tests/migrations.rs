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
