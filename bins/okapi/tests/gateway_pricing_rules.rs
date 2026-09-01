//! 定价规则栈验收（DESIGN §3.4）：console CRUD 控制面 + volume 规则运行期真实命中。
//!
//! 本套件钉死的正是补齐前的缺陷形状：`pricing_rules` 只有 SELECT 没有写入口，
//! 且 `monthly_tokens` 恒为 0 —— 规则配得上却永不命中。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

/// 固定 usage：100 prompt + 20 completion，倍率 1.0 下标价 240 micro。
async fn mock_chat(_body: axum::body::Bytes) -> axum::response::Response {
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion","model":"m",
        "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
        "usage":{"prompt_tokens":100,"completion_tokens":20,
                 "prompt_tokens_details":{"cached_tokens":0}}
    }))
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
    gateway: SocketAddr,
    console: SocketAddr,
    token: String,
    super_token: String,
    user_id: i64,
    model: String,
    suffix: String,
}

/// 规则在 build_state 之前入库才会进价簿——本函数由调用方在建 state 前调用。
async fn insert_volume_rule(pg: &PgPool, code: &str, user_id: i64, model: &str, threshold: u64) {
    okapi_store::admin::upsert_pricing_rule(
        pg,
        okapi_store::admin::PricingRuleInput {
            rule_code: code,
            rule_type: "volume",
            // 必须限定到本用例的用户与模型：pricing_rules 是全局表，
            // 无作用域的规则会污染并行跑的其它计费用例
            scope: &json!({ "users": [user_id], "models": [model] }),
            params: &json!({ "multiplier": "0.5", "min_monthly_tokens": threshold }),
            priority: 0,
            enabled: true,
            valid_from: None,
            valid_to: None,
        },
    )
    .await
    .unwrap();
}

async fn setup(with_volume_rule: Option<u64>) -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("rule-{}", &suffix[..12]);

    let user_id = okapi_store::provision::create_user(&pg, &format!("ru-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-rule-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-rule-u")
        .await
        .unwrap();
    let super_id = okapi_store::provision::create_user(&pg, &format!("rs-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-rule-s-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-rule-s")
        .await
        .unwrap();

    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_chat))).await;
    okapi_store::provision::create_channel(
        &pg,
        &format!("rule-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "mock",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    if let Some(threshold) = with_volume_rule {
        insert_volume_rule(&pg, &format!("vol-{suffix}"), user_id, &model, threshold).await;
    }

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(50_000_000))
        .await
        .unwrap();
    let gw = serve(gateway::router(state.clone())).await;
    let console_addr = serve(console::router(state)).await;

    Env {
        pg,
        gateway: gw,
        console: console_addr,
        token,
        super_token,
        user_id,
        model,
        suffix,
    }
}

