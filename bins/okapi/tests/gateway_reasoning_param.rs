//! 请求体 reasoning 参数的归一与三向注入（IMPLEMENTATION §11.26）。
//!
//! 修的是一个静默失效：跨方言请求转换是**严格白名单**（`request_openai_to_anthropic`
//! 只搬 model/max_tokens/system/messages/stop_sequences/tools/tool_choice），
//! 客户端发的 `reasoning_effort` 到不了 anthropic 上游——请求照样 200、
//! 思考从没开过、钱照收。同方言透传时又是好的，所以很难被发现。
//!
//! 现在三种方言的写法都先归一成一个内部指令，再按渠道方言展开注入。
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

/// 上游实际收到的请求体（断言注入结果用）。
type Seen = Arc<Mutex<Vec<Value>>>;

struct Bed {
    pg: PgPool,
    user_id: i64,
    token: String,
    base: String,
    gateway: SocketAddr,
    seen: Seen,
}

impl Bed {
    /// 上游最后收到的那一份请求体。
    fn upstream(&self) -> Value {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("上游没收到任何请求")
    }
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// 录下请求体并回一个最小的合法响应（两种方言各一份形状）。
async fn mock(provider: &str) -> (SocketAddr, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&seen);
    let anthropic = provider == "anthropic";
    let handler = move |body: axum::body::Bytes| {
        let rec = Arc::clone(&rec);
        async move {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            rec.lock().unwrap().push(v.clone());
            if anthropic {
                axum::Json(json!({
                    "id": "msg_1", "type": "message", "role": "assistant", "model": "up",
                    "content": [{"type": "text", "text": "hi"}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 100, "output_tokens": 100}
                }))
                .into_response()
            } else {
                axum::Json(json!({
                    "id": "cmpl", "object": "chat.completion",
                    "model": v.get("model").cloned().unwrap_or(Value::Null),
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}}],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 100}
                }))
                .into_response()
            }
        }
    };
    let path = if anthropic {
        "/v1/messages"
    } else {
        "/v1/chat/completions"
    };
    (serve(Router::new().route(path, post(handler))).await, seen)
}

