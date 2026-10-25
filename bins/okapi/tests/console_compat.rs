//! 适配规则管理接口验收（docs/compat-rules.md §9、§12）：保存即发布的版本、乐观并发、
//! 受保护 / 敏感 / 计费关键路径的写入门槛、厂商目录与按主机的建议（只建议）、全局护栏、试跑，
//! 以及创建 / 修改渠道时随体带规则。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

struct Env {
    pg: PgPool,
    console: SocketAddr,
    admin: String,
    /// 只有 channel.read / channel.write 的管理员（自定义角色，没有 compat.* 权限点）。
    limited: String,
    limited_id: i64,
    channel: i64,
}

async fn user(pg: &PgPool, role: i16, admin_role_id: Option<i64>) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let id = okapi_store::provision::create_user(pg, &format!("cc-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = $2, admin_role_id = $3 WHERE id = $1")
        .bind(id)
        .bind(role)
        .bind(admin_role_id)
        .execute(pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-cc-{suffix}");
    okapi_store::provision::create_api_key(pg, id, &hash(&token), "sk-cc")
        .await
        .unwrap();
    (id, token)
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('ssrf_policy', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(json!({"allow_http": true, "allow_private": true}))
    .execute(&pg)
    .await
    .unwrap();
    // 全局护栏每个用例都会改：测试库在套件内共用，开头先清掉上一个用例留下的
    sqlx::query("DELETE FROM rule_sets WHERE scope_kind = 'global'")
        .execute(&pg)
        .await
        .unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let console = serve(console::router(state)).await;

    let (_, admin) = user(&pg, 100, None).await;
    let code = format!("compat-limited-{}", Uuid::new_v4().simple());
    let created: Value = reqwest::Client::new()
        .post(format!("http://{console}/admin/roles"))
        .bearer_auth(&admin)
        .json(&json!({"role_code": code, "display_name": "只管渠道",
                      "permissions": ["channel.read", "channel.write"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let role_id = created["admin_role_id"].as_i64().expect("role id");
    let (limited_id, limited) = user(&pg, 10, Some(role_id)).await;

    let suffix = Uuid::new_v4().simple().to_string();
    let (channel, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("cc-ch-{suffix}"),
        "openai_compat",
        "https://api.deepseek.com/v1",
        "mock-credential",
        &[format!("m-cc-{suffix}").as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    Env {
        pg,
        console,
        admin,
        limited,
        limited_id,
        channel,
    }
}

async fn send(
    env: &Env,
    method: reqwest::Method,
    token: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = reqwest::Client::new()
        .request(method, format!("http://{}{path}", env.console))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

fn compat_path(env: &Env) -> String {
    format!("/admin/channels/{}/compat", env.channel)
}

fn strip_user() -> Value {
    json!([{"id": "strip-request-fields", "do": [{"op": "remove", "paths": ["/user"]}]}])
}

#[tokio::test]
async fn saving_publishes_versions_and_stale_bases_conflict() {
    let env = setup().await;
    let path = compat_path(&env);
    let (status, body) = send(&env, reqwest::Method::GET, &env.admin, &path, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["channel"], Value::Null);
    assert_eq!(body["effective"]["rules"], json!([]));

    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"rules": strip_user(), "base_version": 0, "note": "first"})),
    )
    .await;
    assert_eq!((status, &body["version"]), (200, &json!(1)), "{body}");
    let (status, body) = send(&env, reqwest::Method::GET, &env.admin, &path, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["channel"]["version"], 1);
    assert_eq!(body["channel"]["note"], "first");
    assert_eq!(body["effective"]["rules"][0]["layer"], "channel");
    assert_eq!(
        body["effective"]["rules"][0]["rule"]["id"],
        "strip-request-fields"
    );

    let two = json!([{"id": "strip-request-fields", "do": [{"op": "remove", "paths": ["/user", "/metadata"]}]}]);
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"rules": two, "base_version": 1})),
    )
    .await;
    assert_eq!((status, &body["version"]), (200, &json!(2)), "{body}");
    // 另一个管理员还拿着版本 1：不能盖掉版本 2
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"rules": strip_user(), "base_version": 1})),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "rules_version_conflict");
    // 同一个请求里带的厂商改动，不能在版本冲突时偷偷落库
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"vendor": "deepseek", "rules": strip_user(), "base_version": 1})),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    let vendor: Option<String> = sqlx::query_scalar("SELECT vendor FROM channels WHERE id = $1")
        .bind(env.channel)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(vendor, None, "冲突发生在写任何东西之前");
    // 备注过长同样在写任何东西之前被拒
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"vendor": "deepseek", "rules": strip_user(), "note": "x".repeat(300)})),
    )
    .await;
    assert_eq!(
        (status, &body["error"]["param"]),
        (400, &json!("note")),
        "{body}"
    );
    let vendor: Option<String> = sqlx::query_scalar("SELECT vendor FROM channels WHERE id = $1")
        .bind(env.channel)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(vendor, None);

    let history: Vec<(i32, String)> = sqlx::query_as(
        "SELECT version, status FROM rule_sets WHERE scope_id = $1 AND scope_kind = 'channel' ORDER BY version",
    )
    .bind(env.channel)
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        history,
        [(1, "archived".to_owned()), (2, "published".to_owned())]
    );
}

