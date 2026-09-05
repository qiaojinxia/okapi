//! M2 运维批验收：按日志退款（事件溯源冲销 + CH 口径一致）/ 代客查看 / 缓存清理。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::worker::chsink;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

async fn mock_ok(_body: axum::body::Bytes) -> axum::response::Response {
    use std::fmt::Write as _;
    let chunks = [
        json!({"choices":[{"index":0,"delta":{"role":"assistant"}}]}),
        json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,
            "prompt_tokens_details":{"cached_tokens":0}}}),
    ];
    let mut body = String::new();
    for c in chunks {
        let _ = write!(body, "data: {c}\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

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
    state: gateway::state::AppState,
    console: SocketAddr,
    gateway: SocketAddr,
    model: String,
    super_token: String,
    user_id: i64,
    user_token: String,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("m-ops-{}", &suffix[..10]);

    // super_admin 与普通用户
    let super_id = okapi_store::provision::create_user(&pg, &format!("os-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-ops-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-ops-s")
        .await
        .unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("ou-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-ops-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-ops-u")
        .await
        .unwrap();

    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let mock = serve(Router::new().route("/ok/v1/chat/completions", post(mock_ok))).await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("ops-{suffix}"),
        "openai",
        &format!("http://{mock}/ok/v1"),
        "mock",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_000_000))
        .await
        .unwrap();
    okapi_ledger::pg::record_credit(
        &pg,
        user_id,
        Money::from_micros(1_000_000),
        "recharge",
        "test",
        json!({}),
    )
    .await
    .unwrap();

    let console = serve(console::router(state.clone())).await;
    let gw = serve(gateway::router(state.clone())).await;
    Env {
        pg,
        state,
        console,
        gateway: gw,
        model,
        super_token,
        user_id,
        user_token,
    }
}