/// 建一条 `provider` 方言的渠道 + 一个基座模型（倍率 1.0）+ 一个 `@effort:low`
/// 定价变体（倍率 3.0，用来验证"参数改注入、不改计费名"）。
async fn setup(provider: &str) -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let (upstream, seen) = mock(provider).await;

    let base = format!("rp-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &base, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    // 变体价必须在 build_state 之前落库：PriceBook 是进程内快照
    okapi_store::provision::create_model_ratio(
        &pg,
        &format!("{base}@effort:low"),
        "3.0",
        "1.0",
        "1.0",
    )
    .await
    .unwrap();
    okapi_store::provision::create_channel(
        &pg,
        &format!("rp-ch-{suffix}"),
        provider,
        &format!("http://{upstream}/v1"),
        "cred",
        &[base.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("rp-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-rp-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-rp")
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
    let gateway = serve(gateway::router(state)).await;

    Bed {
        pg,
        user_id,
        token,
        base,
        gateway,
        seen,
    }
}

/// `extra` 合并进请求体顶层；`msg` 换文避免 L2 粘性。返回 HTTP 状态。
async fn chat(bed: &Bed, model: &str, msg: &str, extra: Value) -> u16 {
    let mut body = json!({
        "model": model, "stream": false, "max_tokens": 64,
        "messages": [{"role": "user", "content": msg}]
    });
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// 等第 n 笔结算，返回记账模型名与实收。
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

/// **本次要修的缺口**：OpenAI 方言客户端 + Anthropic 上游，
/// `reasoning_effort` 此前被白名单转换器整个丢掉，现在应翻译成 thinking 预算。
#[tokio::test]
async fn openai_effort_reaches_an_anthropic_upstream() {
    let bed = setup("anthropic").await;
    assert_eq!(
        chat(&bed, &bed.base, "x1", json!({"reasoning_effort": "high"})).await,
        200
    );
    let up = bed.upstream();
    assert_eq!(
        up["thinking"]["type"], "enabled",
        "high 应翻译成 thinking：{up}"
    );
    assert_eq!(up["thinking"]["budget_tokens"], 16_000, "high 档的预算映射");
    assert!(
        up["max_tokens"].as_u64().unwrap() > 16_000,
        "anthropic 硬约束：max_tokens 必须大于思考预算"
    );
    assert!(
        up.get("reasoning_effort").is_none(),
        "anthropic 不认识这个键"
    );
}

/// 统一 `reasoning` 对象翻译成 OpenAI 原生档位，且原对象必须剥掉——上游不认识它。
#[tokio::test]
async fn unified_object_is_translated_then_stripped() {
    let bed = setup("openai").await;
    assert_eq!(
        chat(
            &bed,
            &bed.base,
            "x2",
            json!({"reasoning": {"effort": "high", "max_tokens": 4096}})
        )
        .await,
        200
    );
    let up = bed.upstream();
    assert_eq!(up["reasoning_effort"], "high");
    assert!(
        up.get("reasoning").is_none(),
        "okapi 自己的指令必须剥掉，否则上游 400：{up}"
    );
}

/// 参数压过模型名后缀，但**不改计费名**——与 OpenRouter 一致：
/// 只有模型名上的变体改计价，请求参数不改。
#[tokio::test]
async fn parameter_overrides_the_name_but_not_the_price() {
    let bed = setup("openai").await;
    let variant = format!("{}@effort:low", bed.base);
    assert_eq!(
        chat(
            &bed,
            &variant,
            "x3",
            json!({"reasoning": {"effort": "high"}})
        )
        .await,
        200
    );
    assert_eq!(
        bed.upstream()["reasoning_effort"],
        "high",
        "显式参数压过名字里的 low"
    );

    let (model, amount) = settlement(&bed.pg, bed.user_id, 1).await;
    assert_eq!(model, variant, "计费名仍由模型名决定");
    assert_eq!(amount, 1200, "变体 3.0 倍率：(100+100) × 3 × $2/1M");
}

/// `enabled:false` 要能关掉模型名里带的思考——否则"名字选好了就关不掉"。
#[tokio::test]
async fn explicit_disable_beats_the_name_suffix() {
    let bed = setup("openai").await;
    let variant = format!("{}@effort:low", bed.base);
    assert_eq!(
        chat(
            &bed,
            &variant,
            "x4",
            json!({"reasoning": {"enabled": false}})
        )
        .await,
        200
    );
    let up = bed.upstream();
    assert!(
        up.get("reasoning_effort").is_none(),
        "明确关掉后不该再注入档位：{up}"
    );
    assert!(up.get("reasoning").is_none(), "指令仍要剥掉");

    // 对照：不写参数时，名字里的 low 照常注入
    assert_eq!(chat(&bed, &variant, "x5", json!({})).await, 200);
    assert_eq!(bed.upstream()["reasoning_effort"], "low");
}

/// 对称的另一半：Anthropic 方言客户端 + OpenAI 上游。客户端只会说预算、
/// 上游只认档位，此前 `apply_openai` 见没有 effort 就直接原样返回——同样是静默丢失。
#[tokio::test]
async fn anthropic_thinking_budget_reaches_an_openai_upstream() {
    let bed = setup("openai").await;
    let status = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&json!({
            "model": bed.base, "max_tokens": 64,
            "messages": [{"role": "user", "content": "x6"}],
            "thinking": {"type": "enabled", "budget_tokens": 4096}
        }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(status, 200);
    let up = bed.upstream();
    assert_eq!(
        up["reasoning_effort"], "low",
        "预算 4096 应按逆映射折成 low：{up}"
    );
}
