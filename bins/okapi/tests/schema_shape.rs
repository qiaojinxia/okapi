//! 迁移产出的 schema 形状守卫。
//!
//! 0001 是把 16 个增量迁移压平的结果，压平最容易犯两类错：漏搬一列，或把已被
//! 推翻的中间态（如被渠道池取代的 group_channel_bindings）又搬回来。这里在临时
//! 库上核对若干"必须在"与"必须不在"，顺带验证 0001 能从零干净应用。

use sqlx::Row;
use uuid::Uuid;

/// 必须存在的表 → 关键列。只列压平时真正容易漏的（新表、后续加的列）。
const REQUIRED: &[(&str, &[&str])] = &[
    ("channel_pools", &["pool_code", "routing_strategy"]),
    ("pool_channels", &["pool_code", "channel_id"]),
    ("price_groups", &["group_ratio", "pool_code"]),
    (
        "api_keys",
        &["pool_override", "member_user_id", "group_override"],
    ),
    (
        "users",
        &["kind", "balance_expires_at", "aff_code", "inviter_id"],
    ),
    (
        "channel_keys",
        &[
            "model_subset",
            "rpm_limit",
            "daily_spend_cap_micro",
            "max_concurrency",
        ],
    ),
    ("models", &["fallback_models", "vendor"]),
    (
        "model_pricing",
        &[
            "cache_ratio",
            "cache_write_ratio",
            "audio_ratio",
            "audio_completion_ratio",
            "image_ratio",
            "tier_ratios",
            "tier_expr",
        ],
    ),
    ("user_pricing", &["custom_cache_write_ratio"]),
    (
        "redemption_codes",
        &["code_hash", "plan_id", "bind_user_id", "max_per_ip"],
    ),
    ("plans", &["plan_code", "grant_micro", "balance_valid_days"]),
    ("recharge_orders", &["order_no", "gateway"]),
    ("team_members", &["monthly_spend_limit_micro"]),
    ("oauth_identities", &["provider", "subject"]),
    ("model_aliases", &["pattern", "target_model"]),
    ("audit_logs", &["actor", "action"]),
];

/// 必须**不**存在：已被池取代的中间态。搬回来会让可见性同时有两处实现。
const FORBIDDEN_TABLES: &[&str] = &["group_channel_bindings"];

/// 明文不落库：这些列一旦出现，就是把密钥/兑换码明文写进了库。
const FORBIDDEN_COLUMNS: &[(&str, &str)] = &[
    ("redemption_codes", "code"),
    ("api_keys", "key"),
    ("users", "password"),
];

#[tokio::test]
async fn migrations_produce_expected_schema_shape() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");

    let admin_pool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!("okapi_shape_{}", &Uuid::new_v4().simple().to_string()[..12]);
    // 库名为本测试生成的随机标识符（无注入面），显式审计标注
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin_pool)
    .await
    .unwrap();

    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let fresh_url = format!("{base}/{db_name}");
    let fresh = okapi_store::connect_pg(&fresh_url).await.unwrap();
    okapi_store::run_migrations(&fresh)
        .await
        .expect("0001 应能从零干净应用");

    let mut problems: Vec<String> = Vec::new();

    for (table, columns) in REQUIRED {
        for column in *columns {
            let found: i64 = sqlx::query(
                "SELECT count(*) FROM information_schema.columns
                 WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
            )
            .bind(table)
            .bind(column)
            .fetch_one(&fresh)
            .await
            .unwrap()
            .get(0);
            if found == 0 {
                problems.push(format!("缺列 {table}.{column}"));
            }
        }
    }

    for table in FORBIDDEN_TABLES {
        let found: i64 = sqlx::query(
            "SELECT count(*) FROM information_schema.tables
             WHERE table_schema = 'public' AND table_name = $1",
        )
        .bind(table)
        .fetch_one(&fresh)
        .await
        .unwrap()
        .get(0);
        if found > 0 {
            problems.push(format!("已废弃的表又出现了：{table}"));
        }
    }

    for (table, column) in FORBIDDEN_COLUMNS {
        let found: i64 = sqlx::query(
            "SELECT count(*) FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_one(&fresh)
        .await
        .unwrap()
        .get(0);
        if found > 0 {
            problems.push(format!("明文列不应存在：{table}.{column}"));
        }
    }

    // 路由策略取值受 CHECK 约束，写错值应当被库拒绝
    let bad = sqlx::query(
        "INSERT INTO channel_pools (pool_code, routing_strategy) VALUES ('t', 'nonsense')",
    )
    .execute(&fresh)
    .await;
    if bad.is_ok() {
        problems.push("channel_pools.routing_strategy 缺 CHECK 约束".to_owned());
    }

    drop(fresh);
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#
    )))
    .execute(&admin_pool)
    .await;

    assert!(problems.is_empty(), "schema 形状不符：{problems:#?}");
}
