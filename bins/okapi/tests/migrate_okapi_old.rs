//! 迁移演练（M4 验收项）：老 ok-api（Go/UUID schema）JSONL 样本库 → Okapi 全量校验。
//! 覆盖：DECIMAL USD→micro 定点换算、bcrypt 密码免重置登录、AES-GCM 密文解出 key 重哈希、
//! providers×keys→channels 展开、token/request 计价→倍率与 per_call、幂等二跑不覆盖已改密码、
//! dry-run 零写入。依赖 .env（scripts/dev-deps.sh up）。

use okapi::migrate;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const ENC_PASS: &str = "old-okapi-enc-passphrase";

struct Env {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    dir: std::path::PathBuf,
    suffix: String,
    alice_email: String,
    alice_key: String,
}

// 五表样本构造脚本，拆分会割裂样本语义
#[allow(clippy::too_many_lines)]
async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let ledger = okapi_ledger::BalanceLedger::new(redis);

    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let dir = std::env::temp_dir().join(format!("okapi-old-migrate-{suffix}"));
    std::fs::create_dir_all(&dir).unwrap();

    let key = migrate::derive_old_key(ENC_PASS);
    let other_key = migrate::derive_old_key("wrong-passphrase");
    let alice_id = Uuid::new_v4().to_string();
    let bob_id = Uuid::new_v4().to_string();
    let carol_id = Uuid::new_v4().to_string();
    let alice_email = format!("old-alice-{suffix}@okapi.test");
    let alice_key = format!("sk-oldAlice{suffix}");

    // ---- users：超管（带余额，bcrypt 密码）/ 禁用用户 / 负余额用户 ----
    // cost=4 仅为测试提速；老库为 bcrypt.DefaultCost，前缀同为 $2
    let alice_pw = bcrypt::hash("legacy-pass", 4).unwrap();
    let users = [
        json!({"id": alice_id, "email": alice_email, "username": format!("old-alice-{suffix}"),
               "name": "Alice", "password_hash": alice_pw, "role": "super_admin",
               "status": "active", "balance": "12.34567891"}),
        json!({"id": bob_id, "email": format!("old-bob-{suffix}@okapi.test"),
               "username": format!("old-bob-{suffix}"), "role": "user",
               "status": "suspended", "balance": "0"}),
        // 无 username：按 email 本地部分兜底；负余额不入账
        json!({"id": carol_id, "email": format!("old-carol-{suffix}@okapi.test"),
               "role": "admin", "status": "active", "balance": "-5.5"}),
    ];

    // ---- api_keys：可解密 / 无密文 / 错口令密文 / 非 sk 明文 / 孤儿 ----
    let api_keys = [
        json!({"user_id": alice_id, "name": "cli", "key_prefix": "sk-oldAlice...",
               "key_hash": "$2a$10$bcryptHashIrreversible", "status": "active",
               "key_encrypted": migrate::encrypt_old(&key, &alice_key).unwrap(),
               "allowed_models": ["gpt-4o", "claude-3-5-sonnet"], "rate_limit_rpm": 120}),
        json!({"user_id": bob_id, "name": "no-cipher", "key_prefix": "sk-bob...",
               "key_hash": "$2a$10$another", "status": "active"}),
        json!({"user_id": alice_id, "name": "wrong-pass", "key_prefix": "sk-wrong...",
               "key_encrypted": migrate::encrypt_old(&other_key, "sk-unreadable").unwrap(),
               "status": "active"}),
        json!({"user_id": alice_id, "name": "not-a-key", "key_prefix": "junk...",
               "key_encrypted": migrate::encrypt_old(&key, "plain-not-sk").unwrap(),
               "status": "active"}),
        json!({"user_id": Uuid::new_v4().to_string(), "name": "orphan",
               "key_encrypted": migrate::encrypt_old(&key, "sk-orphan").unwrap(),
               "status": "active"}),
    ];

    // ---- providers × provider_api_keys → channels ----
    let providers = [
        json!({"id": 1, "provider_code": "anthropic", "provider_name": "Anthropic",
               "api_endpoint": "https://api.anthropic.com", "status": "active"}),
        json!({"id": 2, "provider_code": "someweird", "provider_name": "Weird",
               "api_endpoint": "https://weird.example/v1", "status": "active"}),
    ];
    let provider_api_keys = [
        json!({"provider_id": 1, "key_name": format!("k1-{suffix}"), "api_key": "up-secret-1",
               "base_url": "https://anthropic-proxy.example", "adapter_type": "claude",
               "supported_models": ["claude-3-5-sonnet", "claude-3-opus"],
               "weight": 4, "priority": 7, "status": "active"}),
        // base_url 空 → 回落 providers.api_endpoint；models 逗号分隔形态
        json!({"provider_id": 2, "key_name": format!("k2-{suffix}"), "api_key": "up-secret-2",
               "base_url": "", "supported_models": "w-model-a, w-model-b", "status": "inactive"}),
        json!({"provider_id": 99, "key_name": "orphan", "api_key": "x", "status": "active"}),
    ];

    // ---- models：token / request / hourly（不迁）/ 非 active（不迁）----
    let models = [
        // input $0.03、output $0.06、cached $0.015 每 1K → 倍率 15 / 2 / 0.5
        json!({"model_code": format!("old-token-{suffix}"), "pricing_type": "token",
               "input_price": "0.03", "output_price": "0.06",
               "cached_input_price": "0.015", "status": "active"}),
        // 数字字面量导出形态（非 ::text）也须接受
        json!({"model_code": format!("old-call-{suffix}"), "pricing_type": "request",
               "request_price": 0.02, "status": "active"}),
        json!({"model_code": format!("old-hourly-{suffix}"), "pricing_type": "hourly",
               "hourly_price": "1.5", "status": "active"}),
        json!({"model_code": format!("old-dead-{suffix}"), "pricing_type": "token",
               "input_price": "0.01", "status": "deprecated"}),
    ];

    let write = |name: &str, rows: &[serde_json::Value]| {
        use std::fmt::Write as _;
        let body = rows.iter().fold(String::new(), |mut acc, r| {
            let _ = writeln!(acc, "{r}");
            acc
        });
        std::fs::write(dir.join(name), body).unwrap();
    };
    write("users.jsonl", &users);
    write("api_keys.jsonl", &api_keys);
    write("providers.jsonl", &providers);
    write("provider_api_keys.jsonl", &provider_api_keys);
    write("models.jsonl", &models);

    Env {
        pg,
        ledger,
        dir,
        suffix,
        alice_email,
        alice_key,
    }
}

