//! 全新库上的第一次定价发布必须能热更（DESIGN §3.3 失效路径）。
//! `pricing_epochs` 为空时价簿 epoch 读作 0：首次发布拿到的 epoch 正是 1（IDENTITY 从 1 起），
//! 若空表读作 1，在跑的 gateway 会把第一次发布判成"不比当前新"而永不装载，直到第二次发布或重启。
//! 用独立临时库（共享库的 epochs 表从不为空）。依赖 .env（scripts/dev-deps.sh up）。

use okapi::gateway;
use uuid::Uuid;

#[tokio::test]
async fn first_publish_on_fresh_database_hot_reloads() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    let admin_pool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!(
        "okapi_epoch0_{}",
        &Uuid::new_v4().simple().to_string()[..12]
    );
    // 库名为本测试生成的随机标识符（无注入面）
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin_pool)
    .await
    .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let fresh_url = format!("{base}/{db_name}");

    let state = gateway::build_state(&fresh_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    assert_eq!(state.pricebook.epoch(), 0, "空表不得冒充已发布过一次");
    assert!(
        !gateway::refresh_pricebook_if_newer(&state).await.unwrap(),
        "没有发布就没有热更"
    );

    let admin = okapi_store::provision::create_user(&state.pg, "epoch-admin")
        .await
        .unwrap();
    let published = okapi_store::admin::publish_epoch(
        &state.pg,
        admin,
        &serde_json::json!({"reason": "first"}),
    )
    .await
    .unwrap();
    assert_eq!(published, 1);
    assert!(
        gateway::refresh_pricebook_if_newer(&state).await.unwrap(),
        "第一次发布必须被判为更新"
    );
    assert_eq!(state.pricebook.epoch(), 1);
    assert!(
        !gateway::refresh_pricebook_if_newer(&state).await.unwrap(),
        "同一 epoch 不重复装载"
    );

    state.pg.close().await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#
    )))
    .execute(&admin_pool)
    .await;
}
