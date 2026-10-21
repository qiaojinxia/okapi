//! sqlx creates and removes an isolated test database; no live account or price writes.
use okapi_store::{admin::RatioAxes, mutate, provision};
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
