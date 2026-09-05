//! 门户第二批端点验收：/api/pricing 公开价格（无鉴权）+ /api/me/logs
//! 账单明细（含 pricing_snapshot，own 隔离）。依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
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

    let body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/pricing", env.addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == model.as_str())
        .expect("新模型必须出现在公开价格页");
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

    let body: Value = reqwest::Client::new()
        .get(format!("http://{}/api/pricing", env.addr))
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
        .get(format!("http://{}/api/pricing", env.addr))
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