#[tokio::test]
async fn invalid_rules_are_rejected_with_the_first_problem_in_param() {
    let env = setup().await;
    let path = compat_path(&env);
    let put = |rules: Value| {
        let env = &env;
        let path = path.clone();
        async move {
            send(
                env,
                reqwest::Method::PUT,
                &env.admin,
                &path,
                Some(json!({"rules": rules})),
            )
            .await
        }
    };
    for (rules, param) in [
        (
            json!([{"id": "pin", "do": [{"op": "set", "path": "/model", "value": "x"}]}]),
            "pin:protected_path",
        ),
        (
            json!([{"id": "gone", "do": [{"op": "remove", "paths": ["/messages"]}]}]),
            "gone:protected_path",
        ),
        (
            json!([{"id": "u", "do": [{"op": "teleport"}]}]),
            "u:unknown_op",
        ),
        (
            json!([{"id": "c", "if": "true", "do": [{"op": "remove", "paths": ["/x"]}]}]),
            "c:cel_not_enabled",
        ),
        (
            json!([{"id": "s", "do": [{"op": "remove", "paths": ["/stream_options"]}]}]),
            "s:billing_critical_ack_required",
        ),
        (
            json!([{"id": "r", "phase": "response", "do": [{"op": "remove", "paths": ["/x"]}]}]),
            "r:phase_not_enabled",
        ),
        (json!({"not": "an array"}), "not_array"),
    ] {
        let (status, body) = put(rules.clone()).await;
        assert_eq!(status, 400, "{rules} → {body}");
        assert_eq!(body["error"]["code"], "compat_rules_invalid", "{rules}");
        assert_eq!(body["error"]["param"], param, "{rules}");
    }
    // 计费关键路径带确认就放行（管理员明知故犯，留审计）
    let (status, body) = put(json!([{
        "id": "s", "billing_critical_ack": true,
        "do": [{"op": "remove", "paths": ["/stream_options"]}]
    }]))
    .await;
    assert_eq!(status, 200, "{body}");
    let published: Option<i32> = sqlx::query_scalar(
        "SELECT MAX(version) FROM rule_sets WHERE scope_id = $1 AND scope_kind = 'channel'",
    )
    .bind(env.channel)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(published, Some(1), "被拒绝的请求不留下任何版本");
}

#[tokio::test]
async fn the_vendor_is_only_suggested_until_an_admin_chooses_it() {
    let env = setup().await;
    let path = compat_path(&env);
    let (_, body) = send(&env, reqwest::Method::GET, &env.admin, &path, None).await;
    assert_eq!(body["vendor"], Value::Null);
    assert_eq!(
        body["suggested_vendor"]["id"], "builtin:deepseek",
        "地址主机命中厂商目录"
    );
    assert_eq!(
        body["effective"]["layers"],
        json!([]),
        "只建议：没选就不套厂商层"
    );

    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"vendor": "deepseek"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (_, body) = send(&env, reqwest::Method::GET, &env.admin, &path, None).await;
    assert_eq!(body["vendor"], "builtin:deepseek", "裸 id 落库时带命名空间");
    assert_eq!(body["suggested_vendor"], Value::Null);
    assert_eq!(body["effective"]["layers"][0]["layer"], "vendor");
    assert_eq!(body["effective"]["rules"][0]["layer"], "vendor");

    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"vendor": "no-such-vendor"})),
    )
    .await;
    assert_eq!(
        (status, &body["error"]["param"]),
        (400, &json!("vendor")),
        "{body}"
    );
    let (status, _) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"vendor": null})),
    )
    .await;
    assert_eq!(status, 200);
    let (_, body) = send(&env, reqwest::Method::GET, &env.admin, &path, None).await;
    assert_eq!(body["vendor"], Value::Null);
    assert!(
        body["effective"]["rules"].as_array().unwrap().is_empty(),
        "{body}"
    );
}

