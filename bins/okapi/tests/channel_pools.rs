//! 渠道池语义验收（docs/database.md §3.7）。
//!
//! 池把"付多少钱"（price_groups.group_ratio）与"打哪些上游"拆开。这里守住四条：
//! 分组的池限定候选、令牌 pool_override 优先于分组、无池 = 全部可见、
//! 以及 per-key 模型子集能在同一渠道内区分不同 key 的权限。
//!
//! 直接打 store 的候选查询而不走 HTTP：可见性是选路语义，
//! 断在候选集合上比断在响应体上更贴近被测对象。

use sqlx::PgPool;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Env {
    pg: PgPool,
    model: String,
    /// stable 池内两个渠道，fast 池内一个（其中一个渠道同属两池）。
    stable_only: i64,
    both: i64,
    vip_group: String,
    fast_pool: String,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let sfx = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let model = format!("m-pool-{sfx}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();

    let stable_pool = format!("p-stable-{sfx}");
    let fast_pool = format!("p-fast-{sfx}");
    for code in [&stable_pool, &fast_pool] {
        okapi_store::admin::upsert_channel_pool(&pg, code, "t", "priority_weighted")
            .await
            .unwrap();
    }

    let (stable_only, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-s-{sfx}"),
        "openai",
        "https://api.openai.com/v1",
        "cred",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    let (both, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("ch-b-{sfx}"),
        "openai",
        "https://api.openai.com/v1",
        "cred",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    okapi_store::admin::set_channel_pools(&pg, stable_only, std::slice::from_ref(&stable_pool))
        .await
        .unwrap();
    okapi_store::admin::set_channel_pools(&pg, both, &[stable_pool, fast_pool.clone()])
        .await
        .unwrap();

    let vip_group = format!("g-vip-{sfx}");
    okapi_store::admin::upsert_price_group(&pg, &vip_group, "0.85", "vip", Some(&fast_pool))
        .await
        .unwrap();

    Env {
        pg,
        model,
        stable_only,
        both,
        vip_group,
        fast_pool,
    }
}

/// 建一个属于指定分组的用户与令牌，返回鉴权解析出的 AuthedKey。
async fn key_in_group(
    pg: &PgPool,
    group: Option<&str>,
    pool_override: Option<&str>,
) -> okapi_store::auth::AuthedKey {
    let sfx = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let uid = okapi_store::provision::create_user(pg, &format!("u-pool-{sfx}"))
        .await
        .unwrap();
    if let Some(g) = group {
        okapi_store::admin::set_user_groups(pg, uid, &[(g.to_owned(), 10)])
            .await
            .unwrap();
    }
    let token = format!("sk-okapi-pool-{sfx}");
    okapi_store::provision::create_api_key(pg, uid, &hash(&token), "sk-pool")
        .await
        .unwrap();
    if let Some(p) = pool_override {
        sqlx::query!(
            "UPDATE api_keys SET pool_override = $1 WHERE user_id = $2",
            p,
            uid
        )
        .execute(pg)
        .await
        .unwrap();
    }
    okapi_store::auth::find_key_by_hash(pg, &hash(&token))
        .await
        .unwrap()
        .expect("令牌应可鉴权")
}

async fn candidate_channels(pg: &PgPool, model: &str, pool: Option<&str>) -> Vec<i64> {
    let mut ids: Vec<i64> = okapi_store::channels::candidates_for_model(pg, model, pool, None)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.channel_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[tokio::test]
async fn pool_scopes_candidates_and_key_override_wins() {
    let env = setup().await;

    // 1) 分组的池限定候选：vip → fast 池，只应看到同属两池的那个渠道
    let vip = key_in_group(&env.pg, Some(&env.vip_group), None).await;
    assert_eq!(
        vip.pool_code.as_deref(),
        Some(env.fast_pool.as_str()),
        "分组的 pool_code 应被鉴权解析出来"
    );
    let seen = candidate_channels(&env.pg, &env.model, vip.pool_code.as_deref()).await;
    assert!(
        seen.contains(&env.both) && !seen.contains(&env.stable_only),
        "fast 池只含 both 渠道，stable_only 不应出现：{seen:?}"
    );

    // 2) 无池用户只看"未被任何池认领"的渠道：两个渠道都已入池，故一个都看不到。
    //    这条正是池的隔离价值——入池即专属，否则免费档能打到 vip 专属渠道。
    let plain = key_in_group(&env.pg, None, None).await;
    assert!(plain.pool_code.is_none(), "无分组不应解析出池");
    let all = candidate_channels(&env.pg, &env.model, None).await;
    assert!(
        !all.contains(&env.both) && !all.contains(&env.stable_only),
        "已入池的渠道不应对无池用户可见：{all:?}"
    );

    // 2b) 未入任何池的渠道对无池用户可见（宽松默认）
    let (orphan, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("ch-orphan-{}", Uuid::new_v4().simple()),
        "openai",
        "https://api.openai.com/v1",
        "cred",
        &[env.model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    let all = candidate_channels(&env.pg, &env.model, None).await;
    assert!(all.contains(&orphan), "未入池渠道应对无池用户可见：{all:?}");

    // 3) 令牌 pool_override 优先于分组的池：把 vip 用户的 key 钉到 stable 池
    let stable_pool = sqlx::query_scalar!(
        "SELECT pool_code FROM pool_channels WHERE channel_id = $1 AND pool_code <> $2",
        env.stable_only,
        env.fast_pool
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    let pinned = key_in_group(&env.pg, Some(&env.vip_group), Some(&stable_pool)).await;
    assert_eq!(
        pinned.pool_code.as_deref(),
        Some(stable_pool.as_str()),
        "pool_override 应盖过分组的池"
    );
    let pinned_seen = candidate_channels(&env.pg, &env.model, pinned.pool_code.as_deref()).await;
    assert!(
        pinned_seen.contains(&env.stable_only),
        "钉到 stable 池后应能看到 stable_only：{pinned_seen:?}"
    );
}

/// per-key 模型子集：同一渠道下不同 key 的模型权限可以不同。
/// 现实场景是同组织的两把 key 只有一把开了某模型的访问权。
#[tokio::test]
async fn key_model_subset_narrows_candidates_within_one_channel() {
    let env = setup().await;
    // 用 stable 池视角观察（该渠道已入 stable 池）
    let stable_pool = sqlx::query_scalar!(
        "SELECT pool_code FROM pool_channels WHERE channel_id = $1",
        env.stable_only
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    let before = candidate_channels(&env.pg, &env.model, Some(&stable_pool)).await;
    assert!(before.contains(&env.stable_only));

    // 把该渠道下所有 key 限制为只服务另一个模型
    sqlx::query!(
        r#"UPDATE channel_keys SET model_subset = '["other-model"]'::jsonb WHERE channel_id = $1"#,
        env.stable_only
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let after = candidate_channels(&env.pg, &env.model, Some(&stable_pool)).await;
    assert!(
        !after.contains(&env.stable_only),
        "key 的模型子集不含该模型时应被摘出候选：{after:?}"
    );
    assert!(
        after.contains(&env.both),
        "未设子集的渠道不受影响（null = 继承渠道 models）：{after:?}"
    );
}

/// 池被分组引用时不可删：静默解绑等于悄悄放开可见性。
#[tokio::test]
async fn pool_delete_blocked_while_referenced() {
    let env = setup().await;
    let err = okapi_store::mutate::delete_channel_pool(&env.pg, &env.fast_pool).await;
    assert!(
        matches!(err, Err(okapi_store::StoreError::Conflict("pool_in_use"))),
        "被分组引用的池应拒绝删除，实际 {err:?}"
    );

    // 解绑后可删
    okapi_store::admin::upsert_price_group(&env.pg, &env.vip_group, "0.85", "vip", None)
        .await
        .unwrap();
    assert!(
        okapi_store::mutate::delete_channel_pool(&env.pg, &env.fast_pool)
            .await
            .unwrap(),
        "解绑后应可删除"
    );
}
