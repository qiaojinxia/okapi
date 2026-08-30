//! M3 MCP 只读工具面验收（IMPLEMENTATION §7）：
//! initialize / tools/list RBAC 过滤 / query_balance / explain_bill own 语义 /
//! search_logs 管理查询。依赖 .env（scripts/dev-deps.sh up）。

use okapi::console;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestEnv { pg, addr }
}

/// (user_id, token)；role 100 = 超管。
async fn mk_user(pg: &PgPool, role: i16) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(pg, &format!("mcp-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = $2 WHERE id = $1", user_id, role)
        .execute(pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-mcp-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(pg, user_id, &key_hash, "sk-okapi-mcp")
        .await
        .unwrap();
    (user_id, token)
}

async fn rpc(env: &TestEnv, token: &str, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(format!("http://{}/mcp", env.addr))
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn tool_names(resp: &Value) -> Vec<String> {
    resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

/// initialize + tools/list：普通用户只见用户级工具，管理员见全量。
#[tokio::test]
async fn tools_list_filtered_by_rbac() {
    let env = setup().await;
    let (_, user_token) = mk_user(&env.pg, 1).await;
    let (_, admin_token) = mk_user(&env.pg, 100).await;

    let init = rpc(&env, &user_token, "initialize", json!({})).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "okapi-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let user_tools = tool_names(&rpc(&env, &user_token, "tools/list", json!({})).await);
    assert!(user_tools.contains(&"query_balance".to_owned()));
    assert!(user_tools.contains(&"explain_bill".to_owned()));
    assert!(
        !user_tools.contains(&"reconciliation_status".to_owned()),
        "普通用户不得见管理工具"
    );

    let admin_tools = tool_names(&rpc(&env, &admin_token, "tools/list", json!({})).await);
    assert!(admin_tools.contains(&"reconciliation_status".to_owned()));
    assert!(admin_tools.contains(&"usage_stats".to_owned()));
    assert!(admin_tools.contains(&"dlq_list".to_owned()));

    // 越权调用被拒
    let denied = rpc(
        &env,
        &user_token,
        "tools/call",
        json!({"name": "search_logs", "arguments": {}}),
    )
    .await;
    assert_eq!(denied["error"]["message"], "permission_denied");
}

/// query_balance 结构化输出 + 无效鉴权 401。
#[tokio::test]
async fn query_balance_and_auth() {
    let env = setup().await;
    let (user_id, token) = mk_user(&env.pg, 1).await;
    // 直接入账（走 ledger）
    let state = gateway::build_state(
        &std::env::var("DATABASE_URL").unwrap(),
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "test-node",
        None,
        None,
    )
    .await
    .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_234_567))
        .await
        .unwrap();

    let resp = rpc(
        &env,
        &token,
        "tools/call",
        json!({"name": "query_balance", "arguments": {}}),
    )
    .await;
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["user_id"].as_i64(), Some(user_id));
    assert_eq!(sc["balance_micro"].as_i64(), Some(1_234_567));
    assert_eq!(resp["result"]["isError"], false);

    let unauthorized = reqwest::Client::new()
        .post(format!("http://{}/mcp", env.addr))
        .bearer_auth("sk-bogus")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
}

/// explain_bill：own 语义（他人记录 403），管理员可查全部。
#[tokio::test]
async fn explain_bill_own_scope() {
    let env = setup().await;
    let (owner_id, owner_token) = mk_user(&env.pg, 1).await;
    let (_, other_token) = mk_user(&env.pg, 1).await;
    let (_, admin_token) = mk_user(&env.pg, 100).await;

    let request_id = Uuid::new_v4();
    sqlx::query!(
        r#"INSERT INTO billing_records
           (request_id, log_type, user_id, api_key_id, group_code, model_name, status,
            prompt_tokens, completion_tokens, amount_micro, original_amount_micro,
            pricing_snapshot)
           VALUES ($1, 2, $2, 1, 'default', 'm-mcp', 20, 100, 20, 240, 240,
                   '{"model_ratio": "1"}')"#,
        request_id,
        owner_id
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let args = json!({"name": "explain_bill", "arguments": {"request_id": request_id}});
    let own = rpc(&env, &owner_token, "tools/call", args.clone()).await;
    let sc = &own["result"]["structuredContent"];
    assert_eq!(sc["amount_micro"].as_i64(), Some(240));
    assert_eq!(sc["pricing_snapshot"]["model_ratio"], "1");

    let stranger = rpc(&env, &other_token, "tools/call", args.clone()).await;
    assert_eq!(stranger["result"]["isError"], true, "他人记录必须拒绝");

    let admin = rpc(&env, &admin_token, "tools/call", args).await;
    assert_eq!(admin["result"]["isError"], false, "管理员可解释任意账单");
}

/// search_logs 管理查询 + usage_stats 维度校验。
#[tokio::test]
async fn admin_query_tools() {
    let env = setup().await;
    let (target_id, _) = mk_user(&env.pg, 1).await;
    let (_, admin_token) = mk_user(&env.pg, 100).await;
    sqlx::query!(
        r#"INSERT INTO billing_records
           (request_id, log_type, user_id, api_key_id, group_code, model_name, status,
            prompt_tokens, completion_tokens, amount_micro, original_amount_micro)
           VALUES ($1, 2, $2, 1, 'default', 'm-mcp-search', 20, 10, 5, 100, 100)"#,
        Uuid::new_v4(),
        target_id
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let found = rpc(
        &env,
        &admin_token,
        "tools/call",
        json!({"name": "search_logs", "arguments": {"user_id": target_id}}),
    )
    .await;
    let data = found["result"]["structuredContent"]["data"]
        .as_array()
        .unwrap();
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["model"], "m-mcp-search");

    let bad_dim = rpc(
        &env,
        &admin_token,
        "tools/call",
        json!({"name": "usage_stats", "arguments": {"dimension": "drop table"}}),
    )
    .await;
    assert_eq!(bad_dim["result"]["isError"], true, "非法维度必须拒绝");
}