#[tokio::test]
async fn writing_prompt_like_paths_needs_the_sensitive_permission_but_removing_does_not() {
    let env = setup().await;
    let path = compat_path(&env);
    let tier =
        json!([{"id": "tier", "do": [{"op": "set", "path": "/service_tier", "value": "flex"}]}]);
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.limited,
        &path,
        Some(json!({"rules": tier})),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["param"], "compat.sensitive");

    // 只是删字段：任何能写渠道的人都行（旧的 strip 设置就是这样）
    let remove = json!([{"id": "no-tier", "do": [{"op": "remove", "paths": ["/service_tier", "/instructions"]}]}]);
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.limited,
        &path,
        Some(json!({"rules": remove})),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    // 有权限的管理员写入敏感规则后，没有权限的人原样再保存（改别的字段）不受阻
    let (status, _) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        &path,
        Some(json!({"rules": tier})),
    )
    .await;
    assert_eq!(status, 200);
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.limited,
        &path,
        Some(json!({"rules": tier, "note": "just a note"})),
    )
    .await;
    assert_eq!(status, 200, "未新增敏感写入：{body}");
}

#[tokio::test]
async fn the_guard_needs_its_own_permission_and_reaches_every_channel() {
    let env = setup().await;
    let guard = json!([{"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}]);
    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.limited,
        "/admin/compat/guard",
        Some(json!({"rules": guard})),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["param"], "compat.guard.write");

    let (status, body) = send(
        &env,
        reqwest::Method::PUT,
        &env.admin,
        "/admin/compat/guard",
        Some(json!({"rules": guard})),
    )
    .await;
    assert_eq!((status, &body["version"]), (200, &json!(1)), "{body}");
    let (status, body) = send(
        &env,
        reqwest::Method::GET,
        &env.limited,
        "/admin/compat/guard",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["guard"]["version"], 1);

    // 一个什么都没配的渠道，有效规则里也看得到护栏
    let (_, body) = send(
        &env,
        reqwest::Method::GET,
        &env.limited,
        &compat_path(&env),
        None,
    )
    .await;
    assert_eq!(body["effective"]["rules"][0]["layer"], "guard");
    assert_eq!(body["effective"]["layers"][0]["label"], "guard@v1");
    sqlx::query("DELETE FROM rule_sets WHERE scope_kind = 'global'")
        .execute(&env.pg)
        .await
        .unwrap();
}

#[tokio::test]
async fn dry_run_shows_the_difference_and_saves_nothing() {
    let env = setup().await;
    let dry = |body: Value| {
        let env = &env;
        async move {
            send(
                env,
                reqwest::Method::POST,
                &env.limited,
                "/admin/compat/dry-run",
                Some(body),
            )
            .await
        }
    };
    let (status, body) = dry(json!({
        "channel_id": env.channel,
        "vendor": "deepseek",
        "rules": strip_user(),
        "context": {"model": "deepseek-chat"},
        "request": {"model": "deepseek-chat", "user": "u", "messages": [{"role": "developer", "content": "x"}]}
    }))
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["valid"], true);
    assert_eq!(body["changed"], true);
    assert_eq!(
        body["after"],
        json!({"model": "deepseek-chat", "messages": [{"role": "system", "content": "x"}]})
    );
    let fired: Vec<&str> = body["fired"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert_eq!(
        fired,
        ["strict-role.developer-to-system", "strip-request-fields"]
    );

    let (status, body) = dry(json!({
        "rules": [{"id": "pin", "do": [{"op": "set", "path": "/model", "value": "x"}]}],
        "context": {"model": "m"},
        "request": {"model": "m"}
    }))
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["valid"], false);
    assert_eq!(body["issues"][0]["code"], "protected_path");

    let saved: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rule_sets WHERE scope_kind = 'channel' AND scope_id = $1",
    )
    .bind(env.channel)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(saved, 0, "试跑不落库");
    let (status, _) = send(
        &env,
        reqwest::Method::POST,
        "not-a-token",
        "/admin/compat/dry-run",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn the_vendor_catalog_lists_builtins_and_suggests_by_host() {
    let env = setup().await;
    let (status, body) = send(
        &env,
        reqwest::Method::GET,
        &env.limited,
        "/admin/compat/vendors?host=api.moonshot.cn",
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["suggestion"], "builtin:moonshot");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"builtin:deepseek") && ids.contains(&"builtin:xai"),
        "{ids:?}"
    );
}

