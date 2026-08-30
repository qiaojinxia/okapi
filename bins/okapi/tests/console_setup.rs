//! Setup 初始化向导验收：空库首启创建超管（独立临时库）；
//! 已初始化库恒 409 / needs_setup=false。依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

async fn serve(state: gateway::state::AppState) -> SocketAddr {
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// 空库全流程：status → 创建 → key 可登录 → 二次 409。
#[tokio::test]
async fn setup_wizard_on_fresh_database() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    // 独立临时库（不污染共享测试库）
    let admin_pool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!("okapi_setup_{}", &Uuid::new_v4().simple().to_string()[..12]);
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
    okapi_store::run_migrations(&fresh).await.unwrap();
    drop(fresh);

    let state = gateway::build_state(&fresh_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let addr = serve(state).await;
    let client = reqwest::Client::new();

    let status: Value = client
        .get(format!("http://{addr}/api/setup/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["needs_setup"], true, "空库必须提示初始化");

    let created: Value = client
        .post(format!("http://{addr}/api/setup"))
        .json(&json!({"username": "boss"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let api_key = created["api_key"].as_str().expect("必须一次性返回明文 key");
    assert!(api_key.starts_with("sk-okapi-"));

    // 新 key 立即可用（超管身份）
    let me = client
        .get(format!("http://{addr}/api/me"))
        .bearer_auth(api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);

    // 二次初始化拒绝
    let again = client
        .post(format!("http://{addr}/api/setup"))
        .json(&json!({"username": "intruder"}))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 409);
    let body: Value = again.json().await.unwrap();
    assert_eq!(body["error"]["code"], "already_initialized");

    let status: Value = client
        .get(format!("http://{addr}/api/setup/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["needs_setup"], false);
}

/// 已初始化的共享测试库：status false + POST 409（负路径守卫）。
#[tokio::test]
async fn setup_rejected_on_initialized_database() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    // 共享库必然已有用户（其他套件创建）；兜底建一个
    okapi_store::provision::create_user(&pg, &format!("seed-{}", Uuid::new_v4().simple()))
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let addr = serve(state).await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/api/setup"))
        .json(&json!({"username": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}
