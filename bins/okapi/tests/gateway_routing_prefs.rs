//! 请求级路由偏好验收（IMPLEMENTATION §11.24，形状对齐 OpenRouter `provider` 对象）。
//!
//! 此前路由控制全在配置侧（key.pool_override > 分组 pool_code > default），调用方
//! 一点管不着。本轮先落三个子集：
//! - `zdr` / `data_collection:"deny"`：只走声明不留存的渠道（未声明按不满足处理）
//! - `max_price.{prompt,completion}`：单价上限，超了在**预扣之前**拒
//! - `allow_fallbacks:false`：失败即返回，不改投其它渠道
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

/// 记录收到的请求体，供"指令必须剥掉"断言用。
type Seen = Arc<Mutex<Vec<Value>>>;

fn ok_body(model: &Value) -> Value {
    json!({
        "id": "cmpl", "object": "chat.completion", "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 20}
    })
}

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
    model: String,
    gateway: SocketAddr,
    /// 声明 data_retention='none' 的渠道
    zero_channel: i64,
    /// 未声明留存的渠道（优先级更高，缺省会先投它）
    plain_channel: i64,
    seen: Seen,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    // 记录请求体的正常上游
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
                axum::Json(ok_body(&model)).into_response()
            }
        }),
    ))
    .await;

    // model_ratio 1.0 / completion 1.0 → 输入单价恰好等于基准 $2/1M，便于卡上限
    let model = format!("rp-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();

    let mk = |name: String, priority: i32| {
        let pg = pg.clone();
        let base = format!("http://{mock}/v1");
        let m = model.clone();
        async move {
            let (cid, _) = okapi_store::provision::create_channel(
                &pg, &name, "openai", &base, "cred", &[m.as_str()], true, None,
            )
            .await
            .unwrap();
            sqlx::query!(
                "UPDATE channels SET priority = $2 WHERE id = $1",
                cid,
                priority
            )
            .execute(&pg)
            .await
            .unwrap();
            cid
        }
    };
    // plain 优先级更高：缺省请求会先投它，从而能验出 zdr 把它筛掉
    let plain_channel = mk(format!("rp-plain-{suffix}"), 50).await;
    let zero_channel = mk(format!("rp-zero-{suffix}"), 10).await;
    okapi_store::admin::patch_channel(
        &pg,
        zero_channel,
        okapi_store::admin::ChannelPatch {
            data_retention: Some("none"),
            ..Default::default()
        },
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
    let gw = serve(gateway::router(state)).await;

    Bed {
        pg,
        user_id,
        token,
        model,
        gateway: gw,
        zero_channel,
        plain_channel,
        seen,
    }
}

/// `msg` 决定 L2 会话哈希：同文 = 同会话 = 会命中粘性。要验路由选择的用例
/// 必须逐次换文，否则第二次会被粘性直接送回上次成功的渠道，压根不走候选排序。
async fn chat_msg(bed: &Bed, provider: Option<Value>, msg: &str) -> (u16, Value) {
    let mut body = json!({
        "model": bed.model, "stream": false,
        "messages": [{"role": "user", "content": msg}]
    });
    if let Some(p) = provider {
        body["provider"] = p;
    }
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn chat(bed: &Bed, provider: Option<Value>) -> (u16, Value) {
    chat_msg(bed, provider, "hi").await
}

/// 等结算落库并返回最新一笔（结算是响应返回后的后台任务）。
async fn last_record(pg: &PgPool, user_id: i64, n: i64) -> (i64, i64) {
    for _ in 0..80 {
        let rows = sqlx::query!(
            r#"SELECT channel_id, amount_micro FROM billing_records
               WHERE user_id = $1 AND status = 20 ORDER BY id"#,
            user_id
        )
        .fetch_all(pg)
        .await
        .unwrap();
        if rows.len() as i64 >= n {
            let r = &rows[(n - 1) as usize];
            return (r.channel_id.unwrap_or(0), r.amount_micro);
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    panic!("第 {n} 笔结算未落库");
}

/// 零留存：只走声明 none 的渠道；没有这样的渠道时给专门的错误码而不是"无可用渠道"。
#[tokio::test]
async fn zero_retention_filters_candidates() {
    let bed = setup().await;

    // 缺省：优先级更高的 plain 渠道接单
    assert_eq!(chat_msg(&bed, None, "z1").await.0, 200);
    let (ch, _) = last_record(&bed.pg, bed.user_id, 1).await;
    assert_eq!(ch, bed.plain_channel, "缺省应投优先级更高的未声明渠道");

    // zdr=true：plain 被筛掉，落到声明 none 的渠道
    assert_eq!(chat_msg(&bed, Some(json!({"zdr": true})), "z2").await.0, 200);
    let (ch, _) = last_record(&bed.pg, bed.user_id, 2).await;
    assert_eq!(ch, bed.zero_channel, "要求零留存必须落到声明 none 的渠道");

    // data_collection:"deny" 是同一个诉求
    assert_eq!(
        chat_msg(&bed, Some(json!({"data_collection": "deny"})), "z3").await.0,
        200
    );
    let (ch, _) = last_record(&bed.pg, bed.user_id, 3).await;
    assert_eq!(ch, bed.zero_channel);

    // 把唯一的零留存渠道也改成会训练 → 专门的错误码（不是 no_available_channel，
    // 否则运维会以为渠道全挂了）
    okapi_store::admin::patch_channel(
        &bed.pg,
        bed.zero_channel,
        okapi_store::admin::ChannelPatch {
            data_retention: Some("trains"),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5_200)).await; // 候选缓存 5s
    let (status, body) = chat(&bed, Some(json!({"zdr": true}))).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "no_zero_retention_channel");
    // 不要求零留存时照常可用
    assert_eq!(chat(&bed, None).await.0, 200);
}

/// 单价上限：在预扣之前判，超限不扣费、不打上游。
#[tokio::test]
async fn max_price_rejects_before_reserving() {
    let bed = setup().await;
    let before = bed.seen.lock().unwrap().len();

    // 该模型 model_ratio 1.0 → 输入单价 = 基准 $2/1M。上限 1.0 必然超。
    let (status, body) = chat(&bed, Some(json!({"max_price": {"prompt": 1.0}}))).await;
    assert_eq!(status, 402, "{body}");
    assert_eq!(body["error"]["code"], "price_above_max");
    assert!(
        body["error"]["param"].as_str().unwrap_or("").starts_with("prompt:"),
        "param 要回显是哪一轴超了、实际单价多少：{body}"
    );
    assert_eq!(
        bed.seen.lock().unwrap().len(),
        before,
        "超限不该打上游——判在预扣之前"
    );
    let charged = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM billing_records WHERE user_id = $1"#,
        bed.user_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(charged, 0, "超限不该留下任何记账行");

    // 上限放宽到 2.5 → 放行
    assert_eq!(
        chat(&bed, Some(json!({"max_price": {"prompt": 2.5}}))).await.0,
        200
    );
    // 输出侧独立判：completion_ratio 1.0 → 输出单价也是 2.0
    let (status, body) = chat(&bed, Some(json!({"max_price": {"completion": 0.5}}))).await;
    assert_eq!(status, 402);
    assert!(
        body["error"]["param"]
            .as_str()
            .unwrap_or("")
            .starts_with("completion:"),
        "{body}"
    );
}

/// allow_fallbacks=false：首次失败即返回；缺省仍照常 failover。
#[tokio::test]
async fn allow_fallbacks_false_stops_at_first_failure() {
    let bed = setup().await;
    // 把优先级更高的 plain 渠道指到一个不可达地址，逼出 failover
    sqlx::query!(
        "UPDATE channels SET api_base = 'http://127.0.0.1:9/v1' WHERE id = $1",
        bed.plain_channel
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5_200)).await;

    // 缺省：plain 失败 → 改投 zero 渠道 → 成功
    let (status, body) = chat_msg(&bed, None, "round-one").await;
    assert_eq!(status, 200, "缺省应 failover 到可用渠道：{body}");
    let (ch, _) = last_record(&bed.pg, bed.user_id, 1).await;
    assert_eq!(ch, bed.zero_channel);

    // 复位坏渠道的 key 状态：上一次 failover 已经给它记了失败，连续 3 次会转冷却而被
    // 候选查询直接排除——那样第二次请求压根没有"要不要改投"的选择，前提就没了
    sqlx::query!(
        r#"UPDATE channel_keys SET status = 1, failed_count = 0, cooldown_until = NULL
           WHERE channel_id = $1"#,
        bed.plain_channel
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5_200)).await;

    // 前提自检：坏渠道必须重新回到候选里，否则"要不要改投"根本无从谈起
    let plain_ok = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM channel_keys
           WHERE channel_id = $1 AND status = 1
             AND (cooldown_until IS NULL OR cooldown_until < now())"#,
        bed.plain_channel
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(plain_ok, 1, "复位后坏渠道应重新可选");

    // allow_fallbacks=false：同样的坏渠道排在前面，这次不再改投
    let (status, body) = chat_msg(&bed, Some(json!({"allow_fallbacks": false})), "round-two").await;
    // 等第二笔记录落库，看它是"失败且没改投"还是"又 failover 成功了"
    for _ in 0..80 {
        let rows = sqlx::query!(
            r#"SELECT status, failover_count, channel_id FROM billing_records
               WHERE user_id = $1 ORDER BY id"#,
            bed.user_id
        )
        .fetch_all(&bed.pg)
        .await
        .unwrap();
        if rows.len() >= 2 {
            let second = &rows[1];
            assert_ne!(status, 200, "声明不改投时不该再落到备用渠道：{body}");
            assert_eq!(second.status, 40, "第二笔应是失败记账");
            assert_eq!(second.failover_count, 0, "不改投 → failover 计数必须为 0");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    panic!("第二笔记录未落库；本次 HTTP {status} {body}");
}

/// 路由指令是 okapi 自己的，转发前必须剥掉——上游不认识它。
#[tokio::test]
async fn provider_directive_never_reaches_upstream() {
    let bed = setup().await;
    assert_eq!(
        chat(&bed, Some(json!({"zdr": true, "allow_fallbacks": false}))).await.0,
        200
    );
    let seen = bed.seen.lock().unwrap();
    let last = seen.last().expect("上游应收到请求");
    assert!(
        last.get("provider").is_none(),
        "provider 指令必须在转发前剥掉：{last}"
    );
    assert_eq!(last["model"], bed.model, "其余字段原样透传");
}
