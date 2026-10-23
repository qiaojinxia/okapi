//! sqlx creates and removes an isolated test database; no live account or price writes.
use okapi_store::{
    admin::RatioAxes,
    channels::{self, KeyFailure},
    mutate, provision,
};
use sqlx::PgPool;

async fn channel(pool: &PgPool, name: &str) -> (i64, i64) {
    provision::create_channel(
        pool,
        name,
        "openai",
        "https://upstream.invalid/v1",
        "sk-test",
        &["m"],
        false,
        None,
    )
    .await
    .unwrap()
}

async fn key_status(pool: &PgPool, key: i64) -> i16 {
    sqlx::query_scalar("SELECT status FROM channel_keys WHERE id = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 批量软删只停用这次真正删掉的渠道的 key；早就软删的渠道不在这次操作的范围内
/// （与单删的 affected > 0 门控同口径）。
#[sqlx::test(migrations = "./migrations")]
async fn batch_delete_only_disables_keys_of_channels_it_actually_deleted(pool: PgPool) {
    let (fresh, fresh_key) = channel(&pool, "fresh").await;
    let (gone, gone_key) = channel(&pool, "gone").await;
    assert!(mutate::delete_channel(&pool, gone).await.unwrap());
    // 排障时人工把已软删渠道的 key 重新打开：批量删不该再碰它
    sqlx::query("UPDATE channel_keys SET status = 1 WHERE id = $1")
        .bind(gone_key)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(key_status(&pool, fresh_key).await, 1);

    let affected = mutate::batch_delete_channels(&pool, &[fresh, gone])
        .await
        .unwrap();
    assert_eq!(affected, 1);
    assert_eq!(
        key_status(&pool, fresh_key).await,
        2,
        "新删的渠道，key 同步停用"
    );
    assert_eq!(
        key_status(&pool, gone_key).await,
        1,
        "已软删渠道的 key 不在这次操作范围内"
    );

    // 全是已软删的：什么都不动
    let again = mutate::batch_delete_channels(&pool, &[fresh, gone])
        .await
        .unwrap();
    assert_eq!(again, 0);
    assert_eq!(key_status(&pool, gone_key).await, 1);
}

/// 从按次计价改回 ratio：按次单价必须一起清掉，否则它会被不看模式的对外清单（MCP、同步导出）列出来。
#[sqlx::test(migrations = "./migrations")]
async fn switching_back_to_ratio_drops_the_per_call_price(pool: PgPool) {
    let pricing = || async {
        sqlx::query_as::<_, (String, Option<i64>)>(
            "SELECT p.pricing_mode::text, p.per_call_price_micro
               FROM model_pricing p JOIN models m ON m.id = p.model_id
              WHERE m.model_name = 'switch-model'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    okapi_store::admin::upsert_model_per_call(&pool, "switch-model", 12_345)
        .await
        .unwrap();
    assert_eq!(pricing().await, ("per_call".to_owned(), Some(12_345)));

    okapi_store::admin::upsert_model_ratio(
        &pool,
        "switch-model",
        RatioAxes::basic("1", "4", "0.5"),
    )
    .await
    .unwrap();
    assert_eq!(pricing().await, ("ratio".to_owned(), None));
}

/// per_call ↔ tiered 互切：另一种模式的列（阶梯表 / 按次单价）也要一起清掉，理由同改回 ratio——
/// 它们会被不看模式的对外清单（MCP、同步导出）列出来，旧阶梯表还会在下次误切 tiered 时复活。
#[sqlx::test(migrations = "./migrations")]
async fn switching_between_per_call_and_tiered_drops_the_other_modes_columns(pool: PgPool) {
    let pricing = || async {
        sqlx::query_as::<_, (String, Option<i64>, Option<String>)>(
            "SELECT p.pricing_mode::text, p.per_call_price_micro, p.tier_expr
               FROM model_pricing p JOIN models m ON m.id = p.model_id
              WHERE m.model_name = 'mode-hop'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let axes = RatioAxes::basic("1", "4", "0.5");
    okapi_store::admin::upsert_model_tiered(&pool, "mode-hop", axes, "0:5,1000:1")
        .await
        .unwrap();
    assert_eq!(
        pricing().await,
        ("tiered".to_owned(), None, Some("0:5,1000:1".to_owned()))
    );

    okapi_store::admin::upsert_model_per_call(&pool, "mode-hop", 12_345)
        .await
        .unwrap();
    assert_eq!(
        pricing().await,
        ("per_call".to_owned(), Some(12_345), None),
        "改成按次计价要清掉旧阶梯表"
    );

    okapi_store::admin::upsert_model_tiered(&pool, "mode-hop", axes, "0:7,2000:2")
        .await
        .unwrap();
    assert_eq!(
        pricing().await,
        ("tiered".to_owned(), None, Some("0:7,2000:2".to_owned())),
        "改回阶梯计价要清掉旧按次单价"
    );
}

/// 会让整本价簿编译失败的脏值（负的按次单价、非正的专属绝对价）在写入这一步就被拦下，
/// 不是留到发布 / 装载快照时才炸。
#[sqlx::test(migrations = "./migrations")]
async fn dirty_pricing_values_are_refused_when_written(pool: PgPool) {
    okapi_store::admin::upsert_model_per_call(&pool, "guarded", 1_000)
        .await
        .unwrap();
    let negative = sqlx::query("UPDATE model_pricing SET per_call_price_micro = -1")
        .execute(&pool)
        .await;
    assert!(negative.is_err(), "负的按次单价必须被拒");
    okapi_store::admin::upsert_model_per_call(&pool, "guarded", 0)
        .await
        .expect("按次单价 0（免费）是合法值");

    let user = okapi_store::provision::create_user(&pool, "guard-user")
        .await
        .unwrap();
    let model_id: i64 = sqlx::query_scalar("SELECT id FROM models WHERE model_name = 'guarded'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let absolute = |input: i64, output: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO user_pricing
                    (user_id, model_id, override_kind, custom_input_per_1m_micro, custom_output_per_1m_micro)
                 VALUES ($1, $2, 'absolute', $3, $4)
                 ON CONFLICT (user_id, model_id) DO UPDATE SET
                    custom_input_per_1m_micro = EXCLUDED.custom_input_per_1m_micro,
                    custom_output_per_1m_micro = EXCLUDED.custom_output_per_1m_micro",
            )
            .bind(user)
            .bind(model_id)
            .bind(input)
            .bind(output)
            .execute(&pool)
            .await
        }
    };
    assert!(absolute(0, 10).await.is_err(), "输入单价必须为正");
    assert!(absolute(10, -1).await.is_err(), "输出单价不能为负");
    assert!(absolute(10, 0).await.is_ok(), "输出免费是合法的");
}

/// 迁移 0005：保留策略逐批清理 DEFAULT 分区用的 `ORDER BY created_at, <主键列> LIMIT 1000`
/// 有以 created_at 打头的索引可走；未知的 override_kind / 非对象的 tier_ratios 在写入时就被拦下；
/// 去掉重复的部分唯一索引后 aff_code 仍然唯一。
#[sqlx::test(migrations = "./migrations")]
async fn retention_indexes_and_schema_guards_from_migration_0005(pool: PgPool) {
    // 每张分区表的 DEFAULT 分区：保留批次的取数语句必须能走新索引（关掉顺序扫描才看得出"能不能走"）
    for (table, index, key) in [
        (
            "billing_records_default",
            "billing_records_default_created",
            "id",
        ),
        (
            "billing_events_default",
            "billing_events_default_created",
            "event_id",
        ),
        ("audit_logs_default", "audit_logs_default_created", "id"),
    ] {
        let mut tx = pool.begin().await.unwrap();
        // 空表的统计下规划器会选"位图扫描 + 排序"；关掉顺序扫描与位图扫描，只看索引能不能直接给出顺序
        for setting in ["enable_seqscan", "enable_bitmapscan"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("SET LOCAL {setting} = off")))
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        // 表名与列名是上面这张常量表里的，没有外部输入
        let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "EXPLAIN (FORMAT JSON) SELECT {key}, created_at FROM {table} \
             WHERE created_at < now() ORDER BY created_at, {key} LIMIT 1000"
        )))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let plan = plan.to_string();
        assert!(
            plan.contains(index),
            "{table} 的有序取数应走 {index}：{plan}"
        );
        assert!(!plan.contains("\"Sort\""), "{table} 不应再需要排序：{plan}");
    }

    // 值域守卫
    let user = provision::create_user(&pool, "guard-0005").await.unwrap();
    okapi_store::admin::upsert_model_per_call(&pool, "guarded-0005", 1_000)
        .await
        .unwrap();
    let model_id: i64 =
        sqlx::query_scalar("SELECT id FROM models WHERE model_name = 'guarded-0005'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let override_kind = |kind: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO user_pricing (user_id, model_id, override_kind, custom_model_ratio)
                 VALUES ($1, $2, $3, 1)
                 ON CONFLICT (user_id, model_id) DO UPDATE SET override_kind = EXCLUDED.override_kind",
            )
            .bind(user)
            .bind(model_id)
            .bind(kind)
            .execute(&pool)
            .await
        }
    };
    assert!(
        override_kind("bogus").await.is_err(),
        "未知的 override_kind 必须被拒"
    );
    assert!(override_kind("ratio").await.is_ok());
    assert!(
        override_kind("absolute").await.is_ok(),
        "absolute 也是已知的种类"
    );
    let tiers = |value: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query("UPDATE model_pricing SET tier_ratios = $1::jsonb WHERE model_id = $2")
                .bind(value)
                .bind(model_id)
                .execute(&pool)
                .await
        }
    };
    assert!(tiers("[]").await.is_err(), "数组形的 tier_ratios 必须被拒");
    assert!(
        tiers("\"flex\"").await.is_err(),
        "字符串形的 tier_ratios 必须被拒"
    );
    assert!(
        tiers("null").await.is_err(),
        "JSON null 不是对象（没有档位应写 SQL NULL）"
    );
    assert!(tiers(r#"{"flex":"0.5"}"#).await.is_ok());
    sqlx::query("UPDATE model_pricing SET tier_ratios = NULL WHERE model_id = $1")
        .bind(model_id)
        .execute(&pool)
        .await
        .expect("SQL NULL 表示没有档位，必须可写");

    // aff_code：重复的部分唯一索引没了，唯一性由 users_aff_code_key 保证
    let other = provision::create_user(&pool, "guard-0005-b").await.unwrap();
    let has_partial: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_indexes WHERE indexname = 'idx_users_aff_code')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!has_partial, "重复的部分唯一索引应已删除");
    let set = |id: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query("UPDATE users SET aff_code = 'dup-code' WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
        }
    };
    set(user).await.unwrap();
    let err = set(other).await.unwrap_err();
    assert!(
        err.as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation),
        "{err}"
    );
}

