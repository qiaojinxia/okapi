//! 审计读取面验收（IMPLEMENTATION §11.15）：管理写操作 → `/admin/audit` 可查且操作者
//! 有名字回填；动作前缀 / 对象 / 操作者三维过滤；游标翻页；登录成功与失败都落审计，
//! 失败按邮箱归到真实用户名下并可在 `/api/me/logins` 看到；`audit.read` 独立守卫。

use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Env {
    addr: SocketAddr,
    super_token: String,
    super_name: String,
    user_token: String,
    suffix: String,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let super_name = format!("au-s-{suffix}");
    let super_id = okapi_store::provision::create_user(&pg, &super_name)
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-au-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-au-s")
        .await
        .unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("au-u-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-au-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-au-u")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = console::router(state);
    tokio::spawn(async move {
        // 按生产形态挂 connect info：转发头信任闸要拿 socket 对端做锚点（§14.2）
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Env {
        addr,
        super_token,
        super_name,
        user_token,
        suffix,
    }
}

async fn get(env: &Env, path: &str, token: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://{}{path}", env.addr))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    (
        resp.status().as_u16(),
        resp.json::<Value>().await.unwrap_or(Value::Null),
    )
}

async fn post(env: &Env, path: &str, token: &str, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}{path}", env.addr))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        resp.status().as_u16(),
        resp.json::<Value>().await.unwrap_or(Value::Null),
    )
}

/// 三个读取端点同一守卫 `audit.read`；普通用户 403，`/api/me/logins` 则人人可用（只看自己）。
#[tokio::test]
async fn audit_requires_audit_read() {
    let env = setup().await;
    for path in ["/admin/audit", "/admin/audit/actions"] {
        let (status, _) = get(&env, path, &env.user_token).await;
        assert_eq!(status, 403, "{path} 应拒绝无权限用户");
    }
    let (status, body) = get(&env, "/api/me/logins", &env.user_token).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"], json!([]));
}

