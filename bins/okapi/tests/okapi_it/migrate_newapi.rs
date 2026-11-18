//! 迁移演练（M4 验收项）：new-api JSONL 样本库 → Okapi 全量校验。
//! 覆盖：quota→micro 换算、明文 key→哈希、渠道 type 映射、幂等二跑、
//! dry-run 零写入、迁移后 key 直接可用。依赖 .env（scripts/dev-deps.sh up）。

use okapi::migrate;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

struct Env {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    dir: std::path::PathBuf,
    suffix: String,
    /// alice 令牌的 new-api `expired_time`（Unix 秒）。
    alice_expiry: i64,
}

fn write_jsonl(dir: &std::path::Path, name: &str, rows: &[serde_json::Value]) {
    use std::fmt::Write as _;
    let body = rows.iter().fold(String::new(), |mut acc, r| {
        let _ = writeln!(acc, "{r}");
        acc
    });
    std::fs::write(dir.join(name), body).unwrap();
}

/// 2 个 token（alice 带过期时间、bob 永久 = -1）+ 1 个孤儿。
fn token_rows(suffix: &str, alice_expiry: i64) -> Vec<serde_json::Value> {
    vec![
        json!({"user_id": 1, "name": "cli", "key": format!("naKeyAlice{suffix}"), "status": 1,
               "expired_time": alice_expiry, "group": format!("vip-{suffix}")}),
        json!({"user_id": 2, "name": "app", "key": format!("sk-naKeyBob{suffix}"), "status": 1,
               "expired_time": -1, "group": "auto"}),
        json!({"user_id": 99, "name": "orphan", "key": "sk-orphan", "status": 1}),
    ]
}

/// 2 个渠道（openai + 未知 type）；`oai_key` / `oai_weight` 供「源系统轮换后重跑」改写。
fn channel_rows(suffix: &str, oai_key: &str, oai_weight: i64) -> Vec<serde_json::Value> {
    vec![
        json!({"name": format!("na-oai-{suffix}"), "type": 1, "key": oai_key,
               "base_url": "https://oai.example/v1", "models": "gpt-4o,gpt-4o-mini",
               "priority": 5, "weight": oai_weight, "status": 1, "group": format!("default,vip-{suffix}")}),
        json!({"name": format!("na-weird-{suffix}"), "type": 999, "key": "up-key-2",
               "base_url": "https://weird.example/v1", "models": "x-model", "status": 1}),
    ]
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let ledger = okapi_ledger::BalanceLedger::new(redis);

    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let dir = std::env::temp_dir().join(format!("okapi-migrate-{suffix}"));
    std::fs::create_dir_all(&dir).unwrap();

    // new-api 样本库：2 用户（一个带 quota）、2 token、2 渠道（openai + 未知 type）
    let users = [
        json!({"id": 1, "username": format!("na-alice-{suffix}"), "email": format!("alice-{suffix}@na.test"),
               "role": 100, "status": 1, "quota": 5_000_000, "group": "default"}),
        json!({"id": 2, "username": format!("na-bob-{suffix}"), "role": 1, "status": 2, "quota": 0,
               "group": format!("vip-{suffix}")}),
    ];
    let alice_expiry = chrono::Utc::now().timestamp() + 30 * 86_400;
    write_jsonl(&dir, "users.jsonl", &users);
    write_jsonl(&dir, "tokens.jsonl", &token_rows(&suffix, alice_expiry));
    write_jsonl(
        &dir,
        "channels.jsonl",
        &channel_rows(&suffix, "up-key-1", 3),
    );

    Env {
        pg,
        ledger,
        dir,
        suffix,
        alice_expiry,
    }
}