/// 配额耗尽的冷却写明「次日 0 点（UTC）」，与文档、与 `spend:ck:*` 的 UTC 日同一条边界。
/// 生产池的会话时区是本机时区（`connect_pg` 的 after_connect）：不显式换算的 `date_trunc('day', ..)`
/// 会落在本地午夜，偏出时区偏移那么多小时。
#[sqlx::test(migrations = "./migrations")]
async fn quota_exhaustion_cools_down_to_the_next_utc_midnight_in_any_session_zone(pool: PgPool) {
    let (_, key) = channel(&pool, "quota").await;
    for zone in ["Asia/Shanghai", "America/Los_Angeles"] {
        // 单连接池 + 非 UTC 的会话时区，复现生产池的状态
        let local = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(format!("SET TIME ZONE '{zone}'")))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        sqlx::query("UPDATE channel_keys SET status = 1, cooldown_until = NULL WHERE id = $1")
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();

        let before = chrono::Utc::now();
        channels::mark_key_failure(
            &local,
            key,
            "insufficient_quota",
            KeyFailure::QuotaExhausted,
        )
        .await
        .unwrap();
        let after = chrono::Utc::now();

        let (status, until): (i16, Option<chrono::DateTime<chrono::Utc>>) =
            sqlx::query_as("SELECT status, cooldown_until FROM channel_keys WHERE id = $1")
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, 4);
        let until = until.expect("配额耗尽要写冷却截止时间");
        assert_eq!(
            until.time(),
            chrono::NaiveTime::MIN,
            "{zone}：截止时间要落在 UTC 午夜，实际 {until}"
        );
        assert!(
            until > before && until <= after + chrono::Duration::days(1),
            "{zone}：应是下一个 UTC 午夜，实际 {until}"
        );
        local.close().await;
    }
}
