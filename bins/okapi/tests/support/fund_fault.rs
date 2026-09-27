use sqlx::PgPool;

// Isolated test fixture: reject only this test's new user at the final intent
// insert. The constraint is removed before observing/asserting the HTTP result.
pub async fn reject(pg: &PgPool, user_id: i64) -> String {
    let name = format!("fund_fault_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE fund_transfers ADD CONSTRAINT {name} CHECK(user_id<>{user_id}) NOT VALID"
    )))
    .execute(pg)
    .await
    .unwrap();
    name
}
pub async fn restore(pg: &PgPool, name: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE fund_transfers DROP CONSTRAINT {name}"
    )))
    .execute(pg)
    .await
    .unwrap();
}
pub async fn pending_count(pg: &PgPool, user_id: i64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM fund_transfers WHERE user_id=$1")
        .bind(user_id)
        .fetch_one(pg)
        .await
        .unwrap()
}
