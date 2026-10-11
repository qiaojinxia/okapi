//! `find_key_by_hash` 的身份快照逐字段核对。网关层的用例只看 HTTP 状态（401/429），
//! CTE 里分组优先级、钉池、团队成员覆盖这些分支改坏了不一定会让它们失败；这里直接断言
//! 返回的 `AuthedKey`。sqlx 为每个用例建一个一次性库。
use okapi_store::auth::find_key_by_hash;
use okapi_store::provision;
use sqlx::PgPool;

async fn exec(pool: &PgPool, sql: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .unwrap();
}

async fn user(pool: &PgPool, name: &str) -> i64 {
    provision::create_user(pool, name).await.unwrap()
}

async fn key(pool: &PgPool, user_id: i64, hash: &str) -> i64 {
    provision::create_api_key(pool, user_id, hash, "sk-okapi-t")
        .await
        .unwrap()
}

fn hash(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

/// 生效分组：key 覆盖 > 用户所属分组里优先级最高的 > 默认分组；池跟随生效分组，key 可单独钉池。
#[sqlx::test(migrations = "./migrations")]
async fn group_and_pool_follow_documented_precedence(pool: PgPool) {
    exec(&pool, "INSERT INTO channel_pools (pool_code, routing_strategy, fallback_pool_code) VALUES ('pool-a', 'least_latency', 'default'), ('pool-b', 'priority_weighted', NULL)").await;
    exec(&pool, "INSERT INTO price_groups (group_code, pool_code, rpm_limit, rph_limit) VALUES ('g-low', 'default', 10, NULL), ('g-high', 'pool-a', 20, 100), ('g-ovr', 'pool-b', NULL, NULL)").await;
    let uid = user(&pool, "precedence").await;
    let kid = key(&pool, uid, &hash(1)).await;

    let k = find_key_by_hash(&pool, &hash(1)).await.unwrap().unwrap();
    assert_eq!((k.key_id, k.user_id), (kid, uid));
    assert_eq!(k.group_code, "default", "没有任何分组时落到默认分组");
    assert_eq!(k.pool_code.as_deref(), Some("default"));
    assert_eq!(k.pool_strategy.as_deref(), Some("priority_weighted"));
    assert_eq!(k.pool_fallback, None);

    exec(&pool, &format!("INSERT INTO user_groups (user_id, group_code, priority) VALUES ({uid}, 'g-low', 1), ({uid}, 'g-high', 5)")).await;
    let k = find_key_by_hash(&pool, &hash(1)).await.unwrap().unwrap();
    assert_eq!(k.group_code, "g-high", "取优先级最高的用户分组");
    assert_eq!(k.pool_code.as_deref(), Some("pool-a"), "池跟随生效分组");
    assert_eq!(k.pool_strategy.as_deref(), Some("least_latency"));
    assert_eq!(k.pool_fallback.as_deref(), Some("default"));
    assert_eq!(
        (k.group_rpm_limit, k.group_rph_limit),
        (Some(20), Some(100))
    );

    exec(
        &pool,
        &format!("UPDATE api_keys SET group_override = 'g-ovr' WHERE id = {kid}"),
    )
    .await;
    let k = find_key_by_hash(&pool, &hash(1)).await.unwrap().unwrap();
    assert_eq!(k.group_code, "g-ovr", "key 覆盖压过用户分组");
    assert_eq!(k.pool_code.as_deref(), Some("pool-b"));
    assert_eq!((k.group_rpm_limit, k.group_rph_limit), (None, None));

    exec(
        &pool,
        &format!("UPDATE api_keys SET pool_override = 'pool-a' WHERE id = {kid}"),
    )
    .await;
    let k = find_key_by_hash(&pool, &hash(1)).await.unwrap().unwrap();
    assert_eq!(k.group_code, "g-ovr", "钉池不改定价分组");
    assert_eq!(
        k.pool_code.as_deref(),
        Some("pool-a"),
        "pool_override 压过分组的池"
    );
    assert_eq!(k.pool_strategy.as_deref(), Some("least_latency"));
}

/// 团队 key 同时认证钱包（团队）与成员：成员被封、角色权限、月限额都按成员算；移出团队即失效。
#[sqlx::test(migrations = "./migrations")]
async fn team_keys_authenticate_the_member_as_well_as_the_wallet(pool: PgPool) {
    let team = user(&pool, "team-wallet").await;
    let member = user(&pool, "team-member").await;
    exec(&pool, "INSERT INTO admin_roles (role_code, display_name, permissions) VALUES ('owner-role', 'Owner', '[\"users.write\"]'), ('member-role', 'Member', '[\"logs.read\"]')").await;
    exec(&pool, &format!("UPDATE users SET admin_role_id = (SELECT id FROM admin_roles WHERE role_code = 'owner-role'), role = 10 WHERE id = {team}")).await;
    exec(&pool, &format!("UPDATE users SET admin_role_id = (SELECT id FROM admin_roles WHERE role_code = 'member-role'), role = 5 WHERE id = {member}")).await;
    exec(&pool, &format!("INSERT INTO team_members (team_user_id, member_user_id, monthly_spend_limit_micro) VALUES ({team}, {member}, 5000)")).await;
    let kid = key(&pool, team, &hash(2)).await;
    exec(
        &pool,
        &format!("UPDATE api_keys SET member_user_id = {member} WHERE id = {kid}"),
    )
    .await;
    let owner_kid = key(&pool, team, &hash(3)).await;

    let k = find_key_by_hash(&pool, &hash(2)).await.unwrap().unwrap();
    assert_eq!(k.user_id, team, "扣的是团队钱包");
    assert_eq!(k.member_user_id, Some(member));
    assert_eq!(k.member_monthly_limit_micro, Some(5000));
    assert_eq!(k.role, 5, "角色按成员");
    assert_eq!(
        k.permissions,
        Some(vec!["logs.read".to_owned()]),
        "权限按成员，不继承团队所有者"
    );
    assert_eq!(k.user_status, 1);

    // 团队所有者自己的 key 照旧用所有者的角色权限
    let owner = find_key_by_hash(&pool, &hash(3)).await.unwrap().unwrap();
    assert_eq!(owner.key_id, owner_kid);
    assert_eq!((owner.role, owner.member_user_id), (10, None));
    assert_eq!(owner.permissions, Some(vec!["users.write".to_owned()]));

    exec(
        &pool,
        &format!("UPDATE users SET status = 2 WHERE id = {member}"),
    )
    .await;
    let k = find_key_by_hash(&pool, &hash(2)).await.unwrap().unwrap();
    assert_eq!(k.user_status, 2, "成员被封：团队账号正常也不能用这把 key");

    exec(
        &pool,
        &format!("DELETE FROM team_members WHERE member_user_id = {member}"),
    )
    .await;
    assert!(
        find_key_by_hash(&pool, &hash(2)).await.unwrap().is_none(),
        "移出团队即失效"
    );
}

/// 第二次查询带出的额度模式与登录会话，以及删除 / 未知 key 的处理。
#[sqlx::test(migrations = "./migrations")]
async fn quota_mode_session_binding_and_deleted_keys(pool: PgPool) {
    let uid = user(&pool, "session").await;
    let kid = key(&pool, uid, &hash(4)).await;
    let k = find_key_by_hash(&pool, &hash(4)).await.unwrap().unwrap();
    assert!(!k.quota_limited);
    assert_eq!(k.session_hash, None);

    let session = "ab".repeat(32);
    exec(
        &pool,
        &format!("UPDATE api_keys SET quota_mode = 1, session_hash = '{session}' WHERE id = {kid}"),
    )
    .await;
    let k = find_key_by_hash(&pool, &hash(4)).await.unwrap().unwrap();
    assert!(k.quota_limited);
    assert_eq!(k.session_hash.as_deref(), Some(session.as_str()));

    assert!(
        find_key_by_hash(&pool, &hash(5)).await.unwrap().is_none(),
        "未知哈希"
    );
    exec(
        &pool,
        &format!("UPDATE api_keys SET deleted_at = now() WHERE id = {kid}"),
    )
    .await;
    assert!(
        find_key_by_hash(&pool, &hash(4)).await.unwrap().is_none(),
        "已删除的 key"
    );
    let other = key(&pool, uid, &hash(6)).await;
    exec(
        &pool,
        &format!("UPDATE users SET deleted_at = now() WHERE id = {uid}"),
    )
    .await;
    assert!(
        find_key_by_hash(&pool, &hash(6)).await.unwrap().is_none(),
        "所属用户已删除（key {other}）"
    );
}
