//! 用户列表与角色管理端点验收：补齐前管理端只能按 ID 操作、没有列表与角色下拉。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::Value;
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

/// 本文件的用例串行执行。
///
/// `auth_flush()` 是**全局**清空：并行用例里任何一次清空（改角色、改分组、改倍率……）都会
/// 顺带冲掉别的用例刚写进缓存的快照，于是"改完没刷缓存"这类回归会被掩盖——守护它的用例
/// 随调度时序时灵时不灵。变异测试实测：删掉 `assign_role` 里的 `auth_flush()`，默认并行跑
/// SURVIVED，单独跑、单线程跑都 CAUGHT。每个用例开头先拿这把锁。
static SERIAL: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

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
    let _serial = SERIAL.lock().await;
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
    let _serial = SERIAL.lock().await;
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
    let _serial = SERIAL.lock().await;
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
    let _serial = SERIAL.lock().await;
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
    let _serial = SERIAL.lock().await;
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

/// 建一个"只绑了某个自定义角色"的新管理员，返回 (user_id, token)。
///
/// 绑定发生在这个 token **第一次被使用之前**，鉴权缓存里还没有它的旧快照——
/// 于是用它测到的只是权限本身，不会和"改完是否刷缓存"那条规则纠缠在一起。
async fn scoped_admin(env: &Env, tag: &str, permissions: &[&str]) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let uid = okapi_store::provision::create_user(&env.pg, &format!("sa-{tag}-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-sa-{tag}-{suffix}");
    okapi_store::provision::create_api_key(&env.pg, uid, &hash(&token), "sk-sa")
        .await
        .unwrap();
    let role = mk_role(env, &format!("sa_{tag}_{}", &suffix[..8]), permissions).await;
    bind_role(env, uid, 10, Some(role)).await;
    (uid, token)
}

/// 自定义管理员只能做它权限点里写明的写操作。
///
/// RBAC 横切扫描（`route_error_envelope`）只证明"完全没有管理权限的用户会被拒"，
/// 它发现不了**用错了权限点**：变异测试把 `set_user_groups` 的 `user.manage` 换成
/// `user.read`、把 `set_user_multiplier` 的 `pricing.write` 换成 `user.manage`，
/// 全量都没有一个用例变红——前者等于只读权限能执行写操作，后者等于只有用户管理权的人
/// 能改别人的价目表（倍率是计价链上的一个乘数）。每条都带正向对照，确保 403 来自权限而不是别的。
#[tokio::test]
async fn scoped_admins_cannot_write_beyond_their_permissions() {
    let _serial = SERIAL.lock().await;
    let env = setup().await;
    let target = env.user_id;
    let groups_path = format!("/admin/users/{target}/groups");
    let mult_path = format!("/admin/users/{target}/multiplier");
    let empty_groups = serde_json::json!({ "groups": [] });
    let mult = serde_json::json!({ "multiplier": "1" });

    // 只读：能看、不能改分组
    let (_, reader) = scoped_admin(&env, "read", &["user.read"]).await;
    let (status, _) = req(&env, "GET", "/admin/users", &reader, None).await;
    assert_eq!(
        status, 200,
        "user.read 应能列用户（确认这把 token 确实是管理员）"
    );
    let (status, body) = req(
        &env,
        "POST",
        &groups_path,
        &reader,
        Some(empty_groups.clone()),
    )
    .await;
    assert_eq!(status, 403, "只有 user.read 却改动了用户分组：{body}");

    // 用户管理：能改分组、不能改倍率
    let (_, manager) = scoped_admin(&env, "manage", &["user.manage"]).await;
    let (status, body) = req(&env, "POST", &groups_path, &manager, Some(empty_groups)).await;
    assert_eq!(status, 200, "user.manage 应能改分组（正向对照）：{body}");
    let (status, body) = req(&env, "POST", &mult_path, &manager, Some(mult.clone())).await;
    assert_eq!(status, 403, "只有 user.manage 却改动了计价倍率：{body}");

    // 计价写：能改倍率（正向对照）
    let (_, pricer) = scoped_admin(&env, "pricing", &["pricing.write"]).await;
    let (status, body) = req(&env, "POST", &mult_path, &pricer, Some(mult)).await;
    assert_eq!(status, 200, "pricing.write 应能改倍率（正向对照）：{body}");
}

/// 降权必须立刻生效，不能等鉴权缓存过期。
///
/// `assign_role` 写完库后调 `auth_flush()`；变异测试把这一行删掉，全量没有一个用例变红。
/// 后果是安全上的：超管把一个可疑管理员降为普通用户，对方在缓存过期前仍然握着旧权限。
/// 这里先用对方的 token 访问一次管理面，**让它的管理员快照进缓存**，再降权、立刻重访。
#[tokio::test]
async fn demotion_takes_effect_without_waiting_for_the_auth_cache() {
    let _serial = SERIAL.lock().await;
    let env = setup().await;
    let (uid, token) = scoped_admin(&env, "demote", &["user.read"]).await;

    let (status, _) = req(&env, "GET", "/admin/users", &token, None).await;
    assert_eq!(
        status, 200,
        "降权前应能访问（这一次顺带把管理员快照写进缓存）"
    );

    bind_role(&env, uid, 1, None).await;
    let (status, body) = req(&env, "GET", "/admin/users", &token, None).await;
    assert_eq!(
        status, 403,
        "降为普通用户后仍能访问管理面——鉴权缓存没有随改角色刷新：{body}"
    );
}

/// 角色只能是 1（用户）/ 10（管理员）/ 100（超管）。
///
/// 变异测试删掉取值白名单，全量无一变红。后端的角色判断全是阈值式（`>= 100` / `>= 10`），
/// 越界值会落进最近的档位（999 等同超管、50 等同管理员）——只有超管能改角色，所以这不是提权；
/// 白名单守的是"库里的角色值始终是文档约定的三个之一"，按精确值展示或统计的地方才不会冒出未知角色。
#[tokio::test]
async fn role_outside_the_whitelist_is_rejected() {
    let _serial = SERIAL.lock().await;
    let env = setup().await;
    let path = format!("/admin/users/{}/role", env.user_id);
    for bad in [0, 2, 50, 99, 101, 999, -1] {
        let (status, body) = req(
            &env,
            "POST",
            &path,
            &env.super_token,
            Some(serde_json::json!({ "role": bad })),
        )
        .await;
        assert_eq!(status, 400, "role={bad} 应被拒：{body}");
        assert_eq!(
            body["error"]["param"], "role",
            "role={bad} 的错误应指向 role 字段：{body}"
        );
    }
    let stored = sqlx::query_scalar!(
        // 与 manage.rs 那句逐字相同，复用 .sqlx 里已有的离线缓存（CI 以 SQLX_OFFLINE=true 编译）
        r#"SELECT role FROM users WHERE id = $1 AND deleted_at IS NULL"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(stored, 1, "被拒的写入不得落库");

    // 正向对照：白名单内的值照常生效
    bind_role(&env, env.user_id, 10, None).await;
}
