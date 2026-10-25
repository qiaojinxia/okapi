//! MCP 的适配规则工具验收（docs/compat-rules.md §17.7）：五个只读工具（目录 / 有效规则 / 校验 / 试跑 / 解释）
//! 不受写开关影响；`compat_propose` 走三道闸、**只落草稿**（来源 mcp、带依据、审计 actor = mcp:{key_id}），
//! 绝不发布。依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct Env {
    pg: PgPool,
    addr: SocketAddr,
    /// super admin：有全部权限点。
    admin: String,
    admin_key_id: i64,
    /// channel.read + mcp.write（能看、不能写草稿）。
    reader: String,
    /// channel.write + mcp.write（能给渠道提议，没有 compat.guard.write / compat.profile.write）。
    writer: String,
    channel: i64,
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn user(pg: &PgPool, role: i16, admin_role_id: Option<i64>) -> (i64, String, i64) {
    let suffix = Uuid::new_v4().simple().to_string();
    let id = okapi_store::provision::create_user(pg, &format!("mc-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = $2, admin_role_id = $3 WHERE id = $1")
        .bind(id)
        .bind(role)
        .bind(admin_role_id)
        .execute(pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-mc-{suffix}");
    let key_id = okapi_store::provision::create_api_key(pg, id, &hash(&token), "sk-mc")
        .await
        .unwrap();
    (id, token, key_id)
}

async fn role_user(addr: SocketAddr, pg: &PgPool, admin: &str, permissions: &[&str]) -> String {
    let code = format!("mc-{}", Uuid::new_v4().simple());
    let created: Value = reqwest::Client::new()
        .post(format!("http://{addr}/admin/roles"))
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
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (_, admin, admin_key_id) = user(&pg, 100, None).await;
    let reader = role_user(addr, &pg, &admin, &["channel.read", "mcp.write"]).await;
    let writer = role_user(
        addr,
        &pg,
        &admin,
        &["channel.read", "channel.write", "mcp.write"],
    )
    .await;
    let (channel, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("mc-{}", Uuid::new_v4().simple()),
        "openai",
        "http://127.0.0.1:9/v1",
        "x",
        &["m-x"],
        false,
        None,
    )
    .await
    .unwrap();
    Env {
        pg,
        addr,
        admin,
        admin_key_id,
        reader,
        writer,
        channel,
    }
}

async fn set_gate(env: &Env, enabled: bool) {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('mcp_write_enabled', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(json!(enabled))
    .execute(&env.pg)
    .await
    .unwrap();
}

async fn rpc(env: &Env, token: &str, method: &str, params: Value) -> Value {
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

async fn call(env: &Env, token: &str, tool: &str, args: Value) -> Value {
    rpc(
        env,
        token,
        "tools/call",
        json!({"name": tool, "arguments": args}),
    )
    .await
}

async fn tool_names(env: &Env, token: &str) -> Vec<String> {
    rpc(env, token, "tools/list", json!({})).await["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// 工具内部的报错走 `result.isError`（内容是 error_code）；准入闸（写开关 / 权限）才走 RPC 的 `error`。
fn assert_tool_error(resp: &Value, code: &str) {
    assert_eq!(resp["result"]["isError"], true, "应是工具级错误：{resp}");
    assert_eq!(resp["result"]["content"][0]["text"], code, "{resp}");
    assert!(resp["error"].is_null(), "{resp}");
}

fn remove_user() -> Value {
    json!([{"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}])
}

#[tokio::test]
// 端到端场景脚本：写开关是全局键，阶段按开关的先后推进
#[allow(clippy::too_many_lines)]
async fn mcp_compat_tools_scenario() {
    let env = setup().await;
    let admin = env.admin.clone();

    // —— 阶段 1：写开关关。只读工具照常在，propose 不在 ——
    set_gate(&env, false).await;
    let names = tool_names(&env, &admin).await;
    for tool in [
        "compat_catalog",
        "compat_effective",
        "compat_validate",
        "compat_dry_run",
        "compat_explain",
    ] {
        assert!(
            names.contains(&tool.to_owned()),
            "{tool} 是只读工具：{names:?}"
        );
    }
    assert!(
        !names.contains(&"compat_propose".to_owned()),
        "开关关必须隐藏写工具"
    );
    let denied = call(&env, &admin, "compat_propose", json!({})).await;
    assert_eq!(denied["error"]["message"], "mcp_write_disabled");

    // 目录：阶段、操作、配方
    let catalog = call(&env, &admin, "compat_catalog", json!({})).await;
    let catalog = &catalog["result"]["structuredContent"];
    assert_eq!(catalog["phases"].as_array().unwrap().len(), 6, "{catalog}");
    assert!(catalog["recipes"].as_array().unwrap().len() >= 8);

    // 校验：合法的给规范化后的规则与需要 compat.sensitive 的操作；不合法的给问题清单
    let valid = call(
        &env,
        &admin,
        "compat_validate",
        json!({"rules": remove_user()}),
    )
    .await;
    let valid = &valid["result"]["structuredContent"];
    assert_eq!(
        (valid["valid"].as_bool(), valid["count"].as_i64()),
        (Some(true), Some(1)),
        "{valid}"
    );
    assert_eq!(valid["sensitive"], json!([]));
    let sensitive = call(
        &env,
        &admin,
        "compat_validate",
        json!({"rules": [{"id": "tier", "do": [{"op": "set", "path": "/service_tier", "value": "flex"}]}]}),
    )
    .await;
    assert!(
        !sensitive["result"]["structuredContent"]["sensitive"]
            .as_array()
            .unwrap()
            .is_empty(),
        "写 /service_tier 要标出需要 compat.sensitive：{sensitive}"
    );
    let invalid = call(
        &env,
        &admin,
        "compat_validate",
        json!({"rules": [{"id": "bad", "do": [{"op": "remove", "paths": ["/model"]}]}]}),
    )
    .await;
    let invalid = &invalid["result"]["structuredContent"];
    assert_eq!(invalid["valid"], false, "主链字段写不进去：{invalid}");
    assert!(
        !invalid["issues"].as_array().unwrap().is_empty(),
        "{invalid}"
    );
    assert_tool_error(
        &call(&env, &admin, "compat_validate", json!({})).await,
        "bad_request",
    );

    // 有效规则：没选厂商、没有任何一层
    let effective = call(
        &env,
        &admin,
        "compat_effective",
        json!({"channel_id": env.channel}),
    )
    .await;
    let effective = &effective["result"]["structuredContent"];
    assert_eq!(effective["vendor"], Value::Null, "{effective}");
    assert_eq!(effective["effective"]["rules"], json!([]));
    assert_tool_error(
        &call(&env, &admin, "compat_effective", json!({"channel_id": 0})).await,
        "not_found",
    );
    assert_tool_error(
        &call(&env, &admin, "compat_effective", json!({})).await,
        "bad_request",
    );

    // 试跑：用一份渠道层草稿在样本上回放，不落库
    let dry = call(
        &env,
        &admin,
        "compat_dry_run",
        json!({
            "channel_id": env.channel, "rules": remove_user(),
            "context": {"model": "m-x"},
            "request": {"model": "m-x", "user": "u-1", "messages": []}
        }),
    )
    .await;
    let dry = &dry["result"]["structuredContent"];
    assert_eq!(dry["valid"], true, "{dry}");
    assert_eq!(dry["changed"], true);
    assert_eq!(dry["after"], json!({"model": "m-x", "messages": []}));
    assert_eq!(dry["fired"][0]["rule"], "no-user", "{dry}");
    // 带不认识的字段 / 缺样本都是 400，不是 500
    assert_tool_error(
        &call(
            &env,
            &admin,
            "compat_dry_run",
            json!({"context": {"model": "m"}, "bogus": 1}),
        )
        .await,
        "bad_request",
    );
    assert_tool_error(
        &call(
            &env,
            &admin,
            "compat_dry_run",
            json!({"context": {"model": "m"}}),
        )
        .await,
        "bad_request",
    );

    // 解释：不存在的请求 id → not_found；不是 uuid → bad_request
    let missing = Uuid::new_v4().to_string();
    assert_tool_error(
        &call(
            &env,
            &admin,
            "compat_explain",
            json!({"request_id": missing}),
        )
        .await,
        "not_found",
    );
    assert_tool_error(
        &call(
            &env,
            &admin,
            "compat_explain",
            json!({"request_id": "nope"}),
        )
        .await,
        "bad_request",
    );

    // —— 阶段 2：开关开。propose 只落草稿 ——
    set_gate(&env, true).await;
    assert!(
        tool_names(&env, &admin)
            .await
            .contains(&"compat_propose".to_owned())
    );

    let propose = |args: Value| {
        let admin = admin.clone();
        let env = &env;
        async move { call(env, &admin, "compat_propose", args).await }
    };
    // 依据必填；规则要合法
    assert_tool_error(
        &propose(json!({"scope": "channel", "id": env.channel, "rules": remove_user()})).await,
        "bad_request",
    );
    assert_tool_error(
        &propose(json!({"scope": "channel", "id": env.channel, "rules": remove_user(), "rationale": "   "})).await,
        "bad_request",
    );
    assert_tool_error(
        &propose(json!({"scope": "channel", "id": env.channel, "rationale": "r", "rules": "not an array"})).await,
        "bad_request",
    );
    assert_tool_error(
        &propose(
            json!({"scope": "channel", "id": env.channel, "rationale": "r",
            "rules": [{"id": "bad", "do": [{"op": "remove", "paths": ["/model"]}]}]}),
        )
        .await,
        "compat_rules_invalid",
    );
    assert_tool_error(
        &propose(json!({"scope": "channel", "id": 0, "rationale": "r", "rules": remove_user()}))
            .await,
        "not_found",
    );
    assert_tool_error(
        &propose(json!({"scope": "everything", "rationale": "r", "rules": remove_user()})).await,
        "not_found",
    );
    assert_tool_error(
        &propose(
            json!({"scope": "channel", "id": env.channel, "rationale": "r", "rules": remove_user(),
            "base_version": 99}),
        )
        .await,
        "rules_version_conflict",
    );

    let proposed = propose(json!({
        "scope": "channel", "id": env.channel, "rules": remove_user(),
        "rationale": "上游对 user 字段返回 400：unknown parameter user", "note": "drop user",
    }))
    .await;
    let proposed = &proposed["result"]["structuredContent"];
    assert_eq!(proposed["ok"], true, "{proposed}");
    assert_eq!(proposed["status"], "draft");
    assert_eq!(proposed["needs_approval"], true);

    // 只是草稿：线上没有任何已发布版本，网关看到的有效规则不变
    let published: Option<i64> = sqlx::query_scalar(
        "SELECT count(*) FROM rule_sets WHERE scope_kind = 'channel' AND scope_id = $1 AND status = 'published'",
    )
    .bind(env.channel)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(published, Some(0), "propose 绝不能发布");
    let effective = call(
        &env,
        &admin,
        "compat_effective",
        json!({"channel_id": env.channel}),
    )
    .await;
    let effective = &effective["result"]["structuredContent"];
    assert_eq!(
        effective["effective"]["rules"],
        json!([]),
        "草稿不进有效规则"
    );
    assert_eq!(effective["channel"], Value::Null);
    // 草稿带来源与依据，批准的人在页面上看得到
    assert_eq!(effective["draft"]["source"], "mcp", "{effective}");
    assert_eq!(
        effective["draft"]["rationale"],
        "上游对 user 字段返回 400：unknown parameter user"
    );
    assert_eq!(effective["draft"]["note"], "drop user");
    assert_eq!(effective["draft"]["status"], "draft");

    // 审计：actor 是那把 key，不是用户
    let audit: Option<(String, String)> = sqlx::query_as(
        "SELECT actor, target FROM audit_logs WHERE action = 'compat.draft.save' ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&env.pg)
    .await
    .unwrap();
    let (actor, target) = audit.expect("审计");
    assert_eq!(actor, format!("mcp:{}", env.admin_key_id));
    assert_eq!(target, format!("channel:{}", env.channel));

    // 再提议一次：替换同范围的草稿（一个范围只有一份草稿）
    let again = propose(json!({
        "scope": "channel", "id": env.channel, "rationale": "改成删 user 和 metadata",
        "rules": [{"id": "no-user", "do": [{"op": "remove", "paths": ["/user", "/metadata"]}]}],
    }))
    .await;
    assert_eq!(again["result"]["structuredContent"]["ok"], true, "{again}");
    let drafts: Option<i64> = sqlx::query_scalar(
        "SELECT count(*) FROM rule_sets WHERE scope_kind = 'channel' AND scope_id = $1 AND status = 'draft'",
    )
    .bind(env.channel)
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(drafts, Some(1));

    // 护栏范围也能提议（super admin 有 compat.guard.write），同样只是草稿
    let guard =
        propose(json!({"scope": "guard", "rationale": "全局去掉 user", "rules": remove_user()}))
            .await;
    assert_eq!(
        guard["result"]["structuredContent"]["status"], "draft",
        "{guard}"
    );
    let guard_published: Option<i64> = sqlx::query_scalar(
        "SELECT count(*) FROM rule_sets WHERE scope_kind = 'global' AND status = 'published'",
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(guard_published, Some(0));
    sqlx::query("DELETE FROM rule_sets WHERE scope_kind = 'global'")
        .execute(&env.pg)
        .await
        .unwrap();

    // —— 阶段 3：权限 ——
    // 只有 channel.read + mcp.write：看得到只读工具，看不到 propose（缺 channel.write）
    let names = tool_names(&env, &env.reader).await;
    assert!(names.contains(&"compat_effective".to_owned()));
    assert!(!names.contains(&"compat_propose".to_owned()), "{names:?}");
    let resp = call(
        &env,
        &env.reader,
        "compat_propose",
        json!({"scope": "channel", "id": env.channel}),
    )
    .await;
    assert_eq!(resp["error"]["message"], "permission_denied", "{resp}");
    // 看不到没有 billing.read 的人的 explain
    assert!(!names.contains(&"compat_explain".to_owned()));

    // channel.write + mcp.write：渠道范围可以；档案 / 护栏要各自的写权限点
    let names = tool_names(&env, &env.writer).await;
    assert!(names.contains(&"compat_propose".to_owned()));
    let ok = call(
        &env,
        &env.writer,
        "compat_propose",
        json!({"scope": "channel", "id": env.channel, "rationale": "writer 的提议", "rules": remove_user()}),
    )
    .await;
    assert_eq!(ok["result"]["structuredContent"]["ok"], true, "{ok}");
    for scope in ["guard", "profile"] {
        let id = i64::from(scope == "profile");
        let resp = call(
            &env,
            &env.writer,
            "compat_propose",
            json!({"scope": scope, "id": id, "rationale": "x", "rules": remove_user()}),
        )
        .await;
        assert_tool_error(&resp, "permission_denied");
    }

    // 开关关回去：写工具立刻不可用
    set_gate(&env, false).await;
    let resp = call(&env, &admin, "compat_propose", json!({})).await;
    assert_eq!(resp["error"]["message"], "mcp_write_disabled");
    sqlx::query("DELETE FROM settings WHERE key = 'mcp_write_enabled'")
        .execute(&env.pg)
        .await
        .unwrap();
}
