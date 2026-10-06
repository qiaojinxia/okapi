//! 门户第二批端点验收：/api/pricing 公开价格（无鉴权）+ /api/me/logs
//! 账单明细（含 pricing_snapshot，own 隔离）。依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

#[path = "support/key_trends.rs"]
mod key_trends;

struct TestEnv {
    pg: PgPool,
    addr: SocketAddr,
}

async fn setup() -> TestEnv {
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
    TestEnv { pg, addr }
}

async fn mk_user(pg: &PgPool) -> (i64, String) {
    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(pg, &format!("pp-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-pp-{suffix}");
    let key_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(pg, user_id, &key_hash, "sk-okapi-pp")
        .await
        .unwrap();
    (user_id, token)
}

async fn publish_fixture(pg: &PgPool) {
    let (actor, _) = mk_user(pg).await;
    let snapshot = serde_json::to_value(
        okapi_store::pricing::load_pricing_source_rows(pg)
            .await
            .unwrap(),
    )
    .unwrap();
    okapi_store::admin::publish_epoch(pg, actor, &snapshot)
        .await
        .unwrap();
}

/// 公开价格页：无鉴权可访问，含倍率与分组；不泄漏渠道信息。
#[tokio::test]
async fn public_pricing_no_auth() {
    let env = setup().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("pub-{}", &suffix[..12]);
    okapi_store::provision::create_model_ratio(&env.pg, &model, "1.25", "4", "0.5")
        .await
        .unwrap();
    sqlx::query("UPDATE models SET display_name = 'Catalog model', vendor = 'OpenAI', context_window = 128000, max_output = 4096, capabilities = $2 WHERE model_name = $1")
        .bind(&model)
        .bind(json!({"vision": true, "tools": false, "audio": "yes", "internal_note": "not public"}))
        .execute(&env.pg).await.unwrap();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    publish_fixture(&env.pg).await;
    let mut url = reqwest::Url::parse(&format!("http://{}/api/pricing", env.addr)).unwrap();
    url.query_pairs_mut().append_pair("model", &model);
    let started = std::time::Instant::now();
    let bytes = client
        .get(url.clone())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    eprintln!(
        "PRICING_DIRECTORY bytes={} elapsed_ms={}",
        bytes.len(),
        started.elapsed().as_millis()
    );
    let head = client.head(url).send().await.unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-type"], "application/json");
    if let Some(length) = head.headers().get("content-length") {
        assert_eq!(
            length.to_str().unwrap().parse::<usize>().unwrap(),
            bytes.len()
        );
    }
    assert!(head.bytes().await.unwrap().is_empty());
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    drop(bytes);
    let entry = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == model.as_str())
        .expect("已发布模型必须出现在公开价格页");
    assert_eq!(entry["model_ratio"], "1.250000");
    assert_eq!(entry["completion_ratio"], "4.000000");
    assert_eq!(entry["display_name"], "Catalog model");
    assert_eq!(entry["vendor"], "OpenAI");
    assert_eq!(entry["context_window"], 128_000);
    assert_eq!(entry["max_output"], 4096);
    assert_eq!(
        entry["capabilities"],
        json!({"vision": true, "tools": false})
    );
    assert!(entry.get("api_base").is_none(), "不得泄漏渠道信息");
    assert!(body["groups"].is_array());
}

