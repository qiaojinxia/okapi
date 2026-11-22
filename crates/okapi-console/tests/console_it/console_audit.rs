//! 审计读取面验收（IMPLEMENTATION §11.15）：管理写操作 → `/admin/audit` 可查且操作者
//! 有名字回填；动作前缀 / 对象 / 操作者三维过滤；游标翻页；登录成功与失败都落审计，
//! 失败按邮箱归到真实用户名下并可在 `/api/me/logins` 看到；`audit.read` 独立守卫。

use okapi_console as console;
use okapi_gateway as gateway;
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
    okapi_store::test_support::assert_isolated();
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
    assert_eq!(empty["total"], 0);
}

/// 页码分页：`limit` + `offset` 按 id 倒序切页，`total` 是过滤后的总数，页与页之间不重不漏。
#[tokio::test]
async fn audit_pages_by_offset_with_a_total() {
    let env = setup().await;
    for i in 0..5 {
        let (status, _) = post(
            &env,
            "/admin/pools",
            &env.super_token,
            json!({"pool_code": format!("au-pg-{i}-{}", env.suffix), "routing_strategy": "least_latency"}),
        )
        .await;
        assert_eq!(status, 200);
    }
    let (_, probe) = get(
        &env,
        "/admin/audit?action=channel.upsert_pool&limit=200",
        &env.super_token,
    )
    .await;
    let actor = probe["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["target"] == format!("au-pg-0-{}", env.suffix))
        .map(|r| r["actor"].as_str().unwrap().to_owned())
        .expect("本轮写入可查");
    let page = |offset: i64| {
        let env = &env;
        let actor = actor.clone();
        async move {
            let (status, body) = get(
                env,
                &format!(
                    "/admin/audit?actor={actor}&action=channel.upsert_pool&limit=2&offset={offset}"
                ),
                &env.super_token,
            )
            .await;
            assert_eq!(status, 200, "{body}");
            body
        }
    };
    let mut ids = Vec::new();
    for (offset, len, more) in [(0, 2, true), (2, 2, true), (4, 1, false)] {
        let body = page(offset).await;
        assert_eq!(body["total"], 5, "offset={offset}: {body}");
        assert_eq!(body["has_more"], more, "offset={offset}");
        let rows = body["data"].as_array().unwrap();
        assert_eq!(rows.len(), len, "offset={offset}");
        ids.extend(rows.iter().map(|r| r["id"].as_i64().unwrap()));
    }
    let mut sorted = ids.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    assert_eq!(ids, sorted, "三页拼起来严格倒序、不重复");
    assert_eq!(ids.len(), 5);
    let past = page(10).await;
    assert_eq!(past["data"], json!([]));
    assert_eq!(past["total"], 5);
    assert_eq!(past["has_more"], false);
}

/// 邮箱最长可到 254、失败登录更是任意长的串，而审计的对象列只有 128：此前原样写会超宽，整条审计丢失
/// （连成功登录也一样）。现在对象截到列宽，成功与失败的登录都照样留痕。
#[tokio::test]
async fn login_audit_is_kept_when_the_email_is_longer_than_the_target_column() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let ip = format!("198.51.100.{}", 1 + (rand::random::<u8>() % 250));
    let login = |email: String, password: &'static str| {
        let request = client
            .post(format!("http://{}/auth/login", env.addr))
            .header("x-real-ip", &ip)
            .json(&json!({"email": email, "password": password}));
        async move { request.send().await.unwrap() }
    };

    // 不存在的、远超列宽的邮箱：401 之外还要留下一条 anon 的 user.login_failed，对象是前 128 个字符
    let junk = format!("junk-{}-{}@example.test", env.suffix, "x".repeat(250));
    assert_eq!(login(junk.clone(), "x").await.status(), 401);
    let head: String = junk.chars().take(128).collect();
    let (status, body) = get(
        &env,
        &format!("/admin/audit?action=user.login_failed&target={head}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["actor"], "anon");

    // 合法的长邮箱（注册校验放行到 254）：成功登录同样要留痕，归到真实用户名下
    let email = format!(
        "lg{}{}@{}.{}.test",
        env.suffix,
        "a".repeat(52),
        "b".repeat(63),
        "c".repeat(63)
    );
    assert!(email.len() > 128 && email.len() <= 254, "{}", email.len());
    let registered = client
        .post(format!("http://{}/auth/register", env.addr))
        .header("x-real-ip", &ip)
        .json(&json!({"email": email, "username": format!("au-lg-{}", env.suffix), "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), 200);
    assert_eq!(login(email.clone(), "hunter2-strong").await.status(), 200);
    let head: String = email.chars().take(128).collect();
    let (status, body) = get(
        &env,
        &format!("/admin/audit?action=user.login&target={head}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["action"], "user.login");
    assert!(rows[0]["actor"].as_str().unwrap().starts_with("user:"));
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

/// 审计兜底：管理端写接口成功、处理器却没写审计时补一条 `api.<方法>`（对象是路由模板）；
/// 处理器自己写过的不重复，失败的请求不补。
#[tokio::test]
async fn unaudited_admin_writes_get_one_generic_row() {
    let env = setup().await;
    let pool = format!("au-fb-{}", env.suffix);
    let (status, body) = post(
        &env,
        "/admin/pools",
        &env.super_token,
        json!({"pool_code": pool}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (_, rows) = get(
        &env,
        &format!("/admin/audit?target={pool}"),
        &env.super_token,
    )
    .await;
    let rows = rows["data"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["action"], "channel.upsert_pool");
    let actor = rows[0]["actor"].as_str().unwrap().to_owned();

    let (status, body) = post(
        &env,
        "/admin/channels/oauth/start",
        &env.super_token,
        json!({"provider": "codex"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = post(
        &env,
        "/admin/channels/oauth/start",
        &env.super_token,
        json!({"provider": "no-such-provider"}),
    )
    .await;
    assert_ne!(status, 200, "{body}");

    let (_, rows) = get(
        &env,
        &format!("/admin/audit?actor={actor}&action=api."),
        &env.super_token,
    )
    .await;
    let rows = rows["data"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "建池已自带审计、失败的请求不补，只剩一条：{rows:?}"
    );
    assert_eq!(rows[0]["action"], "api.post");
    assert_eq!(rows[0]["target"], "/admin/channels/oauth/start");
    assert_eq!(rows[0]["detail"]["path"], "/admin/channels/oauth/start");
    assert_eq!(rows[0]["detail"]["status"], 200);
    assert!(rows[0]["ip"].is_string(), "兜底审计也应写入来源 IP");
    assert!(rows[0]["actor_key_id"].as_i64().is_some());
}

/// 增强验证 1：管理写操作落真 IP / UA / trace_id / actor_key_id；IP(含 CIDR) / 关键词 / trace_id
/// 检索可用；删除保留 deleted 快照；设置更新保留 before/after 对比。
#[tokio::test]
async fn audit_captures_context_diffs_and_snapshots() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let pool = format!("au-ctx-{}", env.suffix);
    let ip = "198.51.100.77";
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let traceparent = format!("00-{trace_id}-00f067aa0ba902b7-01");

    let resp = client
        .post(format!("http://{}/admin/pools", env.addr))
        .bearer_auth(&env.super_token)
        .header("x-real-ip", ip)
        .header("user-agent", "okapi-audit-it/2.0")
        .header("traceparent", &traceparent)
        .json(&json!({"pool_code": pool, "routing_strategy": "least_latency"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    for query in [
        format!("/admin/audit?ip={ip}&target={pool}"),
        format!("/admin/audit?ip=198.51.100.0/24&target={pool}"),
        format!("/admin/audit?trace_id={trace_id}&target={pool}"),
        format!("/admin/audit?q={pool}"),
    ] {
        let (status, body) = get(&env, &query, &env.super_token).await;
        assert_eq!(status, 200, "{query}: {body}");
        let rows = body["data"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{query}: {body}");
        assert_eq!(rows[0]["ip"], ip);
        assert_eq!(rows[0]["user_agent"], "okapi-audit-it/2.0");
        assert_eq!(rows[0]["trace_id"], trace_id);
        assert!(rows[0]["actor_key_id"].as_i64().is_some());
    }
    let (bad_status, _) = get(&env, "/admin/audit?ip=not-an-ip", &env.super_token).await;
    assert_eq!(bad_status, 400);

    // 删除池 → 保留 deleted 快照（非空 {}）
    let del = client
        .delete(format!("http://{}/admin/pools/{pool}", env.addr))
        .bearer_auth(&env.super_token)
        .header("x-real-ip", ip)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 200);
    let (_, del_rows) = get(
        &env,
        &format!("/admin/audit?action=channel.delete_pool&target={pool}"),
        &env.super_token,
    )
    .await;
    assert_eq!(
        del_rows["data"][0]["detail"]["deleted"]["pool_code"],
        json!(pool)
    );
    assert_eq!(
        del_rows["data"][0]["detail"]["deleted"]["routing_strategy"],
        "least_latency"
    );

    // 设置两次写入 → 第二次带 before / after 对比。键名不能含 key / token / secret 这类词：
    // 那会被当成密钥类设置（`is_secret_key`），审计只记 redacted、不记前后值
    let setting_key = format!("audit_diff_probe_{}", env.suffix);
    for val in ["v1", "v2"] {
        let (status, _) = post(
            &env,
            "/admin/settings",
            &env.super_token,
            json!({"key": setting_key, "value": val}),
        )
        .await;
        assert_eq!(status, 200);
    }
    let (_, set_rows) = get(
        &env,
        &format!("/admin/audit?action=settings.set&target={setting_key}"),
        &env.super_token,
    )
    .await;
    assert_eq!(set_rows["data"][0]["detail"]["before"], "v1");
    assert_eq!(set_rows["data"][0]["detail"]["after"], "v2");
}

/// 增强验证 2：403 越权留痕 `auth.permission_denied`；CSV/JSON 导出并自审 `audit.export`。
#[tokio::test]
async fn audit_records_denials_and_supports_export() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let pool = format!("au-exp-{}", env.suffix);

    let (status, _) = post(
        &env,
        "/admin/pools",
        &env.super_token,
        json!({"pool_code": pool}),
    )
    .await;
    assert_eq!(status, 200);

    // 普通用户越权访问 /admin/audit → 403 且自动留痕 auth.permission_denied
    let denied = client
        .get(format!("http://{}/admin/audit", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", "198.51.100.88")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let (_, denied_rows) = get(
        &env,
        "/admin/audit?action=auth.permission_denied&ip=198.51.100.88",
        &env.super_token,
    )
    .await;
    assert!(
        !denied_rows["data"].as_array().unwrap().is_empty(),
        "越权 403 应落 auth.permission_denied：{denied_rows}"
    );
    assert_eq!(denied_rows["data"][0]["detail"]["permission"], "audit.read");

    // CSV 与 JSON 导出 + 自审 audit.export
    let csv_resp = client
        .get(format!(
            "http://{}/admin/audit/export?format=csv&target={pool}",
            env.addr
        ))
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap();
    assert_eq!(csv_resp.status(), 200);
    let csv_text = csv_resp.text().await.unwrap();
    assert!(csv_text.starts_with("\u{FEFF}id,created_at,actor,"));
    assert!(csv_text.contains(&pool));

    let json_resp = client
        .get(format!(
            "http://{}/admin/audit/export?format=json&target={pool}",
            env.addr
        ))
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap();
    assert_eq!(json_resp.status(), 200);
    let json_body: Value = json_resp.json().await.unwrap();
    assert_eq!(json_body["count"], 1);

    let (_, export_rows) = get(&env, "/admin/audit?action=audit.export", &env.super_token).await;
    assert!(
        export_rows["data"].as_array().unwrap().len() >= 2,
        "导出行为本身也应落 audit.export"
    );
}