/// 管理写操作 → 审计可查：操作者回填用户名、detail 原样、按动作前缀与对象过滤、游标翻页。
#[tokio::test]
async fn admin_writes_are_searchable() {
    let env = setup().await;
    let pool_a = format!("au-pool-a-{}", env.suffix);
    let pool_b = format!("au-pool-b-{}", env.suffix);
    for code in [&pool_a, &pool_b] {
        let (status, _) = post(
            &env,
            "/admin/pools",
            &env.super_token,
            json!({"pool_code": code, "routing_strategy": "least_latency"}),
        )
        .await;
        assert_eq!(status, 200);
    }

    // 对象精确匹配 + 动作前缀
    let (status, body) = get(
        &env,
        &format!("/admin/audit?action=channel.&target={pool_a}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "只该命中 pool_a 那一条：{body}");
    let row = &rows[0];
    assert_eq!(row["action"], "channel.upsert_pool");
    assert_eq!(row["target"], pool_a);
    assert_eq!(row["detail"]["routing_strategy"], "least_latency");
    assert_eq!(row["actor_info"]["kind"], "admin");
    assert_eq!(
        row["actor_info"]["label"], env.super_name,
        "操作者应回填用户名"
    );
    let actor = row["actor"].as_str().unwrap().to_owned();
    assert!(actor.starts_with("admin:"));

    // 按操作者过滤 + 游标：limit=1 两页拿到两条不同记录
    let (status, page1) = get(
        &env,
        &format!("/admin/audit?actor={actor}&action=channel.upsert_pool&limit=1"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(page1["has_more"], true);
    let first_id = page1["data"][0]["id"].as_i64().unwrap();
    assert_eq!(page1["next_before"], first_id);
    let (_, page2) = get(
        &env,
        &format!("/admin/audit?actor={actor}&action=channel.upsert_pool&limit=1&before={first_id}"),
        &env.super_token,
    )
    .await;
    let second_id = page2["data"][0]["id"].as_i64().unwrap();
    assert!(second_id < first_id, "游标翻页应严格倒序");
    assert_eq!(
        page2["data"][0]["target"], pool_a,
        "第二页是更早写入的 pool_a"
    );

    // 动作清单含本轮动作
    let (_, actions) = get(&env, "/admin/audit/actions", &env.super_token).await;
    assert!(
        actions["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "channel.upsert_pool"),
        "{actions}"
    );

    // 非法时间窗被 clamp、不存在的 actor 返回空而非报错
    let (status, empty) = get(
        &env,
        "/admin/audit?actor=admin:0&hours=999999",
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(empty["data"], json!([]));
    assert_eq!(empty["has_more"], false);
}

/// 登录审计：成功记 user.login；错密码记 user.login_failed 到真实用户名下（用户在
/// /api/me/logins 能看到"有人在试我的密码"）；不存在的邮箱记 anon；TOTP 缺失原因单独命名。
#[tokio::test]
async fn login_attempts_are_audited() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let email = format!("au-{}@ok.test", env.suffix);
    let ip = format!("198.51.100.{}", 1 + (rand::random::<u8>() % 250));
    let reg = client
        .post(format!("http://{}/auth/register", env.addr))
        .header("x-real-ip", &ip)
        .json(&json!({"email": email, "username": format!("au-web-{}", env.suffix), "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), 200);

    // 错密码 → 401，审计 user.login_failed（归到该邮箱的用户）
    let bad = client
        .post(format!("http://{}/auth/login", env.addr))
        .header("x-real-ip", &ip)
        .header("user-agent", "audit-test/1.0")
        .json(&json!({"email": email, "password": "wrong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    // 不存在的邮箱 → anon
    let ghost = format!("ghost-{}@ok.test", env.suffix);
    client
        .post(format!("http://{}/auth/login", env.addr))
        .header("x-real-ip", &ip)
        .json(&json!({"email": ghost, "password": "x"}))
        .send()
        .await
        .unwrap();
    // 成功
    let ok = client
        .post(format!("http://{}/auth/login", env.addr))
        .header("x-real-ip", &ip)
        .header("user-agent", "audit-test/1.0")
        .json(&json!({"email": email, "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let cookie = ok
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap()
        .to_owned();

    // 管理端按对象（邮箱）查：一条失败 + 一条成功，IP / UA / 原因齐备
    let (status, body) = get(
        &env,
        &format!("/admin/audit?action=user.login&target={email}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{body}");
    assert_eq!(rows[0]["action"], "user.login", "倒序：最新的是成功登录");
    assert_eq!(rows[0]["detail"]["ip"], ip);
    assert_eq!(rows[0]["detail"]["ua"], "audit-test/1.0");
    assert_eq!(rows[1]["action"], "user.login_failed");
    assert_eq!(rows[1]["detail"]["reason"], "invalid_credentials");
    assert_eq!(
        rows[0]["actor"], rows[1]["actor"],
        "失败尝试归到同一真实用户名下"
    );
    assert!(rows[0]["actor"].as_str().unwrap().starts_with("user:"));
    assert_eq!(rows[0]["actor_info"]["kind"], "user");

    let (_, ghost_rows) = get(
        &env,
        &format!("/admin/audit?action=user.login_failed&target={ghost}"),
        &env.super_token,
    )
    .await;
    assert_eq!(
        ghost_rows["data"][0]["actor"], "anon",
        "不存在的邮箱记 anon"
    );

    // 用户自己看：用会话兑 key 后读 /api/me/logins
    let minted: Value = client
        .post(format!("http://{}/auth/keys", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({"name": "au"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (status, mine) = get(&env, "/api/me/logins", minted["api_key"].as_str().unwrap()).await;
    assert_eq!(status, 200, "{mine}");
    let mine = mine["data"].as_array().unwrap();
    assert_eq!(mine.len(), 2);
    assert_eq!(mine[0]["ok"], true);
    assert_eq!(mine[0]["ip"], ip);
    assert_eq!(mine[1]["ok"], false);
    assert_eq!(mine[1]["reason"], "invalid_credentials");
    assert!(mine[0]["ua"].as_str().is_some());
}