#[tokio::test]
// 迁移演练场景脚本：全量校验一体
#[allow(clippy::too_many_lines)]
async fn okapi_old_sample_migration_full_check() {
    let env = setup().await;
    let sfx = &env.suffix;

    // —— dry-run：只统计，零写入 ——
    let stats = migrate::run_okapi_old(&env.pg, None, &env.dir, Some(ENC_PASS), true, None)
        .await
        .unwrap();
    assert_eq!(stats.users, 3);
    assert_eq!(stats.keys, 1, "仅可解密且 sk- 前缀的 key 计入");
    assert_eq!(stats.keys_undecryptable, 3, "无密文/错口令/非 sk 三种");
    assert_eq!(stats.channels, 2, "孤儿 provider_id 跳过");
    assert_eq!(stats.models, 2, "hourly 与非 active 不迁");
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM users WHERE username LIKE $1"#,
        format!("old-%{sfx}")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(count, 0, "dry-run 不得写入");

    // —— 正式迁移 ——
    let stats = migrate::run_okapi_old(
        &env.pg,
        Some(&env.ledger),
        &env.dir,
        Some(ENC_PASS),
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(stats.users, 3);
    assert_eq!(stats.users_credited, 1);
    assert!(
        stats.skipped.iter().any(|w| w.contains("负余额")),
        "负余额必须告警：{:?}",
        stats.skipped
    );
    assert!(
        stats
            .skipped
            .iter()
            .any(|w| w.contains("pricing_type=hourly")),
        "无对应语义的计价类型必须告警：{:?}",
        stats.skipped
    );

    // 用户：角色/状态映射 + 无 username 时 email 本地部分兜底
    let alice = sqlx::query!(
        r#"SELECT id, role, status FROM users WHERE email = $1"#,
        env.alice_email
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(alice.role, 100, "super_admin → 100");
    assert_eq!(alice.status, 1);
    let bob = sqlx::query!(
        r#"SELECT status FROM users WHERE email = $1"#,
        format!("old-bob-{sfx}@okapi.test")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(bob.status, 2, "suspended → 禁用");
    let carol = sqlx::query!(
        r#"SELECT username, role FROM users WHERE email = $1"#,
        format!("old-carol-{sfx}@okapi.test")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(carol.username, format!("old-carol-{sfx}"));
    assert_eq!(carol.role, 10, "admin → 10");

    // 余额：$12.34567891 → 12_345_678 micro（第 7 位起截断，不四舍五入）
    let balance = env.ledger.balance(alice.id).await.unwrap();
    assert_eq!(balance.as_micros(), 12_345_678, "DECIMAL→micro 定点截断");
    let event = sqlx::query!(
        r#"SELECT actor, delta_micro, event_type FROM billing_events WHERE user_id = $1"#,
        alice.id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(event.actor, "system:migrate:okapi_old");
    assert_eq!(event.event_type, "adjust");
    assert_eq!(event.delta_micro, 12_345_678);
    let carol_balance = env
        .ledger
        .balance(
            sqlx::query_scalar!(
                r#"SELECT id FROM users WHERE email = $1"#,
                format!("old-carol-{sfx}@okapi.test")
            )
            .fetch_one(&env.pg)
            .await
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(carol_balance.as_micros(), 0, "负余额不入账");

    // bcrypt 密码免重置登录（老库哈希原样迁移）
    let login = okapi_store::identity::find_login_user(&env.pg, &env.alice_email, "legacy-pass")
        .await
        .unwrap();
    assert!(login.is_some(), "老 bcrypt 密码必须可直接登录");
    assert_eq!(login.unwrap().user_id, alice.id);
    assert!(
        okapi_store::identity::find_login_user(&env.pg, &env.alice_email, "wrong")
            .await
            .unwrap()
            .is_none()
    );

    // key：密文解出明文重新 SHA-256，落库即可鉴权 + 限额属性
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(env.alice_key.as_bytes()))
    };
    let found = okapi_store::auth::find_key_by_hash(&env.pg, &key_hash)
        .await
        .unwrap()
        .expect("迁移的 key 必须可鉴权");
    assert_eq!(found.user_id, alice.id);
    let key_row = sqlx::query!(
        r#"SELECT model_allowlist, rpm_limit, key_prefix FROM api_keys WHERE key_hash = $1"#,
        key_hash
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        key_row.model_allowlist,
        Some(json!(["gpt-4o", "claude-3-5-sonnet"]))
    );
    assert_eq!(key_row.rpm_limit, Some(120));
    assert_eq!(key_row.key_prefix, &env.alice_key[..16]);
    // 不可解密的 key 一律不落库（宁缺毋滥：错哈希会导致永久鉴权失败）
    let unreadable = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM api_keys
           WHERE user_id = $1 AND name IN ('wrong-pass', 'not-a-key')"#,
        alice.id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(unreadable, 0);

    // 渠道：provider_code+adapter_type 映射、base_url 覆盖/回落、权重与优先级
    let ch = sqlx::query!(
        r#"SELECT c.provider, c.api_base, c.models, c.priority, c.status, ck.weight
           FROM channels c JOIN channel_keys ck ON ck.channel_id = c.id
           WHERE c.name = $1"#,
        format!("old/anthropic/k1-{sfx}")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(ch.provider, "anthropic", "claude adapter → anthropic");
    assert_eq!(
        ch.api_base.as_deref(),
        Some("https://anthropic-proxy.example"),
        "key 级 base_url 优先于 provider 级"
    );
    assert_eq!(ch.models, json!(["claude-3-5-sonnet", "claude-3-opus"]));
    assert_eq!(ch.priority, 7);
    assert_eq!(ch.status, 1);
    assert_eq!(ch.weight, 4);
    let weird = sqlx::query!(
        r#"SELECT provider, api_base, models, status FROM channels WHERE name = $1"#,
        format!("old/someweird/k2-{sfx}")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(weird.provider, "openai_compat", "未知 provider 兜底");
    assert_eq!(
        weird.api_base.as_deref(),
        Some("https://weird.example/v1"),
        "base_url 空 → 回落 provider api_endpoint"
    );
    assert_eq!(weird.models, json!(["w-model-a", "w-model-b"]));
    assert_eq!(weird.status, 2, "inactive → 禁用");

    // 定价：token 单价 → 倍率（基准 $0.002/1K）；request → per_call micro
    let ratios = sqlx::query!(
        r#"SELECT (p.model_ratio = 15) AS "ratio_ok!",
                  (p.completion_ratio = 2) AS "completion_ok!",
                  (p.cache_ratio = 0.5) AS "cache_ok!",
                  p.pricing_mode
           FROM model_pricing p JOIN models m ON m.id = p.model_id
           WHERE m.model_name = $1"#,
        format!("old-token-{sfx}")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(ratios.ratio_ok, "$0.03/1K ÷ $0.002 = 15");
    assert!(ratios.completion_ok, "0.06/0.03 = 2");
    assert!(ratios.cache_ok, "0.015/0.03 = 0.5");
    assert_eq!(ratios.pricing_mode, "ratio");
    let per_call = sqlx::query!(
        r#"SELECT p.pricing_mode, p.per_call_price_micro
           FROM model_pricing p JOIN models m ON m.id = p.model_id
           WHERE m.model_name = $1"#,
        format!("old-call-{sfx}")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(per_call.pricing_mode, "per_call");
    assert_eq!(
        per_call.per_call_price_micro,
        Some(20_000),
        "$0.02 → 20000µ"
    );
    let unpriced = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM models WHERE model_name = ANY($1)"#,
        &[format!("old-hourly-{sfx}"), format!("old-dead-{sfx}")][..]
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(unpriced, 0, "不迁的计价类型不得建 models 行");

    // —— 幂等二跑：余额不翻倍、行数不增、已改密码不被回退 ——
    let new_hash = okapi_store::identity::hash_password("rotated-pass").unwrap();
    sqlx::query!(
        r#"UPDATE users SET password_hash = $2 WHERE id = $1"#,
        alice.id,
        new_hash
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let stats2 = migrate::run_okapi_old(
        &env.pg,
        Some(&env.ledger),
        &env.dir,
        Some(ENC_PASS),
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(stats2.users, 3);
    assert_eq!(
        env.ledger.balance(alice.id).await.unwrap().as_micros(),
        12_345_678,
        "二跑余额不得翻倍"
    );
    let events = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events
           WHERE user_id = $1 AND actor = 'system:migrate:okapi_old'"#,
        alice.id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(events, 1);
    assert!(
        okapi_store::identity::find_login_user(&env.pg, &env.alice_email, "rotated-pass")
            .await
            .unwrap()
            .is_some(),
        "二跑不得把已改密码回退为老 bcrypt 哈希"
    );
    let channels = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM channels
           WHERE name LIKE $1 AND deleted_at IS NULL"#,
        format!("old/%{sfx}")
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(channels, 2, "渠道 upsert 不重复");
    let keys = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM api_keys WHERE user_id = $1"#,
        alice.id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(keys, 1, "key upsert 不重复");

    // —— 无口令：key 全部跳过但用户/渠道/定价照迁 ——
    let stats3 = migrate::run_okapi_old(&env.pg, Some(&env.ledger), &env.dir, None, true, None)
        .await
        .unwrap();
    assert_eq!(stats3.keys, 0);
    assert_eq!(stats3.keys_undecryptable, 4, "含孤儿外的四把全部不可解密");
    assert_eq!(stats3.users, 3);
    assert_eq!(stats3.channels, 2);

    std::fs::remove_dir_all(&env.dir).ok();
}
