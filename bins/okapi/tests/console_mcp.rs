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
    // 接上 CH：query_usage / platform_kpi 没有它一律 stats_disabled，接不上就等于没测
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
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

/// 22 个 MCP 工具里有 7 个从没被任何用例调用过（09-08 第十九轮机械对表发现）。这里补齐
/// 5 个只读的：`list_my_keys` / `list_models_pricing` / `channel_health` 三个走 PG，
/// `query_usage` / `platform_kpi` 两个走 CH。
///
/// 除了输出形状，重点是两条不该错的边界：只读工具里带 `permission` 的那几个（platform_kpi 要
/// billing.read、channel_health 要 channel.read）对普通用户必须既不在 tools/list 里、调也调不动；
/// 而 `list_my_keys` 是无权限点的用户级工具，它的"自己"必须是**调用方这把 key 的属主**，
/// 不能顺手把别人的 key 列出来。
// 五个工具一次对表：拆开会各自重建一遍用户 / 模型 / 渠道，反而看不出"哪些还没被调过"
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn readonly_tools_cover_keys_pricing_health_and_ch_backed_usage() {
    let env = setup().await;
    let (user_id, user_token) = mk_user(&env.pg, 1).await;
    let (_, admin_token) = mk_user(&env.pg, 100).await;
    let suffix = Uuid::new_v4().simple().to_string();

    let call = async |token: &str, tool: &str, args: Value| {
        rpc(
            &env,
            token,
            "tools/call",
            json!({ "name": tool, "arguments": args }),
        )
        .await
    };

    // —— list_my_keys：只列本人的 key ——
    let second = format!("sk-okapi-mcp2-{suffix}");
    let second_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(second.as_bytes()))
    };
    let second_id =
        okapi_store::provision::create_api_key(&env.pg, user_id, &second_hash, "sk-two")
            .await
            .unwrap();
    let listed = call(&user_token, "list_my_keys", json!({})).await;
    let keys = listed["result"]["structuredContent"]["data"]
        .as_array()
        .expect("应返回 data 数组")
        .clone();
    let ids: Vec<i64> = keys.iter().map(|k| k["id"].as_i64().unwrap()).collect();
    assert!(ids.contains(&second_id), "本人新建的 key 应在列：{listed}");
    let owned = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL"#,
        user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        i64::try_from(keys.len()).unwrap(),
        owned,
        "不多不少就是本人的 key：{listed}"
    );
    assert!(
        keys.iter().all(|k| k["key_prefix"].is_string()),
        "只回前缀，明文 key 不该出现：{listed}"
    );

    // —— list_models_pricing：只列启用模型，倍率是十进制字符串（不是浮点） ——
    let model = format!("mcp-m-{}", &suffix[..10]);
    okapi_store::provision::create_model_ratio(&env.pg, &model, "2.5", "3", "0.25")
        .await
        .unwrap();
    let priced = call(&user_token, "list_models_pricing", json!({})).await;
    let mine = priced["result"]["structuredContent"]["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == model.as_str())
        .expect("新建模型应出现在价目里");
    // 值按列精度渲染（NUMERIC::text 带足小数位），要紧的是它是**字符串**而不是 JSON 浮点：
    // 一旦漏成 number，2.5 这种还看不出来，0.1 类倍率过 JSON 就带上二进制尾巴了
    for (axis, want) in [
        ("model_ratio", "2.500000"),
        ("completion_ratio", "3.000000"),
        ("cache_ratio", "0.2500"),
    ] {
        assert_eq!(
            mine[axis].as_str(),
            Some(want),
            "{axis} 应是十进制字符串：{mine}"
        );
    }
    assert!(
        mine["per_call_price_micro"].is_null(),
        "倍率模式没有按次价：{mine}"
    );
    assert_eq!(mine["mode"], "ratio");

    // —— channel_health：管理侧工具，普通用户看不见也调不动 ——
    let tools = tool_names(&rpc(&env, &user_token, "tools/list", json!({})).await);
    for gated in ["channel_health", "platform_kpi"] {
        assert!(
            !tools.contains(&(*gated).to_owned()),
            "{gated} 不该露给普通用户"
        );
        let denied = call(&user_token, gated, json!({})).await;
        assert_eq!(denied["error"]["message"], "permission_denied", "{gated}");
    }
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("mcp-ch-{suffix}"),
        "openai",
        "http://127.0.0.1:9/v1",
        "mock",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    let health = call(&admin_token, "channel_health", json!({})).await;
    let row = health["result"]["structuredContent"]["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == channel_id)
        .expect("新建渠道应在健康表里");
    assert_eq!(row["provider"], "openai");
    assert_eq!(row["status"], 1);
    let ch_keys = row["keys"].as_array().expect("渠道下的 key 状态要一并带出");
    assert_eq!(ch_keys.len(), 1, "调度健康就看这个：{row}");
    assert_eq!(ch_keys[0]["id"], channel_key_id);
    assert_eq!(ch_keys[0]["status"], 1);
    assert!(
        ch_keys[0]["cooldown_until"].is_null(),
        "新建 key 不该在冷却"
    );

    // —— 两个走 CH 的：没接 CH 时按定案回 stats_disabled，接上了就要给出形状 ——
    let usage = call(&user_token, "query_usage", json!({ "days": 3 })).await;
    let kpi = call(&admin_token, "platform_kpi", json!({})).await;
    if std::env::var("OKAPI_CLICKHOUSE_URL").is_err() {
        assert_eq!(usage["error"]["message"], "stats_disabled");
        assert_eq!(kpi["error"]["message"], "stats_disabled");
        eprintln!("跳过 CH 断言：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    }
    let sc = &usage["result"]["structuredContent"];
    assert_eq!(sc["scope"], "key", "缺省按调用方这把 key 统计：{usage}");
    assert_eq!(sc["days"], 3, "days 应原样回显：{usage}");
    assert!(sc["data"].is_array(), "无数据也要是空数组：{usage}");
    // scope=user 换一张物化视图，同样得走通；days 超上界夹到 90
    let by_user = call(
        &user_token,
        "query_usage",
        json!({ "scope": "user", "days": 999 }),
    )
    .await;
    let sc = &by_user["result"]["structuredContent"];
    assert_eq!(sc["scope"], "user");
    assert_eq!(sc["days"], 90, "days 上界应夹到 90：{by_user}");
    assert!(sc["data"].is_array());

    let today = &kpi["result"]["structuredContent"]["today"];
    for field in ["requests", "tokens", "amount_micro", "active_users"] {
        assert!(today.get(field).is_some(), "今日 KPI 缺字段 {field}：{kpi}");
    }
}