async fn chat(env: &Env) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", env.gateway))
        .bearer_auth(&env.token)
        .json(&json!({ "model": env.model, "messages": [{"role":"user","content":"hi"}] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.bytes().await.unwrap();
}

/// 等到第 n 笔 committed 记录（按时间序），返回金额与快照。
async fn wait_committed_nth(pg: &PgPool, user_id: i64, n: usize) -> (i64, Value) {
    for _ in 0..80 {
        let rows = sqlx::query!(
            r#"SELECT amount_micro, pricing_snapshot FROM billing_records
               WHERE user_id = $1 AND status = 20 ORDER BY created_at, id"#,
            user_id
        )
        .fetch_all(pg)
        .await
        .unwrap();
        if rows.len() >= n {
            let row = &rows[n - 1];
            return (
                row.amount_micro,
                row.pricing_snapshot.clone().unwrap_or(Value::Null),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("未等到第 {n} 笔 committed 记录");
}

/// 端到端：首笔不打折并把 token 记进本月计数，次笔越过阈值即按 0.5 计价。
/// 这条链路任一环断开（计数没写 / 报价没读 / 规则没进价簿）本用例都会失败。
#[tokio::test]
async fn volume_rule_fires_once_monthly_tokens_cross_threshold() {
    let env = setup(Some(100)).await;

    chat(&env).await;
    let (first, snap1) = wait_committed_nth(&env.pg, env.user_id, 1).await;
    assert_eq!(first, 240, "首笔时本月计数为 0，规则不应命中");
    assert_eq!(
        snap1["rules"].as_array().map_or(0, Vec::len),
        0,
        "未命中的规则不得进快照：{snap1}"
    );

    chat(&env).await;
    let (second, snap2) = wait_committed_nth(&env.pg, env.user_id, 2).await;
    assert_eq!(second, 120, "首笔已累计 120 token ≥ 阈值 100 → 0.5 倍率");
    let rules = snap2["rules"].as_array().expect("命中规则必须进快照");
    assert_eq!(rules.len(), 1, "应恰好命中一条 volume 规则：{snap2}");
    assert_eq!(rules[0]["type"], "volume");
    assert_eq!(rules[0]["code"], format!("vol-{}", env.suffix));

    // 计数键按用户隔离，取值可精确断言：两笔各 120 token
    // （门控本身"无规则不采集"由 okapi-pricing 的 compile 单测覆盖——
    //   pricing_rules 是全局表，并行用例插入的规则会翻转进程级门控，
    //   在集成层断言"键不存在"必然不稳定）
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let client = okapi_store::connect_redis(&redis_url).await.unwrap();
    let key = format!(
        "tok:{{{}}}:{}",
        env.user_id,
        chrono::Utc::now().format("%Y%m")
    );
    let counted: Option<String> = fred::interfaces::KeysInterface::get(&client, &key)
        .await
        .unwrap();
    assert_eq!(
        counted.as_deref(),
        Some("240"),
        "结算应把两笔各 120 token 累加进 {key}"
    );
}

/// 控制面：CRUD 打通 + 非法组合在配置期就被拒（而非入库后静默失效）。
#[tokio::test]
async fn console_rule_crud_and_validation() {
    let env = setup(None).await;
    let client = reqwest::Client::new();
    let url = format!("http://{}/admin/pricing/rules", env.console);
    let code = format!("t-{}", &env.suffix[..12]);

    // volume 缺阈值 → 400（补齐前这类规则会入库并永不命中）
    let resp = client
        .post(&url)
        .bearer_auth(&env.super_token)
        .json(&json!({ "rule_code": code, "rule_type": "volume", "multiplier": "0.8" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        400,
        "volume 规则缺 min_monthly_tokens 必须拒绝"
    );

    // start == end 空窗 → 400（对齐 new-api rc.27 #6934 的反面：不静默退化为全天）
    let resp = client
        .post(&url)
        .bearer_auth(&env.super_token)
        .json(&json!({ "rule_code": code, "rule_type": "time_based",
                       "multiplier": "0.8", "start_minute": 600, "end_minute": 600 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "空窗规则必须拒绝");

    // 非法倍率字面量 → 400
    let resp = client
        .post(&url)
        .bearer_auth(&env.super_token)
        .json(&json!({ "rule_code": code, "rule_type": "discount", "multiplier": "abc" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // 合法 time_based 落库
    let resp = client
        .post(&url)
        .bearer_auth(&env.super_token)
        .json(
            &json!({ "rule_code": code, "rule_type": "time_based", "multiplier": "0.8",
                       "start_minute": 1320, "end_minute": 360,
                       "scope": { "models": [env.model] } }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let listed: Value = client
        .get(&url)
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mine = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["rule_code"] == code)
        .expect("新建规则应出现在列表");
    assert_eq!(mine["params"]["multiplier"], "0.8");
    assert_eq!(mine["params"]["start_minute"], 1320);

    // 普通用户无 pricing.write → 403
    let resp = client
        .get(&url)
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // 删除幂等：首次 200、再次 404
    let del = format!("{url}/{code}");
    let resp = client
        .delete(&del)
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .delete(&del)
        .bearer_auth(&env.super_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