async fn chat_settled(env: &Env) -> (Uuid, i64) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({
            "model": env.model, "stream": true, "max_tokens": 32,
            "messages": [{"role":"user","content":"hello ops"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let request_id = resp
        .headers()
        .get("x-okapi-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v).ok())
        .unwrap();
    let _ = resp.text().await.unwrap();
    for _ in 0..50 {
        if let Some(r) = sqlx::query!(
            r#"SELECT amount_micro FROM billing_records WHERE request_id = $1 AND status = 20"#,
            request_id
        )
        .fetch_optional(&env.pg)
        .await
        .unwrap()
        {
            return (request_id, r.amount_micro);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("结算未出现");
}

/// 退款闭环：查账预览、余额回补、状态翻转、事件留痕、CH 负额冲销、幂等。
// 线性生命周期用例：查→退→复退→复查一气呵成，拆开就丢了状态推进的因果
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn admin_refund_full_cycle() {
    let env = setup().await;
    let (request_id, amount) = chat_settled(&env).await;
    assert_eq!(amount, 240);
    let balance_before = env.state.ledger.balance(env.user_id).await.unwrap();

    // 退款前查账预览（先查后退的第一步）：能看到是谁的哪笔、可退
    let preview: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/billing/record/{request_id}",
            env.console
        ))
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["amount_micro"], 240);
    assert_eq!(preview["user_id"], env.user_id);
    assert_eq!(preview["refundable"], true, "committed 记录应可退");
    assert!(preview["username"].is_string(), "预览应带用户名");

    let r = reqwest::Client::new()
        .post(format!("http://{}/admin/billing/refund", env.console))
        .bearer_auth(&env.super_token)
        .json(&json!({"request_id": request_id, "reason": "客诉补偿"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["outcome"], "refunded");
    assert_eq!(body["refunded_micro"], 240);

    // 余额回补 + 状态翻转 + 事件留痕
    let balance_after = env.state.ledger.balance(env.user_id).await.unwrap();
    assert_eq!(balance_after.as_micros() - balance_before.as_micros(), 240);
    let status = sqlx::query_scalar!(
        r#"SELECT status FROM billing_records WHERE request_id = $1"#,
        request_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(status, 30, "committed → refunded");
    let events = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_events
           WHERE request_id = $1 AND event_type = 'refund' AND delta_micro = 240"#,
        request_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(events, 1);

    // 幂等：再退一次 → 200 already_refunded（此前与"id 不存在"混在 404 里，
    // 管理员分不清"打错了"还是"重复点了但安全"）
    let r = reqwest::Client::new()
        .post(format!("http://{}/admin/billing/refund", env.console))
        .bearer_auth(&env.super_token)
        .json(&json!({"request_id": request_id, "reason": "again"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "重复退款是幂等语义而非错误");
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["outcome"], "already_refunded");

    // 退款后查账：状态翻转、不可再退
    let preview: Value = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/billing/record/{request_id}",
            env.console
        ))
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(preview["status"], 30);
    assert_eq!(preview["refundable"], false);

    // 不存在的 id：查账与退款都 404
    let ghost = Uuid::new_v4();
    for (method, path) in [
        ("GET", format!("/admin/billing/record/{ghost}")),
        ("POST", "/admin/billing/refund".to_owned()),
    ] {
        let client = reqwest::Client::new();
        let req = if method == "GET" {
            client.get(format!("http://{}{path}", env.console))
        } else {
            client
                .post(format!("http://{}{path}", env.console))
                .json(&json!({"request_id": ghost, "reason": "ghost"}))
        };
        let r = req.bearer_auth(&env.super_token).send().await.unwrap();
        assert_eq!(r.status(), 404, "{method} {path} 对不存在 id 应 404");
    }

    // CH 冲销：drain 后该用户金额聚合归零（240 + (-240)）。
    // outbox 是全局队列，同二进制内并行用例的 chsink 会抢走本用例的行，
    // 因此轮询"有行且和为零"——空结果集的和也是 0，不能只断言总和。
    if let Some(ch) = &env.state.ch {
        ch.ensure_schema().await.unwrap();
        let mut seen: Option<i64> = None;
        for _ in 0..50 {
            let _ = chsink::process_once(&env.pg, ch).await.unwrap();
            let rows = ch
                .query_json_each_row(&format!(
                    "SELECT sumMerge(amount) AS a FROM mv_user_day WHERE user_id = {} GROUP BY user_id, day",
                    env.user_id
                ))
                .await
                .unwrap();
            if rows.is_empty() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            let total: i64 = rows
                .iter()
                .map(|r| {
                    r.get("a").map_or(0, |v| {
                        v.as_str()
                            .map_or_else(|| v.as_i64(), |s| s.parse().ok())
                            .unwrap_or(0)
                    })
                })
                .sum();
            seen = Some(total);
            if total == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(seen, Some(0), "CH 口径应被负额行冲平");
    }

    // 对账干净（消费-240 + 退款+240 + 充值 1_000_000 = redis 余额）
    let drifts = okapi::worker::reconcile_balances(&env.pg, &env.state.ledger, 1_000_000)
        .await
        .unwrap();
    assert!(
        !drifts.iter().any(|d| d.user_id == env.user_id),
        "退款后三方对账必须干净"
    );
}

/// 代客查看：user.assist 权限可读用户概览且留审计；无权限 403。
#[tokio::test]
async fn assist_overview_scoped_and_audited() {
    let env = setup().await;
    // 只有 user.assist 的客服角色
    let code = format!("support-{}", Uuid::new_v4().simple());
    let r = reqwest::Client::new()
        .post(format!("http://{}/admin/roles", env.console))
        .bearer_auth(&env.super_token)
        .json(&json!({"role_code": code, "display_name": "客服", "permissions": ["user.assist"]}))
        .send()
        .await
        .unwrap();
    let role_id = r.json::<Value>().await.unwrap()["admin_role_id"]
        .as_i64()
        .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let support_id = okapi_store::provision::create_user(&env.pg, &format!("sp-{suffix}"))
        .await
        .unwrap();
    sqlx::query!(
        "UPDATE users SET role = 10, admin_role_id = $2 WHERE id = $1",
        support_id,
        role_id
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let support_token = format!("sk-okapi-sp-{suffix}");
    okapi_store::provision::create_api_key(&env.pg, support_id, &hash(&support_token), "sk-sp")
        .await
        .unwrap();

    // 客服可读概览
    let r = reqwest::Client::new()
        .get(format!(
            "http://{}/admin/users/{}/overview",
            env.console, env.user_id
        ))
        .bearer_auth(&support_token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["user"]["id"], env.user_id);
    assert!(body["keys"].as_array().is_some_and(|k| !k.is_empty()));

    // 但不能做渠道写（403）
    let r = reqwest::Client::new()
        .get(format!("http://{}/admin/channels", env.console))
        .bearer_auth(&support_token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // 审计留痕（谁看了谁）
    let audits = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs
           WHERE actor = $1 AND action = 'user.assist.view' AND target = $2"#,
        format!("admin:{support_id}"),
        env.user_id.to_string()
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(audits, 1, "代客查看必须留痕");
}

/// 缓存清理：直改 DB 倍率（不发 epoch）→ flush pricebook → 新价立即生效。
#[tokio::test]
async fn cache_flush_pricebook_hotfix() {
    let env = setup().await;
    let (_, amount) = chat_settled(&env).await;
    assert_eq!(amount, 240);

    // 直接改库（模拟热修复场景，不走 publish）
    sqlx::query!(
        r#"UPDATE model_pricing SET model_ratio = 3
           WHERE model_id = (SELECT id FROM models WHERE model_name = $1)"#,
        env.model
    )
    .execute(&env.pg)
    .await
    .unwrap();

    // flush 前旧价仍生效
    let (_, amount) = chat_settled(&env).await;
    assert_eq!(amount, 240, "未 flush 前应仍是旧价");

    let r = reqwest::Client::new()
        .post(format!("http://{}/admin/cache/flush", env.console))
        .bearer_auth(&env.super_token)
        .json(&json!({"scope": "pricebook"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let (_, amount) = chat_settled(&env).await;
    assert_eq!(amount, 720, "flush 后新倍率立即生效（240×3）");
}

/// M4 运维界面支撑端点：settings 单键读取回显 + 用户消耗排行（CH）。
#[tokio::test]
// 回显→权限→灌数→轮询上榜的一体时序脚本
#[allow(clippy::too_many_lines)]
async fn settings_get_and_leaderboard() {
    let env = setup().await;
    let client = reqwest::Client::new();

    // settings 读写回显
    let key = format!("ops-test-{}", Uuid::new_v4().simple());
    let w = client
        .post(format!("http://{}/admin/settings", env.console))
        .bearer_auth(&env.super_token)
        .json(&json!({"key": key, "value": {"nested": 42}}))
        .send()
        .await
        .unwrap();
    assert_eq!(w.status(), 200);
    let r: Value = client
        .get(format!("http://{}/admin/settings/{key}", env.console))
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["value"]["nested"], 42, "读回应与写入一致：{r}");
    // 未配置键：value=null
    let empty: Value = client
        .get(format!(
            "http://{}/admin/settings/never-configured-{key}",
            env.console
        ))
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(empty["value"].is_null());
    // 普通用户无权
    let denied = client
        .get(format!("http://{}/admin/settings/{key}", env.console))
        .bearer_auth(&env.user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    sqlx::query!("DELETE FROM settings WHERE key = $1", key)
        .execute(&env.pg)
        .await
        .unwrap();

    // 排行榜：灌一笔大额结算（共享 CH 有海量历史测试用户，小额挤不进榜）
    let big = okapi_ledger::SettlementInput {
        dimensions: Default::default(),
        request_id: Uuid::new_v4(),
        log_type: 2,
        user_id: env.user_id,
        api_key_id: 0,
        group_code: "default",
        model_name: &env.model,
        channel_id: None,
        channel_key_id: None,
        state: okapi_domain::BillingState::Committed,
        usage: okapi_domain::TokenUsage::default(),
        amount: Money::from_micros(77_000_000),
        original: Money::from_micros(77_000_000),
        discount: Money::ZERO,
        list_price: Money::ZERO,
        upstream_cost: None,
        pricing_epoch: None,
        pricing_snapshot: None,
        latency_ms: 1,
        ttft_ms: None,
        is_stream: false,
        retry_count: 0,
        failover_count: 0,
        upstream_status: Some(200),
        error_code: None,
        upstream_request_id: None,
        node: "test-node",
        sticky_layer: 0,
        client_type: "test",
        client_ip: None,
        delta_micro: -77_000_000,
        balance_after: None,
        event_type: "commit",
    };
    okapi_ledger::record_settlement(&env.pg, big).await.unwrap();
    if let Some(ch) = env.state.ch.as_ref() {
        for _ in 0..50 {
            if chsink::process_once(&env.pg, ch).await.unwrap() == 0 {
                break;
            }
        }
        // CH 异步合并：轮询直到本用户上榜
        let mut found = false;
        for _ in 0..30 {
            let board: Value = client
                .get(format!(
                    "http://{}/admin/leaderboard?days=7&limit=100",
                    env.console
                ))
                .bearer_auth(&env.super_token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let rows = board["data"].as_array().unwrap();
            found = rows.iter().any(|r| {
                let uid = r["user_id"]
                    .as_str()
                    .map_or_else(|| r["user_id"].as_i64(), |s| s.parse().ok())
                    .unwrap_or(0);
                uid == env.user_id && !r["username"].as_str().unwrap_or("").is_empty()
            });
            if found {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(found, "消费用户应出现在排行榜且带用户名");
    }
}
