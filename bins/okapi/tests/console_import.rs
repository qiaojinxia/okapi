//! new-api ratio JSON 一键导入验收（M3 验收项）：
//! 数字/字符串字面量混合、model_price → per_call、非法值 skipped、
//! 导入后发布编译通过。依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
    admin_token: String,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("imp-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-imp-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(admin_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &key_hash, "sk-okapi-imp")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestEnv {
        pg,
        addr,
        admin_token,
    }
}

#[tokio::test]
async fn import_newapi_ratio_json_end_to_end() {
    let env = setup().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let m1 = format!("na-{}-gpt", &suffix[..8]);
    let m2 = format!("na-{}-cheap", &suffix[..8]);
    let mj = format!("na-{}-mj", &suffix[..8]);
    let bad = format!("na-{}-bad", &suffix[..8]);

    // new-api 官方形状：数字字面量（含小数）+ 字符串混合
    let payload = json!({
        "model_ratio": {
            m1.clone(): 15,
            m2.clone(): 0.75,
            bad.clone(): "not-a-number",
        },
        "completion_ratio": { m1.clone(): 2 },
        "cache_ratio": { m1.clone(): "0.5" },
        "model_price": { mj.clone(): 0.1 },
    });
    let resp: Value = reqwest::Client::new()
        .post(format!("http://{}/admin/pricing/import-newapi", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&payload)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["imported"], 3, "{resp}");
    assert_eq!(resp["skipped"], json!([bad]), "非法值必须跳过并报告");
    assert_eq!(resp["published"], false, "导入不得自动发布");

    // 入库精度校验（禁浮点误差）：数据库数值级比较
    let row = sqlx::query!(
        r#"SELECT (p.model_ratio = 15) AS "ratio_ok!",
                  (p.completion_ratio = 2) AS "completion_ok!",
                  (p.cache_ratio = 0.5) AS "cache_ok!"
           FROM models m JOIN model_pricing p ON p.model_id = m.id WHERE m.model_name = $1"#,
        m1
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(row.ratio_ok && row.completion_ok && row.cache_ok);

    let cheap_ok = sqlx::query_scalar!(
        r#"SELECT (p.model_ratio = 0.75) AS "ok!" FROM models m
           JOIN model_pricing p ON p.model_id = m.id WHERE m.model_name = $1"#,
        m2
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(cheap_ok, "0.75 精确入库无浮点尾巴");

    let per_call = sqlx::query!(
        r#"SELECT p.pricing_mode, p.per_call_price_micro FROM models m
           JOIN model_pricing p ON p.model_id = m.id WHERE m.model_name = $1"#,
        mj
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(per_call.pricing_mode, "per_call");
    assert_eq!(
        per_call.per_call_price_micro,
        Some(100_000),
        "$0.1 = 100000 micro"
    );

    // 导入后发布：编译校验必须通过
    let publish: Value = reqwest::Client::new()
        .post(format!("http://{}/admin/pricing/publish", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(publish["epoch"].as_i64().unwrap() > 0, "{publish}");
}
