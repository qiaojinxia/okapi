//! 同一个失败原因，**每一个**计费端点必须回同一个 `error_code`。
//!
//! 为什么不写成"输入 X 应回码 Y"的硬编码真值表：那等于把业务语义在用例里抄第二遍，
//! 抄错了还会变成"实现和用例都错、但互相印证"。这里换个判据——
//! **拿覆盖最全的 `/v1/chat/completions` 当参照系，在运行时把真值表导出来**，
//! 再要求其余端点逐条对齐。用例不声称哪个码是"对"的，只声称**它们必须一致**。
//!
//! 这条缺口是实的：`gateway_key_admission` 把停用 / 过期 / 封禁 / 白名单 / 未知模型
//! 五种条件验得很细，但**只在 chat 上验**。其余七个计费端点各自跑一遍鉴权与模型解析，
//! 任何一条分支回了不同的码（甚至不同的 HTTP 状态），用户侧就是"同一个错在不同接口
//! 报不同话"，而每个端点自己的套件都是绿的。
//!
//! 覆盖的失败条件（都不依赖上游，故不必起 mock 上游的对应路由）：
//! 无 key / 停用 key / 过期 key / 封禁属主 / 模型不在白名单 / 模型不存在 / 余额不足。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::gateway;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(router: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Bed {
    pg: PgPool,
    gateway: SocketAddr,
    user_id: i64,
    model: String,
    /// 存在但不在本 key 白名单里的模型。
    walled_model: String,
    /// per_call 定价：images 只收这种（图片请求没有 token，ratio 算出来恒 0）。
    per_call_model: String,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let model = format!("tax-m-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let walled_model = format!("tax-w-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &walled_model, "1", "1", "1")
        .await
        .unwrap();
    let per_call_model = format!("tax-p-{suffix}");
    let pc_id = okapi_store::provision::create_model_ratio(&pg, &per_call_model, "1", "1", "1")
        .await
        .unwrap();
    sqlx::query!(
        r#"UPDATE model_pricing SET pricing_mode = 'per_call', per_call_price_micro = 40000
           WHERE model_id = $1"#,
        pc_id
    )
    .execute(&pg)
    .await
    .unwrap();
    // 渠道存在但指向黑洞：本用例全部条件都在打上游之前就该失败
    okapi_store::provision::create_channel(
        &pg,
        &format!("tax-ch-{suffix}"),
        "openai",
        "http://127.0.0.1:1/v1",
        "cred",
        &[
            model.as_str(),
            walled_model.as_str(),
            per_call_model.as_str(),
        ],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("tax-u-{suffix}"))
        .await
        .unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "tax-node", None, None)
        .await
        .unwrap();
    // 本用例只打数据面，不需要 console
    let gateway_addr = serve(gateway::router(state)).await;

    Bed {
        pg,
        gateway: gateway_addr,
        user_id,
        model,
        walled_model,
        per_call_model,
    }
}

/// 该端点能接受的模型定价形态。
///
/// 不是所有端点都收同一种模型：images 的请求没有 token，ratio 定价算出来恒 0，
/// 故它（与 audio 的 transcriptions 一样）只收 per_call。要验"余额不足"这类
/// **跨端点共有**的失败条件，就得各自喂它收得下的模型，否则撞的是配置错而不是余额。
fn natural_model<'a>(bed: &'a Bed, path: &str) -> &'a str {
    if path.starts_with("/v1/images/") {
        &bed.per_call_model
    } else {
        &bed.model
    }
}

/// 建一把新 key（每个条件用各自的 key，免得鉴权缓存串味）。
async fn new_key(bed: &Bed, tag: &str) -> String {
    let token = format!("sk-okapi-tax-{tag}-{}", Uuid::new_v4().simple());
    okapi_store::provision::create_api_key(&bed.pg, bed.user_id, &hash(&token), "sk-tax")
        .await
        .unwrap();
    token
}

/// 打一个端点，回 (status, error_code)。
async fn probe(bed: &Bed, path: &str, token: &str, model: &str) -> (u16, String) {
    let body = match path {
        "/v1/embeddings" => json!({"model": model, "input": "hi"}),
        "/v1/rerank" => json!({"model": model, "query": "q", "documents": ["a"]}),
        "/v1/images/generations" => json!({"model": model, "prompt": "a cat"}),
        "/v1/audio/speech" => json!({"model": model, "input": "hi", "voice": "alloy"}),
        _ => json!({"model": model, "stream": false,
                    "messages": [{"role": "user", "content": "hi"}]}),
    };
    let resp = reqwest::Client::new()
        .post(format!("http://{}{path}", bed.gateway))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let v = resp.json::<Value>().await.unwrap_or(Value::Null);
    let code = v["error"]["code"]
        .as_str()
        .or_else(|| v["error"]["type"].as_str())
        .unwrap_or("<无码>")
        .to_owned();
    (status, code)
}

/// 七类失败条件 × 五个计费端点：码与状态必须与 chat 参照系逐条相同。
#[tokio::test]
async fn every_billing_endpoint_reports_the_same_code_for_the_same_failure() {
    const REFERENCE: &str = "/v1/chat/completions";

    let bed = setup().await;
    let others = [
        "/v1/embeddings",
        "/v1/rerank",
        "/v1/images/generations",
        "/v1/audio/speech",
    ];

    // 每个条件：(名字, 准备好 token 与 model)
    // model 为 None = 用该端点的自然模型（见 natural_model）
    let mut conditions: Vec<(&str, String, Option<String>)> = Vec::new();

    conditions.push(("无 key", "sk-okapi-does-not-exist".to_owned(), None));

    let t = new_key(&bed, "disabled").await;
    sqlx::query!(
        "UPDATE api_keys SET status = 2 WHERE key_hash = $1",
        hash(&t)
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    conditions.push(("停用 key", t, None));

    let t = new_key(&bed, "expired").await;
    sqlx::query!(
        "UPDATE api_keys SET expires_at = now() - interval '1 day' WHERE key_hash = $1",
        hash(&t)
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    conditions.push(("过期 key", t, None));

    let t = new_key(&bed, "walled").await;
    sqlx::query!(
        "UPDATE api_keys SET model_allowlist = $2 WHERE key_hash = $1",
        hash(&t),
        json!([bed.model])
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    conditions.push(("模型不在白名单", t, Some(bed.walled_model.clone())));

    let t = new_key(&bed, "nomodel").await;
    conditions.push((
        "模型不存在",
        t,
        Some(format!("no-such-{}", Uuid::new_v4().simple())),
    ));

    let t = new_key(&bed, "nobalance").await;
    conditions.push(("余额不足", t, None));

    let mut problems: Vec<String> = Vec::new();
    for (name, token, model) in &conditions {
        let ref_model = model
            .clone()
            .unwrap_or_else(|| natural_model(&bed, REFERENCE).to_owned());
        let reference = probe(&bed, REFERENCE, token, &ref_model).await;
        if reference.0 < 400 {
            problems.push(format!(
                "条件「{name}」在参照端点上没失败（{} {}），这条没构造成功",
                reference.0, reference.1
            ));
            continue;
        }
        for ep in others {
            let ep_model = model
                .clone()
                .unwrap_or_else(|| natural_model(&bed, ep).to_owned());
            let got = probe(&bed, ep, token, &ep_model).await;
            if got != reference {
                problems.push(format!(
                    "条件「{name}」：{ep} 回 {} {}，而 {REFERENCE} 回 {} {}",
                    got.0, got.1, reference.0, reference.1
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "{} 处失败语义不一致：\n{}",
        problems.len(),
        problems.join("\n")
    );
}