#[tokio::test]
async fn creating_and_patching_a_channel_carry_the_vendor_and_rules() {
    let env = setup().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let (status, body) = send(
        &env,
        reqwest::Method::POST,
        &env.admin,
        "/admin/channels",
        Some(json!({
            "name": format!("cc-new-{suffix}"), "api_base": "http://127.0.0.1:9/v1",
            "credential": "c", "models": [format!("m-{suffix}")],
            "vendor": "builtin:moonshot",
            "compat": {"rules": strip_user(), "note": "at creation"}
        })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let id = body["channel_id"].as_i64().unwrap();
    let (_, view) = send(
        &env,
        reqwest::Method::GET,
        &env.admin,
        &format!("/admin/channels/{id}/compat"),
        None,
    )
    .await;
    assert_eq!(view["vendor"], "builtin:moonshot");
    assert_eq!(view["channel"]["version"], 1);
    assert_eq!(view["channel"]["note"], "at creation");

    // 修改：规则 + 厂商 + 其它字段一个请求
    let (status, body) = send(
        &env,
        reqwest::Method::PATCH,
        &env.admin,
        &format!("/admin/channels/{id}"),
        Some(json!({
            "name": format!("cc-new2-{suffix}"), "vendor": null,
            "compat": {"rules": [], "base_version": 1}
        })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (_, view) = send(
        &env,
        reqwest::Method::GET,
        &env.admin,
        &format!("/admin/channels/{id}/compat"),
        None,
    )
    .await;
    assert_eq!(view["vendor"], Value::Null);
    assert_eq!(view["channel"]["version"], 2);
    assert_eq!(view["channel"]["rules"], json!([]));
    // 版本冲突发生在写任何东西之前：改名没有落库
    let (status, body) = send(
        &env,
        reqwest::Method::PATCH,
        &env.admin,
        &format!("/admin/channels/{id}"),
        Some(json!({"name": "should-not-stick", "compat": {"rules": strip_user(), "base_version": 1}})),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    let name: String = sqlx::query_scalar("SELECT name FROM channels WHERE id = $1")
        .bind(id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert!(name.starts_with("cc-new2-"), "{name}");
}

#[tokio::test]
async fn owners_only_reach_their_own_channels_compat() {
    let env = setup().await;
    let created: Value = reqwest::Client::new()
        .post(format!("http://{}/admin/roles", env.console))
        .bearer_auth(&env.admin)
        .json(
            &json!({"role_code": format!("own-{}", Uuid::new_v4().simple()),
                      "display_name": "自管渠道",
                      "permissions": ["channel.read.own", "channel.write.own"]}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let role_id = created["admin_role_id"].as_i64().unwrap();
    let (_, owner) = user(&env.pg, 10, Some(role_id)).await;
    // env.channel 属主是别人（无属主）：own 范围碰不得
    let (status, _) = send(&env, reqwest::Method::GET, &owner, &compat_path(&env), None).await;
    assert_eq!(status, 403);
    let (status, _) = send(
        &env,
        reqwest::Method::PUT,
        &owner,
        &compat_path(&env),
        Some(json!({"rules": strip_user()})),
    )
    .await;
    assert_eq!(status, 403);
    let _ = env.limited_id;
}