/// 每模型可用分组（usable_group，§11.5 展示层收口）：
/// 入池模型仅指池分组可用（入池即专属）；未入池模型仅无池分组可用；
/// 零渠道模型分组为空。
// 线性场景用例：池/组/渠道三件套一次建齐再做三组断言，拆开要来回对照前置数据
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn public_pricing_reports_usable_groups() {
    let env = setup().await;
    let suffix = Uuid::new_v4().simple().to_string();
    let pooled = format!("ug-pooled-{}", &suffix[..10]);
    let open = format!("ug-open-{}", &suffix[..10]);
    let orphan = format!("ug-orphan-{}", &suffix[..10]);
    let pool = format!("ug-pool-{}", &suffix[..10]);
    let vip = format!("ug-vip-{}", &suffix[..10]);
    let free = format!("ug-free-{}", &suffix[..10]);

    for m in [&pooled, &open, &orphan] {
        okapi_store::provision::create_model_ratio(&env.pg, m, "1", "1", "1")
            .await
            .unwrap();
    }
    sqlx::query!(r#"INSERT INTO channel_pools (pool_code) VALUES ($1)"#, pool)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query!(
        r#"INSERT INTO price_groups (group_code, group_ratio, pool_code) VALUES ($1, 0.9, $2)"#,
        vip,
        pool
    )
    .execute(&env.pg)
    .await
    .unwrap();
    // free 组不指定池 → 缺省 default 池；标为可自选，价格页要透出该标记
    // This routing fixture is public; private assignments are covered by catalog_visibility.
    sqlx::query("UPDATE price_groups SET self_select=true WHERE group_code=$1")
        .bind(&vip)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query!(
        r#"INSERT INTO price_groups (group_code, group_ratio, self_select) VALUES ($1, 1, true)"#,
        free
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let (pooled_ch, _) = okapi_store::provision::create_channel(
        &env.pg,
        &format!("ug-ch-p-{suffix}"),
        "openai",
        "http://127.0.0.1:9/v1",
        "cred",
        &[pooled.as_str()],
        false,
        None,
    )
    .await
    .unwrap();
    // 只进专属池（provision 缺省进 default 池，这里覆盖成专属）
    okapi_store::admin::set_channel_pool_codes(&env.pg, pooled_ch, std::slice::from_ref(&pool))
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &env.pg,
        &format!("ug-ch-o-{suffix}"),
        "openai",
        "http://127.0.0.1:9/v1",
        "cred",
        &[open.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    publish_fixture(&env.pg).await;
    let mut pricing_url = reqwest::Url::parse(&format!("http://{}/api/pricing", env.addr)).unwrap();
    pricing_url
        .query_pairs_mut()
        .extend_pairs([("q", &suffix[..10]), ("group_q", &suffix[..10])]);
    let body: Value = reqwest::Client::new()
        .get(pricing_url.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let groups_of = |model: &str| -> Vec<String> {
        body["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["model"] == model)
            .expect("模型应在价格页")["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };

    let pooled_groups = groups_of(&pooled);
    assert!(pooled_groups.contains(&vip), "指池分组应可用池内模型");
    assert!(
        !pooled_groups.contains(&free),
        "渠道只服务它所在的池：default 池的分组不得出现在专属池模型上"
    );

    let open_groups = groups_of(&open);
    assert!(
        open_groups.contains(&free),
        "default 池的分组应可用 default 池（新渠道缺省）模型"
    );
    assert!(
        !open_groups.contains(&vip),
        "专属池分组不自动继承 default 池渠道（未配降级）"
    );

    assert!(groups_of(&orphan).is_empty(), "零渠道模型分组应为空");

    // 分组清单透出 self_select：门户据此决定哪些档位可自选
    let free_entry = body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["code"] == free)
        .expect("free 组应在清单");
    assert_eq!(free_entry["self_select"], true);

    // 专属池配降级到 default：vip 经池链可用 default 池模型
    okapi_store::admin::upsert_channel_pool(
        &env.pg,
        &pool,
        "",
        "priority_weighted",
        Some("default"),
    )
    .await
    .unwrap();
    let body: Value = reqwest::Client::new()
        .get(pricing_url.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let open_groups: Vec<String> = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == open)
        .unwrap()["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(
        open_groups.contains(&vip),
        "配了降级池后 vip 应可用 default 池模型：{open_groups:?}"
    );
}

/// 站点公告：无鉴权可读；未启用/空正文不透出；level 收敛三档、正文截断、字段白名单。
#[tokio::test]
async fn public_notice_whitelists_and_gates() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let fetch = |env: &TestEnv| {
        let url = format!("http://{}/api/notice", env.addr);
        let client = client.clone();
        async move {
            client
                .get(url)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };

    // 关闭态：即便有正文也不透出（settings 缓存 60s：每次 setup 是新 state，缓存为空）
    let long_body = "x".repeat(5_000);
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('site_notice', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!({ "enabled": false, "title": "维护", "body": long_body, "level": "warning" })
    )
    .execute(&env.pg)
    .await
    .unwrap();
    assert!(fetch(&env).await["notice"].is_null(), "未启用不得透出");

    // 启用态：新 state 再读（缓存里已是关闭态，换一个 setup 拿干净缓存）
    sqlx::query!(
        r#"UPDATE settings SET value = $1 WHERE key = 'site_notice'"#,
        json!({ "enabled": true, "title": "  维护通知 ", "body": long_body, "level": "bogus",
                "updated_at": "2026-09-01T00:00:00Z", "secret_field": "must-not-leak" })
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let env2 = setup().await;
    let body = fetch(&env2).await;
    let n = &body["notice"];
    assert_eq!(n["title"], "维护通知", "标题 trim");
    assert_eq!(n["level"], "info", "未知档位收敛为 info");
    assert_eq!(
        n["body"].as_str().unwrap().chars().count(),
        4_000,
        "正文截断到 4000 字"
    );
    assert_eq!(n["updated_at"], "2026-09-01T00:00:00Z");
    assert!(n.get("secret_field").is_none(), "只透出白名单字段：{n}");

    sqlx::query!("DELETE FROM settings WHERE key = 'site_notice'")
        .execute(&env.pg)
        .await
        .unwrap();
}

/// 账户流水：非消费动账事件按来源分类、带变动后余额；$0 的网关失败退款不入流水；
/// 他人事件不可见；充值订单含未支付态。
// 一条流水脚本：播种 → 流水断言 → 订单断言，拆函数反而割裂"同一账户的两个视图"语义
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn me_ledger_and_orders() {
    let env = setup().await;
    let (user_id, token) = mk_user(&env.pg).await;
    let (other_id, _) = mk_user(&env.pg).await;

    let refund_req = Uuid::new_v4();
    let events: [(&str, i64, &str, Value, Option<Uuid>); 6] = [
        (
            "recharge",
            5_000_000,
            "system:payment",
            json!({"tags":["recharge"]}),
            None,
        ),
        (
            "adjust",
            2_000_000,
            "system:redeem",
            json!({"tags":["redeem"]}),
            None,
        ),
        (
            "adjust",
            300_000,
            "system:aff",
            json!({"tags":["aff_rebate"]}),
            None,
        ),
        (
            "adjust",
            -100_000,
            "admin:7",
            json!({"tags":["correction"]}),
            None,
        ),
        (
            "refund",
            240,
            "admin:7",
            json!({"tags":["admin_refund"],"reason":"bad output"}),
            Some(refund_req),
        ),
        // 网关失败路径：预扣全额释放、不动账 → 不该出现在流水里
        ("refund", 0, "gateway", json!({}), Some(Uuid::new_v4())),
    ];
    let mut running = 0_i64;
    for (kind, delta, actor, payload, req) in events {
        running += delta;
        sqlx::query!(
            r#"INSERT INTO billing_events
               (user_id, request_id, event_type, delta_micro, balance_after_micro, payload, actor)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
            user_id,
            req,
            kind,
            delta,
            running,
            payload,
            actor
        )
        .execute(&env.pg)
        .await
        .unwrap();
    }
    // 他人的充值（不得可见）
    sqlx::query!(
        r#"INSERT INTO billing_events (user_id, event_type, delta_micro, payload, actor)
           VALUES ($1, 'recharge', 9_000_000, '{}', 'system:payment')"#,
        other_id
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/me/ledger", env.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 5, "五条动账事件，$0 退款与他人事件不入：{body}");
    // 倒序：最新在前
    let sources: Vec<&str> = data.iter().map(|r| r["source"].as_str().unwrap()).collect();
    assert_eq!(sources, vec!["admin", "admin", "aff", "redeem", "payment"]);
    let refund = &data[0];
    assert_eq!(refund["event_type"], "refund");
    assert_eq!(refund["delta_micro"], 240);
    assert_eq!(refund["request_id"], refund_req.to_string(), "退款锚到请求");
    assert_eq!(refund["tags"][0], "admin_refund");
    assert_eq!(
        refund["balance_after_micro"], 7_200_240,
        "变动后余额随事件带出"
    );
    assert!(
        data.iter().all(|r| !r.to_string().contains("admin:7")),
        "管理员 id 不得透出给用户：{body}"
    );

    // 充值订单：一笔已支付、一笔未支付都可见
    for (no, status, paid) in [("ord-paid", 1_i16, true), ("ord-pending", 0_i16, false)] {
        sqlx::query!(
            r#"INSERT INTO recharge_orders (order_no, user_id, amount_micro, currency, pay_amount, gateway, status, paid_at)
               VALUES ($1, $2, 5_000_000, 'CNY', 36.50, 'epay', $3, CASE WHEN $4 THEN now() ELSE NULL END)"#,
            format!("{no}-{}", Uuid::new_v4().simple()),
            user_id,
            status,
            paid
        )
        .execute(&env.pg)
        .await
        .unwrap();
    }
    let orders: Value = reqwest::Client::new()
        .get(format!("http://{}/api/me/orders", env.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = orders["data"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{orders}");
    assert_eq!(rows[0]["status"], 0, "最新的未支付单在前");
    assert!(rows[0]["paid_at"].is_null());
    assert_eq!(rows[1]["status"], 1);
    assert_eq!(
        rows[1]["pay_amount"], "36.50",
        "原币种金额按文本透出，不走浮点"
    );
    assert_eq!(rows[1]["currency"], "CNY");

    // 无鉴权 401
    for path in ["/api/me/ledger", "/api/me/orders"] {
        let resp = reqwest::Client::new()
            .get(format!("http://{}{path}", env.addr))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "{path}");
    }
}

/// 用量日志：own 隔离 + snapshot 透出（账单解释器数据源）。
///
/// 本用例的记录用占位 api_key_id=1 写入，故走 `scope=user`（钱包主体维度）——
/// 验证的是**跨用户**隔离；key 级缺省隔离（员工只见自己那把 key）在
/// console_portal 端到端用例里用真实 key id 断言。
#[tokio::test]
async fn me_logs_with_snapshot_own_scope() {
    let env = setup().await;
    let (user_id, token) = mk_user(&env.pg).await;
    let (other_id, other_token) = mk_user(&env.pg).await;

    let request_id = Uuid::new_v4();
    sqlx::query!(
        r#"INSERT INTO billing_records
           (request_id, log_type, user_id, api_key_id, group_code, model_name, status,
            prompt_tokens, cached_tokens, completion_tokens, amount_micro,
            original_amount_micro, discount_micro, pricing_snapshot)
           VALUES ($1, 2, $2, 1, 'default', 'm-logs', 20, 100, 40, 20, 200, 240, 40,
                   '{"mode":"ratio","model_ratio":"1","group":"default","group_ratio":"1","user_multiplier":"1","rules":[{"code":"night","kind":"time","multiplier":"0.8"}]}')"#,
        request_id,
        user_id
    )
    .execute(&env.pg)
    .await
    .unwrap();
    // 另一个用户的记录（不得可见）
    sqlx::query!(
        r#"INSERT INTO billing_records
           (request_id, log_type, user_id, api_key_id, group_code, model_name, status,
            prompt_tokens, completion_tokens, amount_micro, original_amount_micro)
           VALUES ($1, 2, $2, 1, 'default', 'm-other', 20, 1, 1, 1, 1)"#,
        Uuid::new_v4(),
        other_id
    )
    .execute(&env.pg)
    .await
    .unwrap();

    let body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/me/logs?scope=user", env.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 1, "只见自己的记录");
    let row = &data[0];
    assert_eq!(row["model"], "m-logs");
    assert_eq!(row["amount_micro"], 200);
    assert_eq!(row["original_amount_micro"], 240);
    assert_eq!(row["discount_micro"], 40);
    assert_eq!(row["usage"]["cached_tokens"], 40);
    assert_eq!(row["pricing_snapshot"]["rules"][0]["code"], "night");
    assert_eq!(
        row["pricing_snapshot"]["rules"][0]["multiplier"], "0.8",
        "夜间折扣必须在快照里可解释（DESIGN §3 账单可解释性）"
    );

    let other_body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/me/logs?scope=user", env.addr))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        other_body["data"].as_array().unwrap().len(),
        1,
        "对方同样只见自己"
    );
    assert_eq!(other_body["data"][0]["model"], "m-other");

    // 无鉴权 401
    let unauthorized = reqwest::Client::new()
        .get(format!("http://{}/api/me/logs", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    let _ = json!({});
}

/// 日期下钻按账本时间过滤，DST 的 25 小时自然日、游标与 own/key 隔离同时成立。
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn me_logs_calendar_range_preserves_scope_and_cursor() {
    let env = setup().await;
    let (user_id, token) = mk_user(&env.pg).await;
    let (other_id, _) = mk_user(&env.pg).await;
    let key_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    let second_key = okapi_store::provision::create_api_key(
        &env.pg,
        user_id,
        &Uuid::new_v4().simple().to_string(),
        "sk-range",
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (owner, key, model, status, time) in [
        (
            user_id,
            key_id,
            "range-model",
            20_i16,
            "2024-11-03T06:59:59Z",
        ),
        (user_id, key_id, "range-model", 20, "2024-11-03T07:00:00Z"),
        (
            user_id,
            key_id,
            "range-model",
            30,
            "2024-11-04T07:59:59.999999Z",
        ),
        (user_id, key_id, "range-model", 20, "2024-11-04T08:00:00Z"),
        (
            user_id,
            second_key,
            "range-model",
            20,
            "2024-11-03T10:00:00Z",
        ),
        (user_id, key_id, "another-model", 20, "2024-11-03T10:00:00Z"),
        (other_id, key_id, "range-model", 20, "2024-11-03T10:00:00Z"),
    ] {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO billing_records (request_id, log_type, user_id, api_key_id, group_code, model_name, status, created_at) \
             VALUES ($1, 2, $2, $3, 'default', $4, $5, $6) RETURNING id",
        )
        .bind(Uuid::new_v4()).bind(owner).bind(key).bind(model).bind(status)
        .bind(chrono::DateTime::parse_from_rfc3339(time).unwrap())
        .fetch_one(&env.pg).await.unwrap();
        ids.push(id);
    }
    let client = reqwest::Client::new();
    let get = |query: String| {
        let req = client
            .get(format!("http://{}/api/me/logs?{query}", env.addr))
            .bearer_auth(&token);
        async move {
            let res = req.send().await.unwrap();
            assert_eq!(res.status(), 200);
            res.json::<Value>().await.unwrap()
        }
    };
    let base = "model=range-model&start_date=2024-11-03&end_date=2024-11-03&timezone=America%2FLos_Angeles";
    let body = get(base.to_owned()).await;
    let row_ids = |v: &Value| {
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_i64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        row_ids(&body),
        [ids[2], ids[1]],
        "必须含首尾边界内的记录，排除第二把 key 和其他用户"
    );
    assert_eq!(body["window"]["timezone"], "America/Los_Angeles");
    assert_eq!(body["window"]["end_date"], "2024-11-03");
    let first = get(format!("{base}&limit=1")).await;
    assert_eq!(row_ids(&first), [ids[2]]);
    let next = get(format!("{base}&limit=1&before={}", first["next_before"])).await;
    assert_eq!(row_ids(&next), [ids[1]]);
    assert!(row_ids(&get(format!("{base}&before={}", ids[1])).await).is_empty());
    assert!(
        row_ids(&get(format!("{base}&errors_only=true")).await).is_empty(),
        "refunded records are not failed requests"
    );
    assert_eq!(
        row_ids(&get(format!("{base}&scope=user")).await),
        [ids[4], ids[2], ids[1]]
    );
    let utc = get("model=range-model&start_date=2024-11-03&end_date=2024-11-03".to_owned()).await;
    assert_eq!(
        row_ids(&utc),
        [ids[1], ids[0]],
        "缺省 UTC，不能把次日当地记录混入"
    );
    assert_eq!(utc["window"]["timezone"], "UTC");
    let all = get("model=range-model".to_owned()).await;
    assert_eq!(
        row_ids(&all),
        [ids[3], ids[2], ids[1], ids[0]],
        "清除日期恢复全部日期，隔离仍有效"
    );
    assert!(all["window"].is_null());
}

#[tokio::test]
async fn me_logs_rejects_invalid_calendar_ranges() {
    let env = setup().await;
    let (_, token) = mk_user(&env.pg).await;
    let client = reqwest::Client::new();
    for query in [
        "start_date=2024-01-01",
        "end_date=2024-01-01",
        "start_date=2024-02-30&end_date=2024-03-01",
        "start_date=2024-01-02&end_date=2024-01-01",
        "start_date=2023-01-01&end_date=2024-01-02",
        "start_date=2148-01-01&end_date=2148-01-01",
        "start_date=2024-01-01&end_date=2024-01-01&timezone=not-a-timezone",
        "start_date=2024-01-01&end_date=2024-01-01&timezone=UTC%27%3BSELECT%201",
    ] {
        let res = client
            .get(format!("http://{}/api/me/logs?{query}", env.addr))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400, "{query}");
    }
}

/// Summary and list use identical owner/model/key/request/date predicates, but summary
/// ignores paging. Refunded billing states must never count as failed calls or net spend.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn me_logs_summary_and_usage_details_are_owned_and_page_independent() {
    async fn get(env: &TestEnv, token: &str, path: &str) -> Value {
        let response = reqwest::Client::new()
            .get(format!("http://{}{path}", env.addr))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
        response.json().await.unwrap()
    }
    let env = setup().await;
    let (user_id, token) = mk_user(&env.pg).await;
    let (other_id, _) = mk_user(&env.pg).await;
    let key_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id=$1")
        .bind(user_id)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    let request_id = Uuid::new_v4();
    for i in 0..51 {
        sqlx::query("INSERT INTO billing_records (request_id, user_id, api_key_id, model_name, status, prompt_tokens, cached_tokens, completion_tokens, amount_micro, latency_ms, ttft_ms, is_stream, usage_details) VALUES ($1,$2,$3,'summary-model',20,1000,$4,100,100,2000,100,true,$5)")
            .bind(if i == 0 { request_id } else { Uuid::new_v4() }).bind(user_id).bind(key_id)
            .bind(if i == 50 { 0 } else { 100_i32 })
            .bind(if i == 50 { None } else { Some(json!({"tokens": {"cache_read_reported": true, "cache_write_reported": true, "cache_write_tokens": if i == 0 {20} else {0}, "cache_write_5m_tokens": if i == 0 {Some(12)} else {None}, "cache_write_1h_tokens": if i == 0 {Some(8)} else {None}, "audio_prompt_tokens": 0, "image_prompt_tokens": if i == 0 {25} else {0}, "audio_completion_tokens": 0, "image_completion_tokens": if i == 0 {Some(10)} else {None}}, "endpoint":"/v1/responses", "requested_model":"alias-model"})) })
            .execute(&env.pg).await.unwrap();
    }
    for (owner, status, amount) in [
        (user_id, 30_i16, 200_i64),
        (user_id, 40, 0),
        (other_id, 20, 999_999),
    ] {
        sqlx::query("INSERT INTO billing_records (request_id,user_id,api_key_id,model_name,status,amount_micro) VALUES ($1,$2,$3,'summary-model',$4,$5)")
            .bind(Uuid::new_v4()).bind(owner).bind(key_id).bind(status).bind(amount)
            .execute(&env.pg).await.unwrap();
    }
    let stat = get(
        &env,
        &token,
        "/api/me/logs/stat?scope=user&model=summary-model&limit=1&before=1",
    )
    .await;
    assert_eq!(stat["records"], 53);
    assert_eq!(stat["settled"], 51);
    assert_eq!(stat["failed"], 1);
    assert_eq!(stat["refunded"], 1);
    assert_eq!(stat["amount_micro"], 5100);
    assert_eq!(stat["refunded_amount_micro"], 200);
    assert_eq!(stat["prompt_tokens"], 51000);
    assert_eq!(stat["completion_tokens"], 5100);
    assert_eq!(stat["cache_read_samples"], 50);
    assert_eq!(stat["cache_write_tokens"], 20);
    assert_eq!(stat["cache_write_samples"], 50);
    assert_eq!(stat["cache_write_5m_tokens"], 12);
    assert_eq!(stat["cache_write_1h_tokens"], 8);
    assert_eq!(stat["cache_write_ttl_samples"], 1);
    assert_eq!(stat["image_prompt_tokens"], 25);
    assert_eq!(stat["image_prompt_samples"], 50);
    assert_eq!(stat["image_completion_tokens"], 10);
    assert_eq!(stat["image_completion_samples"], 1);
    assert!(stat["cache_read_audio_tokens"].is_null());
    assert_eq!(stat["cache_read_modal_samples"], 0);
    assert_eq!(stat["avg_ttft_ms"], 100);
    assert_eq!(stat["ttft_samples"], 51);
    let one = get(
        &env,
        &token,
        &format!("/api/me/logs?request_id={request_id}"),
    )
    .await;
    assert_eq!(one["data"].as_array().unwrap().len(), 1);
    assert!(one["next_before"].is_null());
    let row = &one["data"][0];
    assert_eq!(row["requested_model"], "alias-model");
    assert_eq!(row["endpoint"], "/v1/responses");
    assert_eq!(row["usage"]["cache_write_tokens"], 20);
    assert_eq!(row["usage"]["cache_write_5m_tokens"], 12);
    assert_eq!(row["usage"]["cache_write_1h_tokens"], 8);
    assert_eq!(row["usage"]["image_completion_tokens"], 10);
    assert_eq!(row["usage"]["cache_write_reported"], true);
    assert_eq!(row["usage_details_recorded"], true);
    for field in [
        "channel_id",
        "channel_key_id",
        "upstream_cost_micro",
        "client_ip",
        "upstream_model",
    ] {
        assert!(row.get(field).is_none(), "must not expose {field}");
    }
    let first = get(&env, &token, "/api/me/logs?limit=3").await;
    assert_eq!(first["data"].as_array().unwrap().len(), 3);
    assert!(first["next_before"].is_number());
    let old = &first["data"][2];
    assert_eq!(old["usage_details_recorded"], false);
    assert!(old["usage"]["cache_write_tokens"].is_null());
    assert!(old["usage"]["cache_read_reported"].is_null());
    assert_eq!(first["data"][1]["status"], 30);
    assert_eq!(first["data"][1]["net_amount_micro"], 0);
    let failed = get(&env, &token, "/api/me/logs?errors_only=true").await;
    assert_eq!(failed["data"].as_array().unwrap().len(), 1);
    assert_eq!(failed["data"][0]["status"], 40);
    let failed_stat = get(&env, &token, "/api/me/logs/stat?errors_only=true").await;
    assert_eq!(failed_stat["records"], 1);
    assert_eq!(failed_stat["amount_micro"], 0);
    assert!(failed_stat["avg_ttft_ms"].is_null());
    for query in [
        "model=absent".to_owned(),
        format!("api_key_id={}", key_id + 100_000),
        "start_date=2020-01-01&end_date=2020-01-01".to_owned(),
    ] {
        let empty = get(
            &env,
            &token,
            &format!("/api/me/logs/stat?scope=user&{query}"),
        )
        .await;
        assert_eq!(empty["records"], 0);
        assert!(empty["avg_latency_ms"].is_null());
    }
    let single_stat = get(
        &env,
        &token,
        &format!("/api/me/logs/stat?request_id={request_id}"),
    )
    .await;
    assert_eq!(single_stat["records"], 1);
    for path in ["/api/me/logs", "/api/me/logs/stat"] {
        let unauthorized = reqwest::Client::new()
            .get(format!("http://{}{path}", env.addr))
            .send()
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), 401);
        for invalid in ["api_key_id=-1", "request_id=not-a-uuid"] {
            let response = reqwest::Client::new()
                .get(format!("http://{}{path}?{invalid}", env.addr))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 400);
        }
    }
}

/// Request failure and billing state are independent; personal diagnostics stay owner-scoped.
#[tokio::test]
async fn failed_refunds_and_charged_stream_errors_are_visible_without_exposing_routing() {
    let env = setup().await;
    let (owner, token) = mk_user(&env.pg).await;
    let (other, _) = mk_user(&env.pg).await;
    let key: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id=$1")
        .bind(owner)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    let diagnostics = json!({"request_failed":true,"error_phase":"upstream","error_message":"quota exceeded","response_model":"actual-model","attempts":[{"channel_key_id":99}],"session_id":"private","user_agent":"sdk"});
    for (user, status, log_type, details) in [
        (
            owner,
            30_i16,
            5_i16,
            Some(json!({"diagnostics":diagnostics})),
        ),
        (owner, 30, 2, None),
        (owner, 20, 2, Some(json!({"diagnostics":diagnostics}))),
        (other, 30, 5, Some(json!({"diagnostics":diagnostics}))),
    ] {
        sqlx::query("INSERT INTO billing_records(request_id,user_id,api_key_id,model_name,status,log_type,amount_micro,usage_details) VALUES ($1,$2,$3,'failed-model',$4,$5,100,$6)")
            .bind(Uuid::new_v4()).bind(user).bind(key).bind(status).bind(log_type).bind(details)
            .execute(&env.pg).await.unwrap();
    }
    let client = reqwest::Client::new();
    let rows: Value = client
        .get(format!("http://{}/api/me/logs?errors_only=true", env.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = rows["data"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r["status"] == 30));
    assert!(rows.iter().any(|r| r["status"] == 20));
    for row in rows {
        assert_eq!(row["is_error"], true);
        assert_eq!(row["diagnostics"]["error_message"], "quota exceeded");
        assert_eq!(row["diagnostics"]["response_model"], "actual-model");
        for private in ["attempts", "session_id", "user_agent"] {
            assert!(row["diagnostics"].get(private).is_none());
        }
    }
    let stat: Value = client
        .get(format!(
            "http://{}/api/me/logs/stat?errors_only=true",
            env.addr
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stat["records"], 2);
    assert_eq!(stat["errors"], 2);
    assert_eq!(stat["failed"], 0, "financial failures remain separate");
    assert_eq!(stat["settled"], 1);
    assert_eq!(stat["refunded"], 1);
    assert_eq!(stat["amount_micro"], 100);
}
