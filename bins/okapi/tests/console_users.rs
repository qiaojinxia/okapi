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
    ledger: okapi_ledger::BalanceLedger,
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
    let ledger = state.ledger.clone();
    let app = console::router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    Env {
        pg,
        ledger,
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
    let mut rb = match method {
        "POST" => client.post(url),
        "DELETE" => client.delete(url),
        _ => client.get(url),
    }
    .bearer_auth(token);
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let resp = rb.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json::<Value>().await.unwrap_or(Value::Null))
}

async fn mk_role(env: &Env, code: &str, permissions: &[&str]) -> i64 {
    let (status, created) = req(
        env,
        "POST",
        "/admin/roles",
        &env.super_token,
        Some(serde_json::json!({
            "role_code": code, "display_name": "待删", "permissions": permissions,
        })),
    )
    .await;
    assert_eq!(status, 200, "建角色应成功：{created}");
    created["admin_role_id"].as_i64().unwrap()
}

async fn bind_role(env: &Env, user_id: i64, role: i64, admin_role_id: Option<i64>) {
    let (status, body) = req(
        env,
        "POST",
        &format!("/admin/users/{user_id}/role"),
        &env.super_token,
        Some(serde_json::json!({ "role": role, "admin_role_id": admin_role_id })),
    )
    .await;
    assert_eq!(status, 200, "改角色应成功：{body}");
}

