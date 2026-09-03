//! 渠道池语义验收（IMPLEMENTATION §11.14 / docs/database.md §3.7）。
//!
//! 只有一条可见性规则：**渠道只服务它所在的池**。这里守住：
//! 新渠道缺省进 default 池；分组的池限定候选；令牌 pool_override 优先于分组；
//! 孤儿渠道（不在任何池）对谁都不可达；池级降级（fallback_pool_code）让主池无候选时
//! 退到备池且备池整体排在主池之后；成员级 priority 覆盖让同一渠道在不同池里主备互换；
//! 以及 per-key 模型子集能在同一渠道内区分不同 key 的权限。
//!
//! 直接打 store 的候选查询而不走 HTTP：可见性是选路语义，
//! 断在候选集合上比断在响应体上更贴近被测对象。

use okapi_store::admin::{PoolMember, PriceGroupInput};
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
    stable_pool: String,
    fast_pool: String,
}

fn member(pool: &str) -> PoolMember {
    PoolMember {
        pool_code: pool.to_owned(),
        priority_override: None,
        weight_override: None,
    }
}

async fn new_channel(pg: &PgPool, name: &str, model: &str) -> i64 {
    okapi_store::provision::create_channel(
        pg,
        name,
        "openai",
        "https://api.openai.com/v1",
        "cred",
        &[model],
        false,
        None,
    )
    .await
    .unwrap()
    .0
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
        okapi_store::admin::upsert_channel_pool(&pg, code, "t", "priority_weighted", None)
            .await
            .unwrap();
    }

    let stable_only = new_channel(&pg, &format!("ch-s-{sfx}"), &model).await;
    let both = new_channel(&pg, &format!("ch-b-{sfx}"), &model).await;
    okapi_store::admin::set_channel_pools(&pg, stable_only, &[member(&stable_pool)])
        .await
        .unwrap();
    okapi_store::admin::set_channel_pools(&pg, both, &[member(&stable_pool), member(&fast_pool)])
        .await
        .unwrap();

    let vip_group = format!("g-vip-{sfx}");
    okapi_store::admin::upsert_price_group(
        &pg,
        PriceGroupInput {
            group_code: &vip_group,
            group_ratio: "0.85",
            description: "vip",
            pool_code: Some(&fast_pool),
            self_select: false,
        },
    )
    .await
    .unwrap();

    Env {
        pg,
        model,
        stable_only,
        both,
        vip_group,
        stable_pool,
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

/// 候选渠道 id（去重、按首次出现序保留——排序本身也是被测对象）。
async fn candidate_channels(pg: &PgPool, model: &str, chain: &[&str]) -> Vec<i64> {
    let mut seen = Vec::new();
    for c in okapi_store::channels::candidates_for_model(pg, model, chain, None)
        .await
        .unwrap()
    {
        if !seen.contains(&c.channel_id) {
            seen.push(c.channel_id);
        }
    }
    seen
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
    assert_eq!(
        vip.pool_chain(),
        vec![env.fast_pool.as_str()],
        "无降级配置时池链只有主池"
    );
    let seen = candidate_channels(&env.pg, &env.model, &vip.pool_chain()).await;
    assert!(
        seen.contains(&env.both) && !seen.contains(&env.stable_only),
        "fast 池只含 both 渠道，stable_only 不应出现：{seen:?}"
    );

    // 2) 无分组用户落 default 分组 → default 池：两个渠道都已被移出 default 池，故一个都看不到。
    //    这条正是池的隔离价值——渠道只服务它所在的池，免费档打不到 vip 专属渠道。
    let plain = key_in_group(&env.pg, None, None).await;
    assert_eq!(
        plain.pool_code.as_deref(),
        Some(okapi_store::channels::DEFAULT_POOL),
        "无分组 → default 分组 → default 池，池恒有值"
    );
    let all = candidate_channels(&env.pg, &env.model, &plain.pool_chain()).await;
    assert!(
        !all.contains(&env.both) && !all.contains(&env.stable_only),
        "不在 default 池的渠道不应对 default 用户可见：{all:?}"
    );

    // 2b) 新建渠道缺省进 default 池 → default 用户立刻可见（站长第一条渠道建出来就能用）
    let fresh = new_channel(
        &env.pg,
        &format!("ch-fresh-{}", Uuid::new_v4().simple()),
        &env.model,
    )
    .await;
    let all = candidate_channels(&env.pg, &env.model, &plain.pool_chain()).await;
    assert!(all.contains(&fresh), "新渠道应缺省在 default 池：{all:?}");
    assert!(
        !candidate_channels(&env.pg, &env.model, &vip.pool_chain())
            .await
            .contains(&fresh),
        "default 池的渠道对 fast 池用户不可见（vip 不自动继承公共渠道，除非配降级）"
    );

    // 2c) 孤儿：从所有池移出后对谁都不可达
    okapi_store::admin::set_channel_pools(&env.pg, fresh, &[])
        .await
        .unwrap();
    for chain in [plain.pool_chain(), vip.pool_chain()] {
        assert!(
            !candidate_channels(&env.pg, &env.model, &chain)
                .await
                .contains(&fresh),
            "孤儿渠道对 {chain:?} 不应可见"
        );
    }

    // 3) 令牌 pool_override 优先于分组的池：把 vip 用户的 key 钉到 stable 池
    let pinned = key_in_group(&env.pg, Some(&env.vip_group), Some(&env.stable_pool)).await;
    assert_eq!(
        pinned.pool_code.as_deref(),
        Some(env.stable_pool.as_str()),
        "pool_override 应盖过分组的池"
    );
    let pinned_seen = candidate_channels(&env.pg, &env.model, &pinned.pool_chain()).await;
    assert!(
        pinned_seen.contains(&env.stable_only),
        "钉到 stable 池后应能看到 stable_only：{pinned_seen:?}"
    );
}

/// 池级降级：fast 池配 fallback = stable 后，vip 用户的池链变成 [fast, stable]，
/// stable 独有的渠道可见但整体排在 fast 池候选之后；同属两池的渠道只按主池算一次。
#[tokio::test]
async fn fallback_pool_extends_chain_after_primary() {
    let env = setup().await;
    okapi_store::admin::upsert_channel_pool(
        &env.pg,
        &env.fast_pool,
        "t",
        "priority_weighted",
        Some(&env.stable_pool),
    )
    .await
    .unwrap();
    let vip = key_in_group(&env.pg, Some(&env.vip_group), None).await;
    assert_eq!(
        vip.pool_chain(),
        vec![env.fast_pool.as_str(), env.stable_pool.as_str()],
        "池链 = 主池 → 降级池"
    );

    // 把 stable_only 的渠道优先级抬得很高：若降级池不是整体靠后，它会排到最前
    sqlx::query!(
        "UPDATE channels SET priority = 100 WHERE id = $1",
        env.stable_only
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let cands =
        okapi_store::channels::candidates_for_model(&env.pg, &env.model, &vip.pool_chain(), None)
            .await
            .unwrap();
    let ids: Vec<i64> = cands.iter().map(|c| c.channel_id).collect();
    assert_eq!(
        ids,
        vec![env.both, env.stable_only],
        "主池渠道在前、降级池渠道在后，同属两池的 both 只出现一次：{ids:?}"
    );
    assert_eq!(cands[0].pool_rank, 0, "主池成员 pool_rank=0");
    assert_eq!(cands[1].pool_rank, 1, "经降级池才可见的成员 pool_rank=1");

    // 降级链单跳：stable 再配 fallback 也不会被 vip 用户沿链走到第三个池
    let third = format!("p-third-{}", &Uuid::new_v4().simple().to_string()[..8]);
    okapi_store::admin::upsert_channel_pool(&env.pg, &third, "t", "priority_weighted", None)
        .await
        .unwrap();
    okapi_store::admin::upsert_channel_pool(
        &env.pg,
        &env.stable_pool,
        "t",
        "priority_weighted",
        Some(&third),
    )
    .await
    .unwrap();
    let vip = key_in_group(&env.pg, Some(&env.vip_group), None).await;
    assert_eq!(
        vip.pool_chain().len(),
        2,
        "单跳：不递归展开降级池自己的降级"
    );
}

/// 成员级覆盖：同一渠道在不同池里的优先级可以不同——stable 池里 A 主 B 备，
/// fast 池里反过来；NULL 继承渠道自身 priority。
#[tokio::test]
async fn membership_override_reorders_per_pool() {
    let env = setup().await;
    let sfx = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let a = new_channel(&env.pg, &format!("ch-a-{sfx}"), &env.model).await;
    let b = new_channel(&env.pg, &format!("ch-b2-{sfx}"), &env.model).await;
    // 渠道自身 priority：a=0, b=10（不覆盖时 b 在前）
    sqlx::query!("UPDATE channels SET priority = 10 WHERE id = $1", b)
        .execute(&env.pg)
        .await
        .unwrap();
    let p1 = format!("p-x-{sfx}");
    let p2 = format!("p-y-{sfx}");
    for code in [&p1, &p2] {
        okapi_store::admin::upsert_channel_pool(&env.pg, code, "t", "priority_weighted", None)
            .await
            .unwrap();
    }
    // p1：a 覆盖成 50 → a 在前；p2：不覆盖 → b 在前
    okapi_store::admin::set_channel_pools(
        &env.pg,
        a,
        &[
            PoolMember {
                pool_code: p1.clone(),
                priority_override: Some(50),
                weight_override: Some(7),
            },
            member(&p2),
        ],
    )
    .await
    .unwrap();
    okapi_store::admin::set_channel_pools(&env.pg, b, &[member(&p1), member(&p2)])
        .await
        .unwrap();

    let in_p1 =
        okapi_store::channels::candidates_for_model(&env.pg, &env.model, &[p1.as_str()], None)
            .await
            .unwrap();
    assert_eq!(in_p1[0].channel_id, a, "p1 里 a 的覆盖优先级 50 > b 的 10");
    assert_eq!(in_p1[0].priority, 50, "候选带的是有效优先级（覆盖值）");
    assert_eq!(in_p1[0].weight, 7, "权重覆盖同样生效");
    let in_p2 = candidate_channels(&env.pg, &env.model, &[p2.as_str()]).await;
    assert_eq!(
        in_p2,
        vec![b, a],
        "p2 未覆盖，按渠道自身 priority：b(10) 先于 a(0)"
    );
}

/// per-key 模型子集：同一渠道下不同 key 的模型权限可以不同。
/// 现实场景是同组织的两把 key 只有一把开了某模型的访问权。
#[tokio::test]
async fn key_model_subset_narrows_candidates_within_one_channel() {
    let env = setup().await;
    let before = candidate_channels(&env.pg, &env.model, &[env.stable_pool.as_str()]).await;
    assert!(before.contains(&env.stable_only));

    // 把该渠道下所有 key 限制为只服务另一个模型
    sqlx::query!(
        r#"UPDATE channel_keys SET model_subset = '["other-model"]'::jsonb WHERE channel_id = $1"#,
        env.stable_only
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let after = candidate_channels(&env.pg, &env.model, &[env.stable_pool.as_str()]).await;
    assert!(
        !after.contains(&env.stable_only),
        "key 的模型子集不含该模型时应被摘出候选：{after:?}"
    );
    assert!(
        after.contains(&env.both),
        "未设子集的渠道不受影响（null = 继承渠道 models）：{after:?}"
    );
}

/// 池被分组 / 令牌 / 其它池的降级引用时不可删：静默解绑等于悄悄放开可见性或抽掉兜底；
/// 内置 default 池永远不可删。
#[tokio::test]
async fn pool_delete_blocked_while_referenced() {
    let env = setup().await;
    let err = okapi_store::mutate::delete_channel_pool(&env.pg, &env.fast_pool).await;
    assert!(
        matches!(err, Err(okapi_store::StoreError::Conflict("pool_in_use"))),
        "被分组引用的池应拒绝删除，实际 {err:?}"
    );

    // 分组改指 stable，但 stable 把 fast 当降级目标 → 仍不可删
    okapi_store::admin::upsert_price_group(
        &env.pg,
        PriceGroupInput {
            group_code: &env.vip_group,
            group_ratio: "0.85",
            description: "vip",
            pool_code: Some(&env.stable_pool),
            self_select: false,
        },
    )
    .await
    .unwrap();
    okapi_store::admin::upsert_channel_pool(
        &env.pg,
        &env.stable_pool,
        "t",
        "priority_weighted",
        Some(&env.fast_pool),
    )
    .await
    .unwrap();
    let err = okapi_store::mutate::delete_channel_pool(&env.pg, &env.fast_pool).await;
    assert!(
        matches!(err, Err(okapi_store::StoreError::Conflict("pool_in_use"))),
        "被当作降级目标的池应拒绝删除，实际 {err:?}"
    );

    // 解除降级引用后可删
    okapi_store::admin::upsert_channel_pool(
        &env.pg,
        &env.stable_pool,
        "t",
        "priority_weighted",
        None,
    )
    .await
    .unwrap();
    assert!(
        okapi_store::mutate::delete_channel_pool(&env.pg, &env.fast_pool)
            .await
            .unwrap(),
        "解绑后应可删除"
    );

    let err =
        okapi_store::mutate::delete_channel_pool(&env.pg, okapi_store::channels::DEFAULT_POOL)
            .await;
    assert!(
        matches!(err, Err(okapi_store::StoreError::Conflict("builtin_pool"))),
        "内置 default 池不可删，实际 {err:?}"
    );

    // 分组不能落到"无池"：缺省 pool_code 解析为 default
    okapi_store::admin::upsert_price_group(
        &env.pg,
        PriceGroupInput {
            group_code: &env.vip_group,
            group_ratio: "0.85",
            description: "vip",
            pool_code: None,
            self_select: true,
        },
    )
    .await
    .unwrap();
    let row = sqlx::query!(
        r#"SELECT pool_code, self_select FROM price_groups WHERE group_code = $1"#,
        env.vip_group
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(row.pool_code, okapi_store::channels::DEFAULT_POOL);
    assert!(row.self_select);
}
