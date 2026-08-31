//! 用户列表与角色管理端点验收：补齐前管理端只能按 ID 操作、没有列表与角色下拉。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::Value;
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Env {
    pg: PgPool,
    addr: SocketAddr,
    super_token: String,
    user_token: String,
    user_id: i64,
    username: String,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let super_id = okapi_store::provision::create_user(&pg, &format!("us-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-usr-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-usr-s")
        .await
        .unwrap();

    let username = format!("uu-{suffix}");
    let user_id = okapi_store::provision::create_user(&pg, &username)
        .await
        .unwrap();
    let user_token = format!("sk-okapi-usr-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-usr-u")
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
        addr,
        super_token,
        user_token,
        user_id,
        username,
    }
}

async fn req(
    env: &Env,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let client = reqwest::Client::new();
    let url = format!("http://{}{path}", env.addr);
    let mut rb = if method == "POST" {
        client.post(url)
    } else {
        client.get(url)
    }
    .bearer_auth(token);
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let resp = rb.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json::<Value>().await.unwrap_or(Value::Null))
}

/// 列表可按用户名精确检索；普通用户无 user.manage 一律 403。
#[tokio::test]
async fn user_list_search_and_rbac() {
    let env = setup().await;

    let (status, _) = req(&env, "GET", "/admin/users", &env.user_token, None).await;
    assert_eq!(status, 403, "无 user.manage 应拒绝");

    let path = format!("/admin/users?q={}", env.username);
    let (status, body) = req(&env, "GET", &path, &env.super_token, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 1, "模糊查询应恰好命中本用例用户：{body}");
    let row = &body["data"][0];
    assert_eq!(row["id"], env.user_id);
    assert_eq!(row["username"], env.username);
    assert_eq!(row["role"], 1, "新建用户为普通角色");
    assert_eq!(row["price_multiplier"], "1.0000", "专属倍率缺省 1");

    // 注入面：搜索串走 bind 参数，特殊字符不得破坏语义
    let (status, body) = req(
        &env,
        "GET",
        "/admin/users?q=%27%20OR%201%3D1%20--",
        &env.super_token,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 0, "引号注入应作为普通字面量匹配：{body}");
}

/// 角色：创建自定义角色 → 出现在列表 → 分配给用户后列表回显绑定。
#[tokio::test]
async fn custom_role_create_list_and_assign() {
    let env = setup().await;
    let code = format!("r-{}", Uuid::new_v4().simple());

    let (status, created) = req(
        &env,
        "POST",
        "/admin/roles",
        &env.super_token,
        Some(serde_json::json!({
            "role_code": code,
            "display_name": "只读运营",
            "permissions": ["channel.read", "billing.read"],
        })),
    )
    .await;
    assert_eq!(status, 200, "建角色应成功：{created}");
    let role_id = created["admin_role_id"]
        .as_i64()
        .expect("应返回 admin_role_id");

    let (status, list) = req(&env, "GET", "/admin/roles", &env.super_token, None).await;
    assert_eq!(status, 200);
    let mine = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["role_code"] == code.as_str())
        .expect("新角色应出现在列表");
    assert_eq!(mine["permissions"][0], "channel.read");

    // 提为管理员并绑定自定义角色
    let (status, _) = req(
        &env,
        "POST",
        &format!("/admin/users/{}/role", env.user_id),
        &env.super_token,
        Some(serde_json::json!({ "role": 10, "admin_role_id": role_id })),
    )
    .await;
    assert_eq!(status, 200);

    let path = format!("/admin/users?q={}", env.username);
    let (_, body) = req(&env, "GET", &path, &env.super_token, None).await;
    let row = &body["data"][0];
    assert_eq!(row["role"], 10, "角色应已提升");
    assert_eq!(row["admin_role_id"], role_id, "自定义角色绑定应回显");

    // 非超管不得改角色（防自我提权）
    let (status, _) = req(
        &env,
        "POST",
        &format!("/admin/users/{}/role", env.user_id),
        &env.user_token,
        Some(serde_json::json!({ "role": 100 })),
    )
    .await;
    assert_eq!(status, 403, "改角色必须强制超管");
    let _ = &env.pg;
}
