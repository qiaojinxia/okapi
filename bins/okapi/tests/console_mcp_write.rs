//! MCP 写工具三道闸验收（§7.3）：全局开关（默认 OFF）→ mcp.write + 资源权限 →
//! confirm 两段式；diagnose 全链路；dlq_requeue 闭环；审计 actor = mcp:{key_id}。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
    admin_token: String,
    admin_key_id: i64,
}

async fn setup(write_enabled: bool) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('ssrf_policy', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        serde_json::json!({"allow_http": true, "allow_private": true})
    )
    .execute(&pg)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('mcp_write_enabled', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!(write_enabled)
    )
    .execute(&pg)
    .await
    .unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("mw-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-mw-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(admin_token.as_bytes()))
    };
    let admin_key_id =
        okapi_store::provision::create_api_key(&pg, admin_id, &key_hash, "sk-okapi-mw")
            .await
            .unwrap();

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
        admin_key_id,
    }
}

async fn rpc(env: &TestEnv, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(format!("http://{}/mcp", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn call(env: &TestEnv, tool: &str, args: Value) -> Value {
    rpc(env, "tools/call", json!({"name": tool, "arguments": args})).await
}

/// 工具**内部**的报错走 `result.isError`（内容是 error_code），
/// 只有准入闸（写开关 / 权限）才走 JSON-RPC 的 `error` 通道——两条路别混着断言。
fn assert_tool_error(resp: &Value, code: &str) {
    assert_eq!(resp["result"]["isError"], true, "应是工具级错误：{resp}");
    assert_eq!(resp["result"]["content"][0]["text"], code, "{resp}");
    assert!(
        resp["error"].is_null(),
        "工具级错误不该占用 RPC error 通道：{resp}"
    );
}

async fn set_gate(env: &TestEnv, enabled: bool) {
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('mcp_write_enabled', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!(enabled)
    )
    .execute(&env.pg)
    .await
    .unwrap();
}

/// 三道闸 + 两段式 + 闭环：settings 开关是全局键，场景内串行推进避免并行互踩。
#[tokio::test]
// 端到端场景脚本：阶段拆分割裂闸门时序语义
#[allow(clippy::too_many_lines)]
async fn mcp_write_full_scenario() {
    // —— 阶段 1：开关关 ——
    let env = setup(false).await;
    let list = rpc(&env, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"channel_create"), "开关关必须隐藏写工具");
    assert!(names.contains(&"diagnose"), "diagnose 是只读工具");

    let denied = call(&env, "user_ban", json!({"user_id": 1})).await;
    assert_eq!(denied["error"]["message"], "mcp_write_disabled");

    let diag = call(&env, "diagnose", json!({})).await;
    let sc = &diag["result"]["structuredContent"];
    assert_eq!(sc["postgres"], true);
    assert_eq!(sc["redis"], true);
    assert!(sc["outbox_pending"].as_i64().unwrap() >= 0);

    // —— 阶段 2：开关开，渠道生命周期 + 审计 ——
    set_gate(&env, true).await;
    let suffix = Uuid::new_v4().simple().to_string();
    let created = call(
        &env,
        "channel_create",
        json!({"name": format!("mcp-{suffix}"), "api_base": "http://127.0.0.1:9/v1",
            "credential": "c", "models": [format!("m-{suffix}")]}),
    )
    .await;
    let channel_id = created["result"]["structuredContent"]["channel_id"]
        .as_i64()
        .expect("必须返回渠道 id");

    let toggled = call(
        &env,
        "channel_toggle",
        json!({"channel_id": channel_id, "enable": false}),
    )
    .await;
    assert_eq!(toggled["result"]["structuredContent"]["status"], 2);

    // 测活（不可达上游 → ok=false 带 error_code）
    let probed = call(&env, "channel_test", json!({"channel_id": channel_id})).await;
    assert_eq!(probed["result"]["structuredContent"]["ok"], false);

    let audits = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs
           WHERE actor = $1 AND action IN ('channel.create', 'channel.status', 'channel.test')"#,
        format!("mcp:{}", env.admin_key_id)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(audits, 3, "MCP 写操作必须以 mcp:{{key_id}} 留痕");

    // —— 阶段 3：两段式 confirm ——
    let suffix = Uuid::new_v4().simple().to_string();
    let target = okapi_store::provision::create_user(&env.pg, &format!("tt-{suffix}"))
        .await
        .unwrap();

    let preview = call(
        &env,
        "user_adjust_balance",
        json!({"user_id": target, "amount_micro": 5_000_000}),
    )
    .await;
    let sc = &preview["result"]["structuredContent"];
    assert_eq!(sc["dry_run"], true);
    assert_eq!(sc["balance_after_micro"], 5_000_000);

    let events = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events WHERE user_id = $1"#,
        target
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(events, 0, "dry-run 不得产生账务事件");

    let applied = call(
        &env,
        "user_adjust_balance",
        json!({"user_id": target, "amount_micro": 5_000_000, "confirm": true}),
    )
    .await;
    assert_eq!(
        applied["result"]["structuredContent"]["balance_after_micro"],
        5_000_000
    );

    // ban：dry-run 不动，confirm 后 status=2
    let _ = call(&env, "user_ban", json!({"user_id": target})).await;
    let status = sqlx::query_scalar!(r#"SELECT status FROM users WHERE id = $1"#, target)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 1, "dry-run 不得封禁");
    let _ = call(
        &env,
        "user_ban",
        json!({"user_id": target, "confirm": true}),
    )
    .await;
    let status = sqlx::query_scalar!(r#"SELECT status FROM users WHERE id = $1"#, target)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 2);

    // —— 阶段 4：dlq_requeue 闭环 ——
    let marker = Uuid::new_v4().to_string();
    let dlq_id = sqlx::query_scalar!(
        r#"INSERT INTO billing_dlq (source, payload, error, retry_count)
           VALUES ('chsink', $1, 'boom', 5) RETURNING id"#,
        json!({"request_id": marker, "user_id": 1, "log_type": 2})
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();

    let dry = call(&env, "dlq_requeue", json!({"ids": [dlq_id]})).await;
    assert_eq!(dry["result"]["structuredContent"]["dry_run"], true);

    let done = call(
        &env,
        "dlq_requeue",
        json!({"ids": [dlq_id], "confirm": true}),
    )
    .await;
    assert_eq!(done["result"]["structuredContent"]["requeued"], 1);

    let in_outbox = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_outbox
           WHERE status = 0 AND payload->>'request_id' = $1"#,
        marker
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(in_outbox, 1, "必须重入 outbox 待投");
    let in_dlq = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_dlq WHERE id = $1"#,
        dlq_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(in_dlq, 0, "DLQ 行必须清除");

    // —— 阶段 5：simulate → apply ——
    let sim = call(&env, "simulate_pricing", json!({})).await;
    assert_eq!(sim["result"]["structuredContent"]["ok"], true);

    let before =
        sqlx::query_scalar!(r#"SELECT COALESCE(MAX(epoch), 0) AS "e!" FROM pricing_epochs"#)
            .fetch_one(&env.pg)
            .await
            .unwrap();

    let dry = call(&env, "apply_pricing", json!({})).await;
    assert_eq!(dry["result"]["structuredContent"]["dry_run"], true);

    let applied = call(&env, "apply_pricing", json!({"confirm": true})).await;
    let epoch = applied["result"]["structuredContent"]["epoch"]
        .as_i64()
        .expect("必须返回新 epoch");
    assert!(epoch > before);

    // —— 阶段 6：redemption_create / cache_flush（09-08 第十九轮补，此前两个写工具零调用）——
    // redemption_create 是唯一能凭空造出可入账凭证的 MCP 工具，两段式那道闸尤其不能漏。
    assert_tool_error(
        &call(
            &env,
            "redemption_create",
            json!({"count": 0, "amount_micro": 1}),
        )
        .await,
        "bad_request",
    );
    assert_tool_error(
        &call(
            &env,
            "redemption_create",
            json!({"count": 1, "amount_micro": 0}),
        )
        .await,
        "bad_request",
    );

    let before = sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM redemption_codes"#)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    let dry = call(
        &env,
        "redemption_create",
        json!({"count": 3, "amount_micro": 2_000_000}),
    )
    .await;
    let sc = &dry["result"]["structuredContent"];
    assert_eq!(sc["dry_run"], true);
    assert!(sc["codes"].is_null(), "预览不得吐出明文码：{dry}");
    assert_eq!(
        sqlx::query_scalar!(r#"SELECT COUNT(*)::bigint AS "c!" FROM redemption_codes"#)
            .fetch_one(&env.pg)
            .await
            .unwrap(),
        before,
        "dry-run 一张码都不许落库"
    );

    let minted = call(
        &env,
        "redemption_create",
        json!({"count": 3, "amount_micro": 2_000_000, "confirm": true}),
    )
    .await;
    let sc = &minted["result"]["structuredContent"];
    assert_eq!(sc["dry_run"], false);
    let codes = sc["codes"].as_array().expect("确认后返回明文码");
    assert_eq!(codes.len(), 3);
    assert!(
        codes
            .iter()
            .all(|c| c.as_str().unwrap().starts_with("okapi-"))
    );
    let batch = uuid::Uuid::parse_str(sc["batch_id"].as_str().unwrap()).unwrap();
    let rows = sqlx::query!(
        r#"SELECT amount_micro, status, code_hash FROM redemption_codes WHERE batch_id = $1"#,
        batch
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|r| r.amount_micro == 2_000_000 && r.status == 1),
        "三张都该是可用的 $2 码"
    );
    // 落库的是哈希不是明文——MCP 那头返回的明文是唯一一次，库里翻不出来
    let plain = codes[0].as_str().unwrap();
    let want = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(plain.as_bytes()))
    };
    assert!(
        rows.iter().any(|r| r.code_hash == want),
        "库里应存明文的 sha256"
    );
    assert!(!rows.iter().any(|r| r.code_hash == plain), "明文不得落库");

    // cache_flush：三种范围各刷一次；范围不认得 400
    for scope in ["auth", "routing", "pricebook"] {
        let flushed = call(&env, "cache_flush", json!({ "scope": scope })).await;
        assert_eq!(
            flushed["result"]["structuredContent"]["ok"], true,
            "{scope}: {flushed}"
        );
    }
    assert_tool_error(
        &call(&env, "cache_flush", json!({"scope": "everything"})).await,
        "bad_request",
    );

    // 两个工具的写操作都以 mcp:{key_id} 留痕，cache.flush 的 target 是范围本身
    let flushed_scopes = sqlx::query_scalar!(
        r#"SELECT target FROM audit_logs WHERE actor = $1 AND action = 'cache.flush'
           ORDER BY created_at, id"#,
        format!("mcp:{}", env.admin_key_id)
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        flushed_scopes,
        vec![
            Some("auth".to_owned()),
            Some("routing".to_owned()),
            Some("pricebook".to_owned())
        ]
    );
    let minted_audit = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs
           WHERE actor = $1 AND action = 'redemption.create' AND target = $2"#,
        format!("mcp:{}", env.admin_key_id),
        batch.to_string()
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(minted_audit, 1, "只有确认那次留痕，预览不留");

    // 收尾：把 MCP 写开关关回去。它是**站点级**设置，本用例开了不收拾的话，
    // 开发库上就永远挂着一个没人打开过的写工具面——三道闸的第一道形同虚设，
    // 而且从设置页上看不出这是测试留下的。
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'mcp_write_enabled'"#)
        .execute(&env.pg)
        .await
        .unwrap();
}
