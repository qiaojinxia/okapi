//! 渠道测活端点验收：可达（2xx ok）/ 凭证问题（401 可达不 ok）/ 不可达。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

async fn mock_models(headers: axum::http::HeaderMap) -> axum::response::Response {
    if headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "Bearer good-credential")
    {
        axum::Json(json!({"object": "list",
            "data": [{"id": "gpt-x-2"}, {"id": "gpt-x-1"}, {"id": "gpt-x-2"}]}))
        .into_response()
    } else {
        (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": {"message": "bad key"}})),
        )
            .into_response()
    }
}

async fn spawn_mock() -> SocketAddr {
    let router = Router::new().route("/v1/models", get(mock_models));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
    admin_token: String,
    mock: SocketAddr,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-test-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(admin_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &key_hash, "sk-okapi-test")
        .await
        .unwrap();

    let mock = spawn_mock().await;
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
        mock,
    }
}

async fn mk_channel(env: &TestEnv, credential: &str, base: &str) -> i64 {
    let suffix = Uuid::new_v4().simple().to_string();
    let (id, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("t-{suffix}"),
        "openai",
        base,
        credential,
        &["m-x"],
        false,
        None,
    )
    .await
    .unwrap();
    id
}

async fn test_channel(env: &TestEnv, id: i64) -> Value {
    reqwest::Client::new()
        .post(format!("http://{}/admin/channels/{id}/test", env.addr))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn channel_test_probes_reachability() {
    let env = setup().await;

    // 凭证正确：ok + 2xx
    let good = mk_channel(&env, "good-credential", &format!("http://{}/v1", env.mock)).await;
    let r = test_channel(&env, good).await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["http_status"], 200);
    assert!(r["latency_ms"].as_i64().unwrap() >= 0);

    // 凭证错误：可达但不 ok（401 供管理员判断）
    let bad = mk_channel(&env, "bad-credential", &format!("http://{}/v1", env.mock)).await;
    let r = test_channel(&env, bad).await;
    assert_eq!(r["ok"], false);
    assert_eq!(r["http_status"], 401);

    // 不可达：connect 失败带 error_code
    let dead = mk_channel(&env, "x", "http://127.0.0.1:9/v1").await;
    let r = test_channel(&env, dead).await;
    assert_eq!(r["ok"], false);
    assert!(r["error_code"].is_string(), "{r}");
    assert!(r["at"].is_string(), "测活结果带时间戳供列表回填：{r}");

    // 测活结果留痕：列表页每行回填 last_test（new-api response_time/test_time 语义）
    let list: Value = reqwest::Client::new()
        .get(format!("http://{}/admin/channels", env.addr))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = |id: i64| {
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"].as_i64() == Some(id))
            .cloned()
            .unwrap()
    };
    assert_eq!(row(good)["last_test"]["ok"], true, "{}", row(good));
    assert_eq!(row(good)["last_test"]["http_status"], 200);
    assert_eq!(row(bad)["last_test"]["http_status"], 401);
    assert_eq!(row(dead)["last_test"]["ok"], false);
    assert!(row(dead)["last_test"]["error_code"].is_string());
    let never = mk_channel(&env, "never-tested", &format!("http://{}/v1", env.mock)).await;
    let list2: Value = reqwest::Client::new()
        .get(format!("http://{}/admin/channels", env.addr))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let never_row = list2["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_i64() == Some(never))
        .unwrap();
    assert!(
        never_row["last_test"].is_null(),
        "没测过的渠道 last_test 为 null"
    );

    // 上游模型发现（§11.3）：openai 形状 data[].id
    let models: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/channels/{good}/fetch-models",
            env.addr
        ))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        models["models"],
        serde_json::json!(["gpt-x-1", "gpt-x-2"]),
        "{models}"
    );

    // 审计留痕
    let audits = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs WHERE action = 'channel.test'"#
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(audits >= 3);
}
