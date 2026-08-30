//! SSRF 校验验收（§14.4）：缺省策略拒 http/私网/环回/localhost，
//! 公网 https 放行；用独立临时库保证缺省策略（共享库被其他套件放行）。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

#[tokio::test]
async fn ssrf_default_policy_blocks_private_targets() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    // 独立临时库：缺省无 ssrf_policy
    let admin_pool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!("okapi_ssrf_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin_pool)
    .await
    .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let fresh_url = format!("{base}/{db_name}");
    let pg = okapi_store::connect_pg(&fresh_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 超管
    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("ss-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-ss-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &hash, "sk-okapi-ss")
        .await
        .unwrap();

    let state = gateway::build_state(&fresh_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let create = |api_base: &str| {
        let client = client.clone();
        let token = token.clone();
        let api_base = api_base.to_owned();
        let name = format!("ch-{}", Uuid::new_v4().simple());
        async move {
            client
                .post(format!("http://{addr}/admin/channels"))
                .bearer_auth(&token)
                .json(&json!({"name": name, "api_base": api_base,
                    "credential": "c", "models": ["m-x"]}))
                .send()
                .await
                .unwrap()
        }
    };

    // http：缺省仅 https
    let resp = create("http://api.example.com/v1").await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["param"], "api_base_scheme_https_only");

    // 私网/环回/localhost（https 也拒）
    for target in [
        "https://10.0.0.8/v1",
        "https://192.168.1.1/v1",
        "https://127.0.0.1:8080/v1",
        "https://[::1]/v1",
        "https://localhost/v1",
    ] {
        let resp = create(target).await;
        assert_eq!(resp.status(), 400, "{target} 必须被拒");
        let body: Value = resp.json().await.unwrap();
        assert_eq!(
            body["error"]["param"], "api_base_private_target",
            "{target}"
        );
    }

    // 公网 https：放行
    let resp = create("https://api.example.com/v1").await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
}