#[tokio::test]
// 迁移演练场景脚本：全量校验一体
#[allow(clippy::too_many_lines)]
async fn newapi_sample_migration_full_check() {
    let env = setup().await;

    // —— dry-run：只统计，零写入 ——
    let stats = migrate::run_newapi(&env.pg, None, &env.dir, true, None)
        .await
        .unwrap();
    assert_eq!(stats.users, 2);
    assert_eq!(stats.keys, 2, "孤儿 token 跳过");
    assert_eq!(stats.channels, 2);
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM users WHERE username LIKE $1"#,
        format!("na-%{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(count, 0, "dry-run 不得写入");

    // —— 正式迁移 ——
    let stats = migrate::run_newapi(&env.pg, Some(&env.ledger), &env.dir, false, None)
        .await
        .unwrap();
    assert_eq!(stats.users, 2);
    assert_eq!(stats.users_credited, 1);
    assert!(
        stats.skipped.iter().any(|w| w.contains("type=999")),
        "未知渠道类型必须告警：{:?}",
        stats.skipped
    );

    // 用户与角色/状态
    let alice = sqlx::query!(
        r#"SELECT id, role, status, email FROM users WHERE username = $1"#,
        format!("na-alice-{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(alice.role, 100);
    assert_eq!(alice.status, 1);
    assert!(alice.email.as_deref().unwrap_or("").contains("@na.test"));
    let bob = sqlx::query!(
        r#"SELECT status FROM users WHERE username = $1"#,
        format!("na-bob-{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(bob.status, 2, "禁用状态保留");

    // 余额：5_000_000 quota × 2 = 10_000_000 micro（$10）
    let balance = env.ledger.balance(alice.id).await.unwrap();
    assert_eq!(balance.as_micros(), 10_000_000, "quota→micro 换算");
    let event = sqlx::query!(
        r#"SELECT actor, delta_micro FROM billing_events WHERE user_id = $1"#,
        alice.id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(event.actor, "system:migrate");
    assert_eq!(event.delta_micro, 10_000_000);

    // key：无 sk- 前缀已补齐、哈希落库且立即可鉴权
    let token = format!("sk-naKeyAlice{}", env.suffix);
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    let found = okapi_store::auth::find_key_by_hash(&env.pg, &key_hash)
        .await
        .unwrap()
        .expect("迁移的 key 必须可鉴权");
    assert_eq!(found.user_id, alice.id);

    // 过期时间：new-api 的 expired_time（Unix 秒）进 api_keys.expires_at，-1 = 永久（NULL）。
    // 此前整列被忽略，带过期时间的令牌迁过来就成了永久有效
    let expires = |hash: String| {
        let pg = env.pg.clone();
        async move {
            sqlx::query_scalar::<_, Option<chrono::DateTime<chrono::Utc>>>(
                "SELECT expires_at FROM api_keys WHERE key_hash = $1",
            )
            .bind(hash)
            .fetch_one(&pg)
            .await
            .unwrap()
            .map(|t| t.timestamp())
        }
    };
    assert_eq!(expires(key_hash.clone()).await, Some(env.alice_expiry));
    let bob_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(
            format!("sk-naKeyBob{}", env.suffix).as_bytes(),
        ))
    };
    assert_eq!(expires(bob_hash).await, None, "expired_time=-1 是永久有效");

    // 渠道：type 映射 + models 拆分 + weight
    let ch = sqlx::query!(
        r#"SELECT c.provider, c.api_base, c.models, c.priority, ck.weight
           FROM channels c JOIN channel_keys ck ON ck.channel_id = c.id
           WHERE c.name = $1"#,
        format!("na-oai-{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(ch.provider, "openai");
    assert_eq!(ch.api_base.as_deref(), Some("https://oai.example/v1"));
    assert_eq!(ch.models, json!(["gpt-4o", "gpt-4o-mini"]));
    assert_eq!(ch.priority, 5);
    assert_eq!(ch.weight, 3);
    let weird = sqlx::query!(
        r#"SELECT provider FROM channels WHERE name = $1"#,
        format!("na-weird-{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(weird.provider, "openai_compat", "未知 type 兜底");

    // 分组映射：渠道 group CSV → 同名池成员；用户 group → user_groups；令牌 group → group_override；
    // auto 令牌 → 跟随用户分组（无覆盖）；无 group 的渠道 → 只在 default 池
    let vip = format!("vip-{}", env.suffix);
    let oai_pools = sqlx::query_scalar!(
        r#"SELECT pc.pool_code FROM pool_channels pc JOIN channels c ON c.id = pc.channel_id
           WHERE c.name = $1 ORDER BY pc.pool_code"#,
        format!("na-oai-{}", env.suffix)
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        oai_pools,
        vec!["default".to_owned(), vip.clone()],
        "渠道分组 CSV → 两个池"
    );
    let weird_pools = sqlx::query_scalar!(
        r#"SELECT pc.pool_code FROM pool_channels pc JOIN channels c ON c.id = pc.channel_id
           WHERE c.name = $1"#,
        format!("na-weird-{}", env.suffix)
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        weird_pools,
        vec!["default".to_owned()],
        "无分组渠道只进 default 池"
    );
    let vip_group = sqlx::query!(
        r#"SELECT pool_code, group_ratio::text AS "ratio!" FROM price_groups WHERE group_code = $1"#,
        vip
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(vip_group.pool_code, vip, "同名价格分组指向同名池");
    assert_eq!(
        vip_group.ratio, "1.000000",
        "倍率占位 1，待站长按 GroupRatio 调"
    );
    assert_eq!(
        found.group_code, vip,
        "alice 的令牌 group=vip → group_override，生效分组即 vip"
    );
    assert_eq!(found.pool_code.as_deref(), Some(vip.as_str()));
    let bob_groups = sqlx::query_scalar!(
        r#"SELECT ug.group_code FROM user_groups ug JOIN users u ON u.id = ug.user_id
           WHERE u.username = $1"#,
        format!("na-bob-{}", env.suffix)
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(bob_groups, vec![vip.clone()], "用户 group → user_groups");
    let bob_key_override = sqlx::query_scalar!(
        r#"SELECT k.group_override FROM api_keys k JOIN users u ON u.id = k.user_id
           WHERE u.username = $1"#,
        format!("na-bob-{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(
        bob_key_override.is_none(),
        "auto 令牌不写覆盖，跟随用户分组"
    );

    // —— 幂等二跑：余额不翻倍、行数不增 ——
    let stats2 = migrate::run_newapi(&env.pg, Some(&env.ledger), &env.dir, false, None)
        .await
        .unwrap();
    assert_eq!(stats2.users, 2);
    let balance = env.ledger.balance(alice.id).await.unwrap();
    assert_eq!(balance.as_micros(), 10_000_000, "二跑余额不得翻倍");
    let events = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events
           WHERE user_id = $1 AND actor = 'system:migrate'"#,
        alice.id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(events, 1);
    let channels = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM channels WHERE name LIKE $1 AND deleted_at IS NULL"#,
        format!("na-%{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(channels, 2, "渠道 upsert 不重复");

    // —— 源系统轮换 key / 调权重 / 改过期时间后重跑：同步过来，而不是静默保留旧值 ——
    let key_of = |channel: String| {
        let pg = env.pg.clone();
        async move {
            let (id, weight): (i64, i32) = sqlx::query_as(
                "SELECT k.id, k.weight FROM channel_keys k JOIN channels c ON c.id = k.channel_id
                 WHERE c.name = $1",
            )
            .bind(channel)
            .fetch_one(&pg)
            .await
            .unwrap();
            let credential = okapi_store::admin::read_key_credential(&pg, id, None)
                .await
                .unwrap();
            (credential, weight)
        }
    };
    let oai_name = format!("na-oai-{}", env.suffix);
    let weird_name = format!("na-weird-{}", env.suffix);
    assert_eq!(
        key_of(oai_name.clone()).await,
        (Some("up-key-1".to_owned()), 3)
    );
    // na-weird 渠道运维后来又加了第二把 key：重跑对不上是哪一把，只记注记、不动它
    let weird_id: i64 = sqlx::query_scalar("SELECT id FROM channels WHERE name = $1")
        .bind(&weird_name)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    sqlx::query("INSERT INTO channel_keys (channel_id, credential_ciphertext) VALUES ($1, $2)")
        .bind(weird_id)
        .bind(b"ops-added".to_vec())
        .execute(&env.pg)
        .await
        .unwrap();
    let renewed = env.alice_expiry + 86_400;
    write_jsonl(&env.dir, "tokens.jsonl", &token_rows(&env.suffix, renewed));
    let mut rotated = channel_rows(&env.suffix, "up-key-1-rotated", 9);
    rotated[1]["key"] = json!("up-key-2-rotated");
    write_jsonl(&env.dir, "channels.jsonl", &rotated);
    let stats3 = migrate::run_newapi(&env.pg, Some(&env.ledger), &env.dir, false, None)
        .await
        .unwrap();
    assert_eq!(
        key_of(oai_name).await,
        (Some("up-key-1-rotated".to_owned()), 9),
        "单把 key 的渠道：凭证与权重跟着源系统走"
    );
    let ops_key: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT credential_ciphertext FROM channel_keys WHERE channel_id = $1 ORDER BY id LIMIT 1",
    )
    .bind(weird_id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        ops_key.as_deref(),
        Some(&b"up-key-2"[..]),
        "多把 key 的渠道不猜是哪一把，原凭证原样"
    );
    assert!(
        stats3
            .skipped
            .iter()
            .any(|w| w.contains(&weird_name) && w.contains("多把 key")),
        "对不上的渠道必须留注记：{:?}",
        stats3.skipped
    );
    assert_eq!(
        expires(key_hash).await,
        Some(renewed),
        "重跑同步新的过期时间"
    );

    std::fs::remove_dir_all(&env.dir).ok();
}