/// 列表可按用户名精确检索；普通用户无 user.manage 一律 403。
#[tokio::test]
async fn user_list_search_and_rbac() {
    let env = setup().await;

    let (status, _) = req(&env, "GET", "/admin/users", &env.user_token, None).await;
    assert_eq!(status, 403, "普通用户无 user.read 应拒绝");

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

/// 代客用量视图：无 CH 时用量为空但余额变动史照常（不连坐）；actor 对管理面可见；
/// 无 user.assist 权限 403。
#[tokio::test]
async fn user_usage_degrades_without_ch_and_shows_ledger() {
    let env = setup().await;
    for (kind, delta, actor) in [
        ("recharge", 5_000_000_i64, "system:payment"),
        ("adjust", -200_000, "admin:1"),
    ] {
        sqlx::query!(
            r#"INSERT INTO billing_events (user_id, event_type, delta_micro, payload, actor)
               VALUES ($1, $2, $3, '{"tags":["correction"],"reason":"dup charge"}', $4)"#,
            env.user_id,
            kind,
            delta,
            actor
        )
        .execute(&env.pg)
        .await
        .unwrap();
    }

    let path = format!("/admin/users/{}/usage?days=7", env.user_id);
    let (status, _) = req(&env, "GET", &path, &env.user_token, None).await;
    assert_eq!(status, 403, "无 user.assist 应拒绝");

    let (status, body) = req(&env, "GET", &path, &env.super_token, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["stats_available"], false, "本套件 state 无 CH");
    assert_eq!(body["daily"].as_array().map(Vec::len), Some(0));
    let ledger = body["ledger"].as_array().unwrap();
    assert_eq!(ledger.len(), 2, "余额变动史不依赖 CH：{body}");
    assert_eq!(ledger[0]["event_type"], "adjust", "最新在前");
    assert_eq!(ledger[0]["delta_micro"], -200_000);
    assert_eq!(ledger[0]["actor"], "admin:1", "管理面要看得见谁调的账");
    assert_eq!(ledger[0]["reason"], "dup charge");
    assert_eq!(ledger[0]["tags"][0], "correction");
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

    // 同 code 再写 = 编辑（此前纯 INSERT 撞唯一键 500，角色一经创建不可修改）：
    // 权限集合更新、id 不变、已绑定用户的鉴权缓存被全量失效后按新集合放行
    let (status, again) = req(
        &env,
        "POST",
        "/admin/roles",
        &env.super_token,
        Some(serde_json::json!({
            "role_code": code,
            "display_name": "只读运营（含用户）",
            "permissions": ["channel.read", "billing.read", "user.read"],
        })),
    )
    .await;
    assert_eq!(status, 200, "同 code 应 upsert 而非冲突：{again}");
    assert_eq!(
        again["admin_role_id"], role_id,
        "编辑不换 id，绑定关系不受影响"
    );
    let (_, list) = req(&env, "GET", "/admin/roles", &env.super_token, None).await;
    let mine = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["role_code"] == code.as_str())
        .unwrap();
    assert_eq!(mine["display_name"], "只读运营（含用户）");
    assert_eq!(mine["permissions"].as_array().unwrap().len(), 3);
    // 新增的 user.read 立即生效：该用户现在能读用户列表
    let (status, _) = req(&env, "GET", "/admin/users", &env.user_token, None).await;
    assert_eq!(status, 200, "补上 user.read 后应即时放行（鉴权缓存已失效）");

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

/// 删角色：`DELETE /admin/roles/{code}` 此前零集成覆盖（前端 e2e 桩过 409，后端从没被打过）。
///
/// 四件事一并钉住：带 `role.manage` 的自定义管理员也删不了（防提权链，只认超管）；还有活人绑着 409
/// `role_in_use`；解绑后 200 且落审计、从列表消失；不认识的 code 404。最后一段是本轮抓到的洞——
/// 用户软删只置 `deleted_at`，`admin_role_id` 原样留着，而 `users.admin_role_id` 是无 ON DELETE 的
/// 外键，于是"活人计数"过闸、`DELETE FROM admin_roles` 撞外键 500，角色从此永远删不掉。
#[tokio::test]
async fn role_delete_guards_live_bindings_and_ignores_deleted_users() {
    let env = setup().await;
    let code = format!("rd-{}", Uuid::new_v4().simple());
    // 给角色配上 role.manage，用来验证"有这个权限点也删不了"
    let role_id = mk_role(&env, &code, &["role.manage"]).await;
    bind_role(&env, env.user_id, 10, Some(role_id)).await;

    // 拿着 role.manage 的管理员照样删不动：角色变更只认超管
    let (status, body) = req(
        &env,
        "DELETE",
        &format!("/admin/roles/{code}"),
        &env.user_token,
        None,
    )
    .await;
    assert_eq!(status, 403, "有 role.manage 也不该能删角色：{body}");

    // 还有活人绑着：409 role_in_use，不能把一批人一次性掉权
    let (status, body) = req(
        &env,
        "DELETE",
        &format!("/admin/roles/{code}"),
        &env.super_token,
        None,
    )
    .await;
    assert_eq!(status, 409, "有人绑着应冲突：{body}");
    assert_eq!(body["error"]["code"], "role_in_use", "{body}");

    // 解绑后可删：200、审计留痕、列表里没了
    bind_role(&env, env.user_id, 1, None).await;
    let (status, body) = req(
        &env,
        "DELETE",
        &format!("/admin/roles/{code}"),
        &env.super_token,
        None,
    )
    .await;
    assert_eq!(status, 200, "解绑后应可删：{body}");
    let logged = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs WHERE action = 'role.delete' AND target = $1"#,
        code
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(logged, 1, "删角色要落审计 role.delete");
    let (_, list) = req(&env, "GET", "/admin/roles", &env.super_token, None).await;
    assert!(
        !list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["role_code"] == code.as_str()),
        "删掉的角色不该还在列表：{list}"
    );

    // 不认识的 code：404（不是 200 ok）
    let (status, _) = req(
        &env,
        "DELETE",
        &format!("/admin/roles/{code}-nope"),
        &env.super_token,
        None,
    )
    .await;
    assert_eq!(status, 404);

    // 只剩墓碑绑着：软删的人早已 status=2、令牌全停，留着的 admin_role_id 是条死引用，
    // 不该让角色永远删不掉（此前撞外键 500）
    let tomb_code = format!("{code}-t");
    let tomb_role = mk_role(&env, &tomb_code, &["channel.read"]).await;
    let doomed = okapi_store::provision::create_user(&env.pg, &format!("{}-d", env.username))
        .await
        .unwrap();
    bind_role(&env, doomed, 10, Some(tomb_role)).await;
    let (status, body) = req(
        &env,
        "POST",
        &format!("/admin/users/{doomed}/manage"),
        &env.super_token,
        Some(serde_json::json!({ "action": "delete" })),
    )
    .await;
    assert_eq!(status, 200, "软删用户应成功：{body}");
    let (status, body) = req(
        &env,
        "DELETE",
        &format!("/admin/roles/{tomb_code}"),
        &env.super_token,
        None,
    )
    .await;
    assert_eq!(status, 200, "只剩软删用户引用时应能删：{body}");
    let dangling = sqlx::query_scalar!(r#"SELECT admin_role_id FROM users WHERE id = $1"#, doomed)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert!(dangling.is_none(), "墓碑上的死引用要一并清掉");
}

/// 余额有效期的**管理面那一半**此前零集成覆盖：`worker_m2::balance_expiry_drains_and_records`
/// 验的是清零扫描，但它是拿裸 SQL 写的 `balance_expires_at`——真正给用户设有效期的
/// `POST /admin/users/{id}/balance-expiry` 从没被打过。这里把两半接上：管理面设进去的时间，
/// worker 扫得到、认得出、到点真清零。
#[tokio::test]
async fn balance_expiry_endpoint_feeds_the_worker_sweep() {
    let env = setup().await;
    let set_expiry = async |token: &str, user: i64, at: Value| {
        req(
            &env,
            "POST",
            &format!("/admin/users/{user}/balance-expiry"),
            token,
            Some(serde_json::json!({ "expires_at": at })),
        )
        .await
    };
    let stored = async |user: i64| {
        sqlx::query_scalar!(
            r#"SELECT balance_expires_at FROM users WHERE id = $1"#,
            user
        )
        .fetch_one(&env.pg)
        .await
        .unwrap()
    };

    // 普通用户没有 user.balance_adjust：动不了别人的余额有效期
    let (status, _) = set_expiry(
        &env.user_token,
        env.user_id,
        serde_json::json!("2099-01-01T00:00:00Z"),
    )
    .await;
    assert_eq!(status, 403);

    env.ledger
        .credit(env.user_id, okapi_domain::Money::from_micros(7_000))
        .await
        .unwrap();

    // 设到未来：落库、留痕，扫描一轮不碰它
    let future = chrono::Utc::now() + chrono::Duration::hours(1);
    let (status, body) = set_expiry(
        &env.super_token,
        env.user_id,
        serde_json::json!(future.to_rfc3339()),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let saved = stored(env.user_id).await.expect("应写入有效期");
    assert!(
        (saved - future).num_seconds().abs() <= 1,
        "落库时间应与请求一致：{saved} vs {future}"
    );
    let swept = okapi::worker::expire_balances(&env.pg, &env.ledger, chrono::Utc::now())
        .await
        .unwrap();
    assert!(
        !swept.iter().any(|e| e.user_id == env.user_id),
        "未到期不该被扫走"
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        7_000
    );

    // 改成已过期：同一轮扫描就清零、事件留痕、到期时间重置防重扫
    let past = chrono::Utc::now() - chrono::Duration::minutes(1);
    let (status, body) = set_expiry(
        &env.super_token,
        env.user_id,
        serde_json::json!(past.to_rfc3339()),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let swept = okapi::worker::expire_balances(&env.pg, &env.ledger, chrono::Utc::now())
        .await
        .unwrap();
    let mine: Vec<_> = swept.iter().filter(|e| e.user_id == env.user_id).collect();
    assert_eq!(mine.len(), 1, "到期应恰好清一次");
    assert_eq!(mine[0].drained_micro, 7_000);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        0
    );
    assert!(stored(env.user_id).await.is_none(), "清完要重置防重扫");

    // 传 null = 取消有效期（此时本就是 NULL，验的是不报错也不写坏）
    let (status, body) = set_expiry(&env.super_token, env.user_id, Value::Null).await;
    assert_eq!(status, 200, "{body}");
    assert!(stored(env.user_id).await.is_none());

    // 不存在的用户：404（不是静默 200）
    let (status, _) = set_expiry(&env.super_token, 9_999_999_999, Value::Null).await;
    assert_eq!(status, 404);

    // 三次成功调用都留痕，detail 里带下发的时间
    let audited = sqlx::query!(
        r#"SELECT detail FROM audit_logs WHERE action = 'user.balance_expiry' AND target = $1
           ORDER BY created_at, id"#,
        env.user_id.to_string()
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(audited.len(), 3, "设两次 + 清一次");
    assert!(
        audited[2].detail.clone().unwrap_or(Value::Null)["expires_at"].is_null(),
        "取消那次 detail 应为 null"
    );
}
