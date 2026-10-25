//! 适配规则 P2 管理接口验收（docs/compat-rules.md §17）：自描述目录、适配档案（增删改、被渠道引用时不能删）、
//! 渠道选档案、草稿 → 批准发布（权限、过期检测、敏感门槛）、版本历史与回滚、四个阶段的试跑、按请求 id 解释命中。
//! 依赖 .env（scripts/dev-deps.sh up）。

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
    /// super admin：有全部权限点。
    admin: String,
    admin_id: i64,
    /// 只有 channel.read / channel.write。
    limited: String,
    /// channel.read + compat.profile.write（能建档案，不能发布）。
    profiler: String,
    /// channel.read / channel.write + compat.publish（能批准草稿，没有 compat.sensitive）。
    publisher: String,
    channel: i64,
}

async fn user(pg: &PgPool, role: i16, admin_role_id: Option<i64>) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let id = okapi_store::provision::create_user(pg, &format!("cp2-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = $2, admin_role_id = $3 WHERE id = $1")
        .bind(id)
        .bind(role)
        .bind(admin_role_id)
        .execute(pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-cp2-{suffix}");
    okapi_store::provision::create_api_key(pg, id, &hash(&token), "sk-cp2")
        .await
        .unwrap();
    (id, token)
}

async fn role_user(
    env_console: SocketAddr,
    pg: &PgPool,
    admin: &str,
    permissions: &[&str],
) -> String {
    let code = format!("cp2-{}", Uuid::new_v4().simple());
    let created: Value = reqwest::Client::new()
        .post(format!("http://{env_console}/admin/roles"))
        .bearer_auth(admin)
        .json(&json!({"role_code": code, "display_name": code, "permissions": permissions}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let role_id = created["admin_role_id"].as_i64().expect("role id");
    user(pg, 10, Some(role_id)).await.1
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
    // 档案 slug 与全局护栏在套件内共用：开头先清掉上一个用例留下的
    sqlx::query("DELETE FROM rule_sets WHERE scope_kind IN ('global', 'profile')")
        .execute(&pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET vendor = NULL WHERE vendor LIKE 'custom:%'")
        .execute(&pg)
        .await
        .unwrap();
    sqlx::query("DELETE FROM compat_profiles")
        .execute(&pg)
        .await
        .unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let console = serve(console::router(state)).await;

    let (admin_id, admin) = user(&pg, 100, None).await;
    let limited = role_user(console, &pg, &admin, &["channel.read", "channel.write"]).await;
    let profiler = role_user(
        console,
        &pg,
        &admin,
        &["channel.read", "compat.profile.write"],
    )
    .await;
    let publisher = role_user(
        console,
        &pg,
        &admin,
        &[
            "channel.read",
            "channel.write",
            "compat.publish",
            "compat.profile.write",
        ],
    )
    .await;

    let suffix = Uuid::new_v4().simple().to_string();
    let (channel, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("cp2-ch-{suffix}"),
        "openai_compat",
        "https://relay.example.com/v1",
        "mock-credential",
        &[format!("m-cp2-{suffix}").as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    Env {
        pg,
        console,
        admin,
        admin_id,
        limited,
        profiler,
        publisher,
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

async fn get(env: &Env, token: &str, path: &str) -> (u16, Value) {
    send(env, reqwest::Method::GET, token, path, None).await
}

async fn put(env: &Env, token: &str, path: &str, body: Value) -> (u16, Value) {
    send(env, reqwest::Method::PUT, token, path, Some(body)).await
}

async fn post(env: &Env, token: &str, path: &str, body: Value) -> (u16, Value) {
    send(env, reqwest::Method::POST, token, path, Some(body)).await
}

async fn delete(env: &Env, token: &str, path: &str) -> (u16, Value) {
    send(env, reqwest::Method::DELETE, token, path, None).await
}

fn set_user(value: &str) -> Value {
    json!([{"id": "u", "do": [{"op": "set", "path": "/user", "value": value}]}])
}

fn channel_set(env: &Env, tail: &str) -> String {
    format!("/admin/compat/sets/channel/{}{tail}", env.channel)
}

// ------------------------------------------------------------------------------------ catalog

#[tokio::test]
async fn the_catalog_describes_the_engine_and_needs_a_login() {
    let env = setup().await;
    let (status, _) = send(
        &env,
        reqwest::Method::GET,
        "bad-token",
        "/admin/compat/catalog",
        None,
    )
    .await;
    assert_eq!(status, 401);
    let (status, body) = get(&env, &env.limited, "/admin/compat/catalog").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["ops"].as_array().unwrap().len(),
        okapi_compat::OP_NAMES.len()
    );
    assert_eq!(body["phases"].as_array().unwrap().len(), 6);
    assert!(
        body["usage_slots"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "completion")
    );
    assert!(body["recipes"].as_array().unwrap().len() >= 8);
    assert_eq!(
        body["selector"]["response_dialects"],
        json!(["openai.chat"])
    );
}

// ------------------------------------------------------------------------------------ profiles

#[tokio::test]
// 端到端场景脚本：建 / 校验 / 列 / 改 / 删按时序推进，拆开会丢掉前一步留下的状态
#[allow(clippy::too_many_lines)]
async fn profiles_are_created_validated_listed_patched_and_deleted() {
    let env = setup().await;
    let body = json!({"slug": "acme", "name": "Acme Relay"});
    let (status, _) = post(&env, &env.limited, "/admin/compat/profiles", body.clone()).await;
    assert_eq!(status, 403, "没有 compat.profile.write");

    for (bad, param) in [
        (json!({"slug": "Acme Relay", "name": "x"}), "slug"),
        (json!({"slug": "-lead", "name": "x"}), "slug"),
        (json!({"slug": "ok", "name": "   "}), "name"),
        (
            json!({"slug": "ok", "name": "x", "extends": "no-such-vendor"}),
            "extends",
        ),
        (
            json!({"slug": "ok", "name": "x", "hosts": ["https://relay.example.com/x"]}),
            "hosts",
        ),
        (
            json!({"slug": "ok", "name": "x", "description": "a\nb"}),
            "description",
        ),
    ] {
        let (status, got) = post(&env, &env.profiler, "/admin/compat/profiles", bad.clone()).await;
        assert_eq!(status, 400, "{bad}");
        assert_eq!(got["error"]["param"], param, "{bad}");
    }

    let (status, created) = post(
        &env,
        &env.profiler,
        "/admin/compat/profiles",
        json!({"slug": "ACME", "name": "Acme Relay", "description": "中转商", "extends": "deepseek",
               "hosts": ["Relay.Example.com", "relay.example.com"]}),
    )
    .await;
    assert_eq!(status, 200, "{created}");
    assert_eq!(created["slug"], "acme", "slug 落成小写");
    assert_eq!(created["extends"], "builtin:deepseek");
    assert_eq!(
        created["hosts"],
        json!(["relay.example.com"]),
        "主机小写去重"
    );
    assert_eq!(created["vendor"], "custom:acme");
    let id = created["id"].as_i64().unwrap();

    let (status, dup) = post(&env, &env.profiler, "/admin/compat/profiles", body).await;
    assert_eq!(status, 409, "{dup}");
    assert_eq!(dup["error"]["code"], "profile_slug_taken");

    // 列表任何能看渠道的人都能读，带线上版本 / 草稿 / 在用渠道数
    let (status, listed) = get(&env, &env.limited, "/admin/compat/profiles").await;
    assert_eq!(status, 200);
    let row = &listed["data"][0];
    assert_eq!(
        (
            row["slug"].as_str(),
            row["channels"].as_i64(),
            row["has_draft"].as_bool()
        ),
        (Some("acme"), Some(0), Some(false))
    );
    assert!(row["version"].is_null());

    // 改：名称、清说明、换厂商、换主机
    let (status, patched) = send(
        &env,
        reqwest::Method::PATCH,
        &env.profiler,
        &format!("/admin/compat/profiles/{id}"),
        Some(json!({"name": "Acme", "description": null, "extends": "moonshot", "hosts": []})),
    )
    .await;
    assert_eq!(status, 200, "{patched}");
    assert_eq!(patched["name"], "Acme");
    assert!(patched["description"].is_null());
    assert_eq!(patched["extends"], "builtin:moonshot");
    assert_eq!(patched["hosts"], json!([]));

    // 档案的规则：直接发布要 compat.profile.write + compat.publish（profiler 没有后者）
    let rules_path = format!("/admin/compat/sets/profile/{id}/rules");
    let (status, _) = put(
        &env,
        &env.profiler,
        &rules_path,
        json!({"rules": set_user("p")}),
    )
    .await;
    assert_eq!(status, 403);
    let (status, published) = put(
        &env,
        &env.publisher,
        &rules_path,
        json!({"rules": set_user("p"), "base_version": 0}),
    )
    .await;
    assert_eq!(status, 200, "{published}");
    assert_eq!(published["version"], 1);

    // 详情带有效规则预览：叠加的厂商层 + 档案层
    let (status, detail) = get(&env, &env.limited, &format!("/admin/compat/profiles/{id}")).await;
    assert_eq!(status, 200, "{detail}");
    let labels: Vec<&str> = detail["effective"]["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["builtin:moonshot", "custom:acme@v1"]);
    assert_eq!(detail["published"]["version"], 1);

    // 被渠道引用时不能删；解绑后可以，规则集一并清掉
    sqlx::query("UPDATE channels SET vendor = 'custom:acme' WHERE id = $1")
        .bind(env.channel)
        .execute(&env.pg)
        .await
        .unwrap();
    let (status, listed) = get(&env, &env.limited, "/admin/compat/profiles").await;
    assert_eq!(status, 200);
    assert_eq!(listed["data"][0]["channels"], 1);
    let (status, blocked) =
        delete(&env, &env.profiler, &format!("/admin/compat/profiles/{id}")).await;
    assert_eq!(status, 409, "{blocked}");
    assert_eq!(blocked["error"]["code"], "profile_in_use");
    sqlx::query("UPDATE channels SET vendor = NULL WHERE id = $1")
        .bind(env.channel)
        .execute(&env.pg)
        .await
        .unwrap();
    let (status, _) = delete(&env, &env.profiler, &format!("/admin/compat/profiles/{id}")).await;
    assert_eq!(status, 200);
    let (status, _) = get(&env, &env.limited, &format!("/admin/compat/profiles/{id}")).await;
    assert_eq!(status, 404);
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rule_sets WHERE scope_kind = 'profile' AND scope_id = $1",
    )
    .bind(id)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_channel_can_choose_a_profile_and_unknown_profiles_are_refused() {
    let env = setup().await;
    let (_, created) = post(
        &env,
        &env.profiler,
        "/admin/compat/profiles",
        json!({"slug": "acme", "name": "Acme", "hosts": ["relay.example.com"]}),
    )
    .await;
    assert!(created["id"].is_i64());
    let path = format!("/admin/channels/{}/compat", env.channel);

    let (status, got) = put(&env, &env.limited, &path, json!({"vendor": "custom:nope"})).await;
    assert_eq!(status, 400, "{got}");
    assert_eq!(got["error"]["param"], "vendor");

    // 没选厂商时按地址主机建议档案（只建议）
    let (_, view) = get(&env, &env.limited, &path).await;
    assert!(view["vendor"].is_null());
    assert_eq!(view["suggested_vendor"]["id"], "custom:acme");
    assert_eq!(view["suggested_vendor"]["kind"], "profile");

    let (status, got) = put(&env, &env.limited, &path, json!({"vendor": "custom:acme"})).await;
    assert_eq!(status, 200, "{got}");
    let (_, view) = get(&env, &env.limited, &path).await;
    assert_eq!(view["vendor"], "custom:acme");
    assert_eq!(view["profile"]["slug"], "acme");
    // PATCH 渠道本身也认档案
    let (status, got) = send(
        &env,
        reqwest::Method::PATCH,
        &env.limited,
        &format!("/admin/channels/{}", env.channel),
        Some(json!({"vendor": null})),
    )
    .await;
    assert_eq!(status, 200, "{got}");
    let (status, got) = send(
        &env,
        reqwest::Method::PATCH,
        &env.limited,
        &format!("/admin/channels/{}", env.channel),
        Some(json!({"vendor": "custom:acme"})),
    )
    .await;
    assert_eq!(status, 200, "{got}");

    // 厂商目录也列出档案
    let (_, vendors) = get(
        &env,
        &env.limited,
        "/admin/compat/vendors?host=relay.example.com",
    )
    .await;
    assert_eq!(vendors["profiles"][0]["id"], "custom:acme");
    assert_eq!(vendors["suggestion"]["id"], "custom:acme");
    assert!(
        vendors["data"][0]["endpoints"]
            .as_array()
            .is_some_and(|e| !e.is_empty())
    );
}

// ------------------------------------------------------------------------------------- drafts

/// 创建兼容渠道时选了档案：地址由档案叠加的内置厂商的端点表补全；档案没叠加厂商就补不出来，照样拒绝。
#[tokio::test]
async fn creating_a_channel_with_a_profile_fills_the_address_from_the_vendor_it_extends() {
    let env = setup().await;
    let slug_for = |tag: &str| format!("create-{tag}-{}", Uuid::new_v4().simple());
    let (with_vendor, bare) = (slug_for("ds"), slug_for("bare"));
    for (slug, extends) in [(&with_vendor, Some("deepseek")), (&bare, None)] {
        let (status, body) = post(
            &env,
            &env.profiler,
            "/admin/compat/profiles",
            json!({"slug": slug, "name": "create test", "extends": extends}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }
    let create = |vendor: String| {
        post(
            &env,
            &env.admin,
            "/admin/channels",
            json!({
                "name": format!("cp2-create-{}", Uuid::new_v4().simple()), "provider": "openai_compat",
                "api_base": "", "credential": "k", "models": ["m-create"], "vendor": vendor,
            }),
        )
    };
    let (status, body) = create(format!("custom:{with_vendor}")).await;
    assert_eq!(status, 200, "{body}");
    let (stored_base, stored_vendor): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT api_base, vendor FROM channels WHERE id = $1")
            .bind(body["channel_id"].as_i64().unwrap())
            .fetch_one(&env.pg)
            .await
            .unwrap();
    assert_eq!(stored_base.as_deref(), Some("https://api.deepseek.com/v1"));
    assert_eq!(stored_vendor, Some(format!("custom:{with_vendor}")));

    let (status, body) = create(format!("custom:{bare}")).await;
    assert_eq!(status, 400, "没叠加厂商的档案补不出地址：{body}");
    assert_eq!(body["error"]["param"], "api_base");
}

#[tokio::test]
async fn drafts_are_saved_by_writers_and_published_by_someone_with_compat_publish() {
    let env = setup().await;
    let draft = channel_set(&env, "/draft");
    let (status, saved) = put(
        &env,
        &env.limited,
        &draft,
        json!({"rules": set_user("proposal"), "note": "试一下", "rationale": "上游 400 了 user 字段"}),
    )
    .await;
    assert_eq!(status, 200, "{saved}");

    let (status, overview) = get(&env, &env.limited, &channel_set(&env, "")).await;
    assert_eq!(status, 200);
    assert!(overview["published"].is_null());
    assert_eq!(overview["draft"]["status"], "draft");
    assert_eq!(overview["draft"]["source"], "admin");
    assert_eq!(overview["draft"]["rationale"], "上游 400 了 user 字段");
    assert_eq!(overview["stale"], false);
    // 草稿不影响线上
    let (_, view) = get(
        &env,
        &env.limited,
        &format!("/admin/channels/{}/compat", env.channel),
    )
    .await;
    assert!(view["channel"].is_null());
    assert_eq!(view["draft"]["rules"], set_user("proposal"));

    // 写草稿的人不能批准自己的草稿
    let publish = channel_set(&env, "/draft/publish");
    let (status, denied) = post(&env, &env.limited, &publish, json!({})).await;
    assert_eq!(status, 403, "{denied}");
    assert_eq!(denied["error"]["param"], "compat.publish");
    let (status, ok) = post(&env, &env.publisher, &publish, json!({})).await;
    assert_eq!(status, 200, "{ok}");
    let version = ok["version"].as_i64().unwrap();
    let (_, view) = get(
        &env,
        &env.limited,
        &format!("/admin/channels/{}/compat", env.channel),
    )
    .await;
    assert_eq!(view["channel"]["version"], version);
    assert_eq!(view["channel"]["rules"], set_user("proposal"));
    assert!(view["draft"].is_null(), "批准后草稿没了");

    // 没有草稿：批准 / 丢弃都是 404
    let (status, _) = post(&env, &env.publisher, &publish, json!({})).await;
    assert_eq!(status, 404);
    let (status, _) = delete(&env, &env.limited, &draft).await;
    assert_eq!(status, 404);
    // 存了再丢
    let (status, _) = put(
        &env,
        &env.limited,
        &draft,
        json!({"rules": set_user("again")}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = delete(&env, &env.limited, &draft).await;
    assert_eq!(status, 200);

    // 草稿也要过校验
    let (status, bad) = put(
        &env,
        &env.limited,
        &draft,
        json!({"rules": [{"id": "x", "do": [{"op": "set", "path": "/model", "value": "evil"}]}]}),
    )
    .await;
    assert_eq!(status, 400, "{bad}");
    assert_eq!(bad["error"]["code"], "compat_rules_invalid");
    // 备注 / 依据太长
    let (status, bad) = put(
        &env,
        &env.limited,
        &draft,
        json!({"rules": [], "rationale": "x".repeat(1001)}),
    )
    .await;
    assert_eq!(status, 400, "{bad}");
    assert_eq!(bad["error"]["param"], "rationale");
}

#[tokio::test]
async fn a_stale_draft_cannot_be_published_and_the_inbox_says_so() {
    let env = setup().await;
    let draft = channel_set(&env, "/draft");
    let (status, _) = put(
        &env,
        &env.limited,
        &draft,
        json!({"rules": set_user("old base"), "base_version": 0}),
    )
    .await;
    assert_eq!(status, 200);
    // 起草时读到的版本对不上：409
    let (status, got) = put(
        &env,
        &env.limited,
        &draft,
        json!({"rules": set_user("x"), "base_version": 7}),
    )
    .await;
    assert_eq!(status, 409, "{got}");
    // 期间有人直接发布了
    let (status, _) = put(
        &env,
        &env.limited,
        &format!("/admin/channels/{}/compat", env.channel),
        json!({"rules": set_user("direct"), "base_version": 0}),
    )
    .await;
    assert_eq!(status, 200);

    let (status, inbox) = get(&env, &env.publisher, "/admin/compat/drafts").await;
    assert_eq!(status, 200, "{inbox}");
    let mine = inbox["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == env.channel)
        .expect("in the inbox");
    assert_eq!(
        (mine["scope"].as_str(), mine["stale"].as_bool()),
        (Some("channel"), Some(true))
    );
    // 草稿占了版本号 1（版本号在草稿 / 已发布 / 归档之间共用，只增不减），直接发布的是 2
    assert_eq!(mine["published_version"], 2);
    let (status, _) = get(&env, &env.limited, "/admin/compat/drafts").await;
    assert_eq!(status, 403, "收件箱只给能批准的人");

    let publish = channel_set(&env, "/draft/publish");
    let (status, conflict) = post(&env, &env.publisher, &publish, json!({})).await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(conflict["error"]["code"], "rules_version_conflict");

    // 在新基础上重新起草（不给 base_version = 按现在的版本）就能批准
    let (status, _) = put(
        &env,
        &env.limited,
        &draft,
        json!({"rules": set_user("rebased")}),
    )
    .await;
    assert_eq!(status, 200);
    let (_, inbox) = get(&env, &env.publisher, "/admin/compat/drafts").await;
    let mine = inbox["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == env.channel)
        .unwrap();
    assert_eq!(mine["stale"], false);
    let (status, ok) = post(&env, &env.publisher, &publish, json!({})).await;
    assert_eq!(status, 200, "{ok}");
    assert!(ok["version"].as_i64().unwrap() > 1);
}

#[tokio::test]
async fn sensitive_drafts_need_a_publisher_who_holds_compat_sensitive() {
    let env = setup().await;
    let draft = channel_set(&env, "/draft");
    let sensitive =
        json!([{"id": "tier", "do": [{"op": "set", "path": "/service_tier", "value": "flex"}]}]);
    // 提议敏感改动不受限（这正是 AI 提议、人批准的用法）
    let (status, saved) = put(&env, &env.limited, &draft, json!({"rules": sensitive})).await;
    assert_eq!(status, 200, "{saved}");
    let publish = channel_set(&env, "/draft/publish");
    let (status, denied) = post(&env, &env.publisher, &publish, json!({})).await;
    assert_eq!(status, 403, "{denied}");
    assert_eq!(denied["error"]["param"], "compat.sensitive");
    let (status, ok) = post(&env, &env.admin, &publish, json!({})).await;
    assert_eq!(status, 200, "{ok}");
}

#[tokio::test]
async fn history_lists_versions_and_rollback_republishes_old_content() {
    let env = setup().await;
    let direct = format!("/admin/channels/{}/compat", env.channel);
    put(
        &env,
        &env.limited,
        &direct,
        json!({"rules": set_user("one"), "base_version": 0}),
    )
    .await;
    put(
        &env,
        &env.limited,
        &direct,
        json!({"rules": set_user("two"), "base_version": 1}),
    )
    .await;
    put(
        &env,
        &env.limited,
        &channel_set(&env, "/draft"),
        json!({"rules": set_user("three")}),
    )
    .await;

    let (status, history) = get(&env, &env.limited, &channel_set(&env, "/versions")).await;
    assert_eq!(status, 200, "{history}");
    let shape: Vec<(i64, &str, i64)> = history["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["version"].as_i64().unwrap(),
                v["status"].as_str().unwrap(),
                v["rules"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [(3, "draft", 1), (2, "published", 1), (1, "archived", 1)]
    );
    let (status, one) = get(&env, &env.limited, &channel_set(&env, "/versions/1")).await;
    assert_eq!(status, 200);
    assert_eq!(one["rules"], set_user("one"));
    let (status, _) = get(&env, &env.limited, &channel_set(&env, "/versions/99")).await;
    assert_eq!(status, 404);

    let rollback = channel_set(&env, "/rollback");
    let (status, denied) = post(&env, &env.limited, &rollback, json!({"version": 1})).await;
    assert_eq!(status, 403, "{denied}");
    let (status, _) = post(&env, &env.publisher, &rollback, json!({"version": 99})).await;
    assert_eq!(status, 404);
    let (status, _) = post(&env, &env.publisher, &rollback, json!({"version": 3})).await;
    assert_eq!(status, 404, "草稿不能回滚到");
    let (status, ok) = post(&env, &env.publisher, &rollback, json!({"version": 1})).await;
    assert_eq!(status, 200, "{ok}");
    assert_eq!(ok["version"], 4);
    let (_, view) = get(&env, &env.limited, &direct).await;
    assert_eq!(view["channel"]["rules"], set_user("one"));
    assert_eq!(view["channel"]["version"], 4);

    // 范围写错或对象不存在
    let (status, _) = get(&env, &env.limited, "/admin/compat/sets/planet/1").await;
    assert_eq!(status, 404);
    let (status, _) = get(&env, &env.limited, "/admin/compat/sets/channel/9999999").await;
    assert_eq!(status, 404);
    let (status, _) = get(&env, &env.limited, "/admin/compat/sets/profile/9999999").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn the_guard_goes_through_the_same_draft_flow_with_its_own_permission() {
    let env = setup().await;
    let draft = "/admin/compat/sets/guard/0/draft";
    let (status, _) = put(&env, &env.limited, draft, json!({"rules": []})).await;
    assert_eq!(status, 403, "护栏草稿要 compat.guard.write");
    let rules = json!([{"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}]);
    let (status, saved) = put(&env, &env.admin, draft, json!({"rules": rules})).await;
    assert_eq!(status, 200, "{saved}");
    let (_, guard) = get(&env, &env.limited, "/admin/compat/guard").await;
    assert!(guard["guard"].is_null());
    assert_eq!(guard["draft"]["rules"], rules);
    let (status, ok) = post(
        &env,
        &env.admin,
        "/admin/compat/sets/guard/0/draft/publish",
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "{ok}");
    let (_, guard) = get(&env, &env.limited, "/admin/compat/guard").await;
    assert_eq!(guard["guard"]["rules"], rules);
}

// ------------------------------------------------------------------------------------ dry-run

#[tokio::test]
// 四个阶段共用一个渠道与同一批规则，逐阶段断言放在一起才看得出阶段之间互不串
#[allow(clippy::too_many_lines)]
async fn dry_run_covers_every_phase() {
    let env = setup().await;
    let url = "/admin/compat/dry-run";
    let ctx = json!({"model": "m", "host": "relay.example.com"});

    // 响应阶段
    let (status, got) = post(
        &env,
        &env.limited,
        url,
        json!({
            "phase": "response", "channel_id": env.channel, "context": ctx,
            "rules": [{"id": "alias", "phase": "response", "select": {"dialect": ["openai.chat"]},
                       "do": [{"op": "each", "path": "/choices", "do": [
                           {"op": "copy", "from": "/message/reasoning", "to": "/message/reasoning_content", "mode": "if_absent"}]}]}],
            "response": {"choices": [{"message": {"reasoning": "think"}}]},
        }),
    )
    .await;
    assert_eq!(status, 200, "{got}");
    assert_eq!(got["phase"], "response");
    assert_eq!(got["changed"], true);
    assert_eq!(
        got["after"]["choices"][0]["message"]["reasoning_content"],
        "think"
    );
    assert_eq!(got["fired"][0]["rule"], "alias");

    // 错误阶段：命中、重发会怎么改
    let (status, got) = post(
        &env,
        &env.limited,
        url,
        json!({
            "phase": "error", "channel_id": env.channel, "context": ctx,
            "rules": [{"id": "drop-user", "phase": "error", "when": {"status": [400], "body_contains": ["unsupported parameter"]},
                       "do": [{"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}]}]},
                      {"id": "fallback", "phase": "error", "when": {"status": [400]}, "do": [{"op": "failover"}]}],
            "error": {"status": 400, "body": {"error": {"message": "Unsupported parameter: user"}}},
            "request": {"model": "m", "user": "u"},
        }),
    )
    .await;
    assert_eq!(status, 200, "{got}");
    assert_eq!(got["matched"], true);
    assert_eq!(got["chosen"], 0);
    assert_eq!(got["reactions"][0]["action"], "retry_same");
    assert_eq!(
        got["reactions"][0]["detail"]["resend"],
        json!({"model": "m"})
    );
    // 请求里本来就没有 user：改完没有变化 → 这条重发不可用，轮到下一条
    let (_, got) = post(
        &env,
        &env.limited,
        url,
        json!({
            "phase": "error", "channel_id": env.channel, "context": ctx,
            "rules": [{"id": "drop-user", "phase": "error", "when": {"status": [400]},
                       "do": [{"op": "retry_same", "mutate": [{"op": "remove", "paths": ["/user"]}]}]},
                      {"id": "fallback", "phase": "error", "when": {"status": [400]}, "do": [{"op": "failover"}]}],
            "error": {"status": 400, "body": "plain text body"},
            "request": {"model": "m"},
        }),
    )
    .await;
    assert_eq!(got["reactions"][0]["viable"], false);
    assert_eq!(got["chosen"], 1);
    // 没有规则命中
    let (_, got) = post(
        &env,
        &env.limited,
        url,
        json!({"phase": "error", "context": ctx, "error": {"status": 422}}),
    )
    .await;
    assert_eq!(
        (got["matched"].as_bool(), got["chosen"].is_null()),
        (Some(false), true)
    );

    // 能力阶段：并上渠道自己声明的（渠道优先）
    sqlx::query("UPDATE channels SET capabilities = $2 WHERE id = $1")
        .bind(env.channel)
        .bind(json!({"tools": false}))
        .execute(&env.pg)
        .await
        .unwrap();
    let (status, got) = post(
        &env,
        &env.limited,
        url,
        json!({
            "phase": "capability", "channel_id": env.channel, "context": ctx,
            "rules": [{"id": "caps", "phase": "capability", "do": [{"op": "capability", "facts": {"tools": true, "vision": false}}]}],
        }),
    )
    .await;
    assert_eq!(status, 200, "{got}");
    assert_eq!(got["facts"], json!({"tools": true, "vision": false}));
    assert_eq!(got["merged"], json!({"tools": false, "vision": false}));

    // 写错的规则集、缺样本、不支持的阶段
    let (status, got) = post(&env, &env.limited, url, json!({"context": ctx, "request": {}, "rules": [{"id": "x", "phase": "response", "do": [{"op": "remove", "paths": ["/a"]}]}]})).await;
    assert_eq!(status, 200);
    assert_eq!(got["valid"], false);
    assert_eq!(got["issues"][0]["code"], "response_dialect_unsupported");
    let (status, got) = post(
        &env,
        &env.limited,
        url,
        json!({"phase": "response", "context": ctx}),
    )
    .await;
    assert_eq!(
        (status, got["error"]["param"].as_str()),
        (400, Some("response"))
    );
    let (status, got) = post(
        &env,
        &env.limited,
        url,
        json!({"phase": "wire", "context": ctx}),
    )
    .await;
    assert_eq!(
        (status, got["error"]["param"].as_str()),
        (400, Some("phase"))
    );
}

// ------------------------------------------------------------------------------------ explain

#[tokio::test]
async fn explain_resolves_the_rule_versions_a_request_used() {
    let env = setup().await;
    let (_, created) = post(
        &env,
        &env.profiler,
        "/admin/compat/profiles",
        json!({"slug": "acme", "name": "Acme"}),
    )
    .await;
    let profile_id = created["id"].as_i64().unwrap();
    put(
        &env,
        &env.publisher,
        &format!("/admin/compat/sets/profile/{profile_id}/rules"),
        json!({"rules": set_user("profile"), "base_version": 0}),
    )
    .await;
    put(
        &env,
        &env.limited,
        &format!("/admin/channels/{}/compat", env.channel),
        json!({"rules": set_user("channel"), "base_version": 0}),
    )
    .await;

    let request_id = Uuid::new_v4();
    let diagnostics = json!({"diagnostics": {
        "compat": {
            "channel_id": env.channel,
            "versions": ["builtin:deepseek", "custom:acme@v1", format!("channel#{}@v1", env.channel), "guard@v9"],
            "fired": [{"rule": "u", "layer": "channel", "changes": 1}], "skipped": []
        },
        "compat_error": {"rule": "drop-user", "action": "retry_same", "versions": [format!("channel#{}@v1", env.channel)]},
        "other": {"x": 1}
    }});
    sqlx::query("INSERT INTO billing_records (request_id, user_id, model_name, status, usage_details) VALUES ($1, $2, 'm', 1, $3)")
        .bind(request_id)
        .bind(env.admin_id)
        .bind(&diagnostics)
        .execute(&env.pg)
        .await
        .unwrap();

    let (status, got) = get(
        &env,
        &env.admin,
        &format!("/admin/compat/explain/{request_id}"),
    )
    .await;
    assert_eq!(status, 200, "{got}");
    assert_eq!(got["trace"]["compat"]["fired"][0]["rule"], "u");
    assert_eq!(got["trace"]["compat_error"]["action"], "retry_same");
    assert!(got["trace"].get("other").is_none(), "只摘适配规则的记录");
    let layers = got["layers"].as_array().unwrap();
    let by_label = |label: &str| {
        layers
            .iter()
            .find(|l| l["label"] == label)
            .unwrap_or_else(|| panic!("{label}: {got}"))
    };
    assert_eq!(by_label("builtin:deepseek")["layer"], "vendor");
    assert!(
        by_label("builtin:deepseek")["rules"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    );
    assert_eq!(by_label("custom:acme@v1")["layer"], "profile");
    assert_eq!(by_label("custom:acme@v1")["rules"], set_user("profile"));
    assert_eq!(
        by_label(&format!("channel#{}@v1", env.channel))["rules"],
        set_user("channel")
    );
    assert!(
        by_label("guard@v9")["rules"].is_null(),
        "取不到的版本给 null，不报错"
    );
    assert_eq!(layers.len(), 4, "同一个标签只取一次");

    let (status, _) = get(
        &env,
        &env.admin,
        &format!("/admin/compat/explain/{}", Uuid::new_v4()),
    )
    .await;
    assert_eq!(status, 404);
    let (status, _) = get(
        &env,
        &env.limited,
        &format!("/admin/compat/explain/{request_id}"),
    )
    .await;
    assert_eq!(status, 403, "要 billing.read");
}
