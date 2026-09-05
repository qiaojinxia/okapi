//! 模型修饰符验收（IMPLEMENTATION §11.25，形状对齐 new-api rc.32/33）。
//!
//! 此前只认四个写死的连字符后缀（`-high/-medium/-low/-thinking[-N]`），既表达不了组合，
//! 也**没法给变体单独定价**。现在 `base@key:value` 通用语法 + 规范计费名（与书写顺序无关）
//! + 变体没配价时回退基座价。
//!
//! 关键分工：**路由用基座名、计价用规范变体名**——渠道声明的是基座模型名，
//! 拿 `gpt-5@effort:high` 去选渠道会一个候选都选不到。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::gateway;
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

type Seen = Arc<Mutex<Vec<Value>>>;

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    user_id: i64,
    token: String,
    base: String,
    gateway: SocketAddr,
    seen: Seen,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&seen);
    let mock = serve(Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::body::Bytes| {
            let rec = Arc::clone(&rec);
            async move {
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let model = v.get("model").cloned().unwrap_or(Value::Null);
                rec.lock().unwrap().push(v);
                axum::Json(json!({
                    "id": "cmpl", "object": "chat.completion", "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}}],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 100}
                }))
                .into_response()
            }
        }),
    ))
    .await;

    // 基座 model_ratio 1.0 → (100 + 100) × 1.0 × $2/1M = 400 micro
    let base = format!("mm-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &base, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &pg,
        &format!("mm-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[base.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("mm-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-mm-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-mm")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(100_000_000))
        .await
        .unwrap();
    let gw = serve(gateway::router(state)).await;

    Bed {
        pg,
        user_id,
        token,
        base,
        gateway: gw,
        seen,
    }
}

/// `msg` 换文避免 L2 粘性；返回 HTTP 状态。
async fn chat(bed: &Bed, model: &str, msg: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&json!({
            "model": model, "stream": false,
            "messages": [{"role": "user", "content": msg}]
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// 等第 n 笔结算，返回 (记账模型名, 实收)。
async fn settlement(pg: &PgPool, user_id: i64, n: usize) -> (String, i64) {
    for _ in 0..80 {
        let rows = sqlx::query!(
            r#"SELECT model_name, amount_micro FROM billing_records
               WHERE user_id = $1 AND status = 20 ORDER BY id"#,
            user_id
        )
        .fetch_all(pg)
        .await
        .unwrap();
        if rows.len() >= n {
            let r = &rows[n - 1];
            return (r.model_name.clone(), r.amount_micro);
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    panic!("第 {n} 笔结算未落库");
}

/// 变体没配价 → 回退基座价，且路由照常（渠道只声明了基座名）。
#[tokio::test]
async fn unpriced_variant_falls_back_to_base() {
    let bed = setup().await;
    let variant = format!("{}@effort:high", bed.base);

    assert_eq!(
        chat(&bed, &variant, "v1").await,
        200,
        "变体没配价也不该失败"
    );
    let (model, amount) = settlement(&bed.pg, bed.user_id, 1).await;
    assert_eq!(model, bed.base, "没配变体价 → 记账按基座名");
    assert_eq!(amount, 400, "(100+100) × 1.0 × $2/1M");

    // 路由确实落到了声明基座名的渠道，且上游收到的是基座名而非变体名
    let seen = bed.seen.lock().unwrap();
    assert_eq!(
        seen.last().unwrap()["model"],
        bed.base,
        "上游要拿基座名——它不认识 okapi 的修饰符语法"
    );
}

/// 给变体单独配价 → 按变体收，记账名也是规范变体名。
#[tokio::test]
async fn priced_variant_bills_at_its_own_rate() {
    let bed = setup().await;
    let variant = format!("{}@effort:high", bed.base);
    // 变体贵三倍
    okapi_store::provision::create_model_ratio(&bed.pg, &variant, "3.0", "1.0", "1.0")
        .await
        .unwrap();
    // 价簿是进程内快照，重建 state 让它读到新价
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let gw = serve(gateway::router(state)).await;
    let bed = Bed { gateway: gw, ..bed };

    assert_eq!(chat(&bed, &variant, "p1").await, 200);
    let (model, amount) = settlement(&bed.pg, bed.user_id, 1).await;
    assert_eq!(model, variant, "配了变体价 → 记账按规范变体名");
    assert_eq!(amount, 1200, "变体 3.0 倍率：400 × 3");

    // 上游拿到的仍是基座名
    let seen = bed.seen.lock().unwrap();
    assert_eq!(seen.last().unwrap()["model"], bed.base);
}

/// 书写顺序不同、新旧语法不同，都必须落到同一条账。
#[tokio::test]
async fn different_spellings_collapse_to_one_billing_name() {
    let bed = setup().await;
    let variant = format!("{}@effort:high@thinking:on", bed.base);
    okapi_store::provision::create_model_ratio(&bed.pg, &variant, "2.0", "1.0", "1.0")
        .await
        .unwrap();
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let gw = serve(gateway::router(state)).await;
    let bed = Bed { gateway: gw, ..bed };

    // 反序书写
    let reversed = format!("{}@thinking:on@effort:high", bed.base);
    assert_eq!(chat(&bed, &reversed, "s1").await, 200);
    let (model, amount) = settlement(&bed.pg, bed.user_id, 1).await;
    assert_eq!(
        model, variant,
        "拼接顺序不同不该分裂成两条账（否则价要配两遍）"
    );
    assert_eq!(amount, 800, "变体 2.0 倍率");

    // 旧的连字符写法归一到同一个变体（-high == @effort:high）
    let legacy_variant = format!("{}@effort:high", bed.base);
    okapi_store::provision::create_model_ratio(&bed.pg, &legacy_variant, "5.0", "1.0", "1.0")
        .await
        .unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let gw = serve(gateway::router(state)).await;
    let bed = Bed { gateway: gw, ..bed };
    assert_eq!(chat(&bed, &format!("{}-high", bed.base), "s2").await, 200);
    let (model, amount) = settlement(&bed.pg, bed.user_id, 2).await;
    assert_eq!(model, legacy_variant, "旧后缀应归一到 @effort:high");
    assert_eq!(amount, 2000, "变体 5.0 倍率");
}

/// 不认识的修饰符键不装懂：按模型不存在处理，而不是注入不了却照收钱。
#[tokio::test]
async fn unknown_modifier_is_rejected_not_silently_dropped() {
    let bed = setup().await;
    assert_eq!(
        chat(&bed, &format!("{}@wat:1", bed.base), "u1").await,
        404,
        "不认识的键应报模型不存在"
    );
    assert_eq!(
        chat(&bed, &format!("{}@effort:turbo", bed.base), "u2").await,
        404,
        "值不在值域内同理"
    );
    // 基座本身照常可用
    assert_eq!(chat(&bed, &bed.base, "u3").await, 200);
}
