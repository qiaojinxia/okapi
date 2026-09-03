//! 路由诊断端点验收（GET /admin/diagnose/route）：
//! 逐环淘汰原因（池认领/严格隔离/key 冷却/模型子集）、五种非 ok 结论、
//! 降级链可投性预览、幸存者口径与生产查询一致、权限点 channel.read。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Env {
    pg: PgPool,
    console: SocketAddr,
    suffix: String,
    admin_token: String,
}

impl Env {
    fn name(&self, tag: &str) -> String {
        format!("{tag}-{}", self.suffix)
    }
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..12].to_owned();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("diag-a-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-diag-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-diag")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = console::router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    Env {
        pg,
        console: addr,
        suffix,
        admin_token,
    }
}

async fn diagnose(env: &Env, token: &str, query: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "http://{}/admin/diagnose/route?{query}",
            env.console
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
}

async fn diagnose_json(env: &Env, query: &str) -> Value {
    let resp = diagnose(env, &env.admin_token, query).await;
    assert_eq!(resp.status(), 200, "诊断请求应成功: {query}");
    resp.json().await.unwrap()
}

/// 池认领语义 + 幸存者口径：入池渠道对无池请求给出 pool_claimed，
/// 钉住该池后同一渠道变为可用候选。
#[tokio::test]
async fn pool_claimed_vs_pinned_pool() {
    let env = setup().await;
    let model = env.name("dg-pool-m");
    let pool = env.name("dg-pool");
    okapi_store::provision::create_model_ratio(&env.pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    let (channel_id, key_id) = okapi_store::provision::create_channel(
        &env.pg,
        &env.name("dg-pool-ch"),
        "openai",
        "http://127.0.0.1:9/v1",
        "cred",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query!(r#"INSERT INTO channel_pools (pool_code) VALUES ($1)"#, pool)
        .execute(&env.pg)
        .await
        .unwrap();
    // 只进专属池（provision 缺省放进 default 池，这里覆盖掉）
    okapi_store::admin::set_channel_pool_codes(&env.pg, channel_id, std::slice::from_ref(&pool))
        .await
        .unwrap();

    // 未给分组 = default 池视角：渠道只在专属池里 → 不在池内 → 零候选
    let report = diagnose_json(&env, &format!("model={model}")).await;
    assert_eq!(report["verdict"], "no_available_channel");
    assert_eq!(report["candidates"], 0);
    assert_eq!(report["scope"]["pool_source"], "default");
    assert_eq!(report["scope"]["pool_chain"], json!(["default"]));
    assert_eq!(report["channels"][0]["excluded"], "not_in_pool");

    // 钉住池：同一渠道成为候选，key 标记 ok
    let report = diagnose_json(&env, &format!("model={model}&pool={pool}")).await;
    assert_eq!(report["verdict"], "ok");
    assert_eq!(report["candidates"], 1);
    assert_eq!(report["channels"][0]["excluded"], Value::Null);
    assert_eq!(report["channels"][0]["via_fallback"], false);
    let key = &report["channels"][0]["keys"][0];
    assert_eq!(key["key_id"].as_i64().unwrap(), key_id);
    assert_eq!(key["ok"], true);
    assert_eq!(report["scope"]["pool_source"], "param");

    // default 池配降级到专属池：default 视角经降级可见，且标注 via_fallback
    okapi_store::admin::upsert_channel_pool(
        &env.pg,
        "default",
        "",
        "priority_weighted",
        Some(&pool),
    )
    .await
    .unwrap();
    let report = diagnose_json(&env, &format!("model={model}")).await;
    assert_eq!(report["verdict"], "ok");
    assert_eq!(report["scope"]["pool_chain"], json!(["default", pool]));
    assert_eq!(report["channels"][0]["via_fallback"], true);
    okapi_store::admin::upsert_channel_pool(&env.pg, "default", "", "priority_weighted", None)
        .await
        .unwrap();

    // 孤儿：不在任何池 → 对谁都不可达，原因单独命名
    okapi_store::admin::set_channel_pool_codes(&env.pg, channel_id, &[])
        .await
        .unwrap();
    let report = diagnose_json(&env, &format!("model={model}&pool={pool}")).await;
    assert_eq!(report["verdict"], "no_available_channel");
    assert_eq!(report["channels"][0]["excluded"], "orphan_channel");
}

/// 模型侧三种结论：不存在 / 未定价 / 无渠道服务。
#[tokio::test]
async fn model_side_verdicts() {
    let env = setup().await;

    let report = diagnose_json(&env, &format!("model=ghost-{}", env.suffix)).await;
    assert_eq!(report["verdict"], "model_not_found");

    // 未定价（有渠道也没用——请求在估价就被拒，结论要指向真正的病根）
    let unpriced = env.name("dg-unpriced");
    sqlx::query!(r#"INSERT INTO models (model_name) VALUES ($1)"#, &unpriced)
        .execute(&env.pg)
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &env.pg,
        &env.name("dg-up-ch"),
        "openai",
        "http://127.0.0.1:9/v1",
        "cred",
        &[unpriced.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    let report = diagnose_json(&env, &format!("model={unpriced}")).await;
    assert_eq!(report["verdict"], "model_unpriced");

    // 已定价但没有任何渠道声称服务
    let orphan = env.name("dg-orphan");
    okapi_store::provision::create_model_ratio(&env.pg, &orphan, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    let report = diagnose_json(&env, &format!("model={orphan}")).await;
    assert_eq!(report["verdict"], "no_channel_serves_model");
    assert_eq!(report["channels"].as_array().unwrap().len(), 0);
}

/// key 级淘汰原因：冷却中与模型子集不含。
#[tokio::test]
async fn key_level_reasons() {
    let env = setup().await;
    let model = env.name("dg-key-m");
    okapi_store::provision::create_model_ratio(&env.pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    let (channel_id, cooling_key) = okapi_store::provision::create_channel(
        &env.pg,
        &env.name("dg-key-ch"),
        "openai",
        "http://127.0.0.1:9/v1",
        "cred",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    // key 1 冷却中；key 2 子集不含该模型
    sqlx::query!(
        r#"UPDATE channel_keys SET status = 2, cooldown_until = now() + interval '1 hour'
           WHERE id = $1"#,
        cooling_key
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let subset_key = sqlx::query_scalar!(
        r#"INSERT INTO channel_keys (channel_id, credential_ciphertext, model_subset)
           VALUES ($1, $2, '["other-model"]'::jsonb) RETURNING id"#,
        channel_id,
        "cred".as_bytes()
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();

    let report = diagnose_json(&env, &format!("model={model}")).await;
    assert_eq!(report["verdict"], "no_available_channel");
    let keys = report["channels"][0]["keys"].as_array().unwrap();
    let find = |id: i64| {
        keys.iter()
            .find(|k| k["key_id"].as_i64() == Some(id))
            .unwrap()
    };
    assert_eq!(find(cooling_key)["reason"], "key_cooling");
    assert_eq!(find(subset_key)["reason"], "model_subset_mismatch");
    assert_eq!(find(subset_key)["ok"], false);
}

/// 降级链预览：主模型无渠道时，报告链上每一环的可投性与不可投原因。
#[tokio::test]
async fn fallback_chain_preview() {
    let env = setup().await;
    let a = env.name("dg-fb-a");
    let b = env.name("dg-fb-b");
    okapi_store::provision::create_model_ratio(&env.pg, &a, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    okapi_store::provision::create_model_ratio(&env.pg, &b, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &env.pg,
        &env.name("dg-fb-ch"),
        "openai",
        "http://127.0.0.1:9/v1",
        "cred",
        &[b.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query!(
        r#"UPDATE models SET fallback_models = $2 WHERE model_name = $1"#,
        a,
        json!([b, format!("ghost-{}", env.suffix)])
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let report = diagnose_json(&env, &format!("model={a}")).await;
    assert_eq!(report["verdict"], "no_channel_serves_model");
    let fallbacks = report["fallbacks"].as_array().unwrap();
    assert_eq!(fallbacks.len(), 2);
    assert_eq!(fallbacks[0]["model"], b.as_str());
    assert_eq!(fallbacks[0]["viable"], true);
    assert_eq!(fallbacks[0]["candidates"], 1);
    assert_eq!(fallbacks[1]["viable"], false);
    assert_eq!(fallbacks[1]["reason"], "missing_or_disabled");
}

/// 权限：诊断挂 channel.read，普通用户 403。
#[tokio::test]
async fn requires_channel_read() {
    let env = setup().await;
    let user_id = okapi_store::provision::create_user(&env.pg, &format!("diag-u-{}", env.suffix))
        .await
        .unwrap();
    let token = format!("sk-okapi-diag-u-{}", env.suffix);
    okapi_store::provision::create_api_key(&env.pg, user_id, &hash(&token), "sk-diag-u")
        .await
        .unwrap();
    let resp = diagnose(&env, &token, "model=whatever").await;
    assert_eq!(resp.status(), 403);
}
