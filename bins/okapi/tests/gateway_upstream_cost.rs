//! 上游成本采集验收（IMPLEMENTATION §11.18）：此前 `billing_records.upstream_cost_micro`
//! 从不写、CH 行硬编码 0，毛利永远"待采集"。现在成本 = 官方价（乘分组倍率前）×
//! 渠道相对成本系数，结算时集中折算：
//! - 系数 0.5 的渠道、vip 分组 ×2：实收 = 2 × 官方价，成本 = 0.5 × 官方价（分组倍率不进成本）
//! - outbox 载荷带 upstream_cost_micro（CH 侧据此聚合）
//! - 管理面 PATCH cost_milli 生效 + 列表回显
//!
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

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    if req["messages"][0]["content"] == "reject" {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": {"message": "invalid test input"}})),
        )
            .into_response();
    }
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion","model": req["model"],
        "choices":[{"index":0,"message":{"role":"assistant","content":"hi"}}],
        "usage":{"prompt_tokens":1000,"completion_tokens":200}
    }))
    .into_response()
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
    channel_id: i64,
    gateway: SocketAddr,
    console: SocketAddr,
    admin_token: String,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    let model = format!("cost-m-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "2.0", "1.0", "1.0")
        .await
        .unwrap();
    // vip 分组 ×2，挂在 default 池（渠道也在 default 池）
    let group = format!("cost-vip-{suffix}");
    okapi_store::admin::upsert_price_group(
        &pg,
        okapi_store::admin::PriceGroupInput {
            group_code: &group,
            group_ratio: "2.0",
            description: "",
            pool_code: None,
            self_select: false,
        },
    )
    .await
    .unwrap();
    let (channel_id, _) = okapi_store::provision::create_channel(
        &pg,
        &format!("cost-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    // 相对成本 0.5：五折代理
    okapi_store::admin::patch_channel(
        &pg,
        channel_id,
        okapi_store::admin::ChannelPatch {
            cost_milli: Some(500),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("cost-u-{suffix}"))
        .await
        .unwrap();
    sqlx::query!(
        "INSERT INTO user_groups (user_id, group_code) VALUES ($1, $2)",
        user_id,
        group
    )
    .execute(&pg)
    .await
    .unwrap();
    let token = format!("sk-okapi-cost-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-cost")
        .await
        .unwrap();

    // 超管：管理面 PATCH 渠道用
    let admin_id = okapi_store::provision::create_user(&pg, &format!("cost-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-cost-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-adm")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(50_000_000))
        .await
        .unwrap();
    let gateway_addr = serve(gateway::router(state.clone())).await;
    let console_addr = serve(console::router(state)).await;

    Bed {
        pg,
        user_id,
        token,
        model,
        channel_id,
        gateway: gateway_addr,
        console: console_addr,
        admin_token,
    }
}

async fn chat(bed: &Bed) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&json!({"model": bed.model, "stream": false,
                      "messages": [{"role": "user", "content": "hello"}]}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

struct Committed {
    request_id: Uuid,
    amount: i64,
    original: i64,
    upstream_cost: Option<i64>,
}

async fn wait_committed(pg: &PgPool, user_id: i64, seen: &[Uuid]) -> Committed {
    for _ in 0..50 {
        let rows = sqlx::query!(
            r#"SELECT request_id, amount_micro, original_amount_micro, upstream_cost_micro
               FROM billing_records WHERE user_id = $1 AND status = 20 ORDER BY id"#,
            user_id
        )
        .fetch_all(pg)
        .await
        .unwrap();
        if let Some(r) = rows.into_iter().find(|r| !seen.contains(&r.request_id)) {
            return Committed {
                request_id: r.request_id,
                amount: r.amount_micro,
                original: r.original_amount_micro,
                upstream_cost: r.upstream_cost_micro,
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("未等到 committed 记录");
}

#[tokio::test]
async fn upstream_cost_is_list_price_times_channel_factor() {
    let bed = setup().await;
    assert_eq!(chat(&bed).await, 200);
    let first = wait_committed(&bed.pg, bed.user_id, &[]).await;

    // 官方价 = 实收 / 分组倍率 2；成本 = 官方价 × 0.5 = 实收 / 4
    assert!(first.amount > 0);
    assert_eq!(
        first.original, first.amount,
        "无个人倍率与规则时标价 = 实收"
    );
    let list_price = first.amount / 2;
    assert_eq!(
        first.upstream_cost,
        Some(list_price / 2),
        "成本 = 官方价 × 相对成本系数，分组倍率不进成本"
    );

    // outbox 载荷同样带成本（CH 聚合口径来源）
    let payload: Value = sqlx::query_scalar!(
        r#"SELECT payload FROM billing_outbox WHERE payload->>'request_id' = $1 ORDER BY id DESC LIMIT 1"#,
        first.request_id.to_string()
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(payload["upstream_cost_micro"].as_i64(), first.upstream_cost);
    assert_eq!(payload["upstream_cost_known"], true);
    assert_eq!(payload["requested_model"], bed.model);
    assert_eq!(payload["upstream_model"], bed.model);
    assert_eq!(payload["endpoint"], "/v1/chat/completions");
    assert_eq!(payload["upstream_endpoint"], "/v1/chat/completions");
    assert_eq!(payload["billing_type"], "ratio");

    // 管理面把系数改成 0（自建）→ 缓存失效 → 下一笔成本为 0；列表回显 cost_milli
    let client = reqwest::Client::new();
    let resp = client
        .patch(format!(
            "http://{}/admin/channels/{}",
            bed.console, bed.channel_id
        ))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"cost_milli": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let list: Value = client
        .get(format!("http://{}/admin/channels", bed.console))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mine = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == bed.channel_id)
        .unwrap();
    assert_eq!(mine["cost_milli"], 0);

    assert_eq!(chat(&bed).await, 200);
    let second = wait_committed(&bed.pg, bed.user_id, &[first.request_id]).await;
    assert_eq!(
        second.upstream_cost,
        Some(0),
        "自建渠道成本为 0（而非 None）"
    );

    // 离谱系数拒收
    let resp = client
        .patch(format!(
            "http://{}/admin/channels/{}",
            bed.console, bed.channel_id
        ))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"cost_milli": -5}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["param"], "cost_milli");
}

#[tokio::test]
async fn analytics_dimensions_preserve_alias_billing_model_and_upstream_mapping() {
    let bed = setup().await;
    let alias = format!("alias-{}", bed.model);
    let upstream = format!("upstream-{}", bed.model);
    sqlx::query("INSERT INTO model_aliases (pattern, target_model) VALUES ($1, $2)")
        .bind(&alias)
        .bind(&bed.model)
        .execute(&bed.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET model_mapping = $1 WHERE id = $2")
        .bind(json!({ &bed.model: &upstream }))
        .bind(bed.channel_id)
        .execute(&bed.pg)
        .await
        .unwrap();
    let response = reqwest::Client::new().post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token).json(&json!({"model": alias, "stream": false, "messages": [{"role": "user", "content": "hello"}]}))
        .send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let record = wait_committed(&bed.pg, bed.user_id, &[]).await;
    let row: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'request_id' = $1 ORDER BY id DESC LIMIT 1")
        .bind(record.request_id.to_string()).fetch_one(&bed.pg).await.unwrap();
    assert_eq!(row["requested_model"], alias);
    assert_eq!(row["model"], bed.model);
    assert_eq!(row["upstream_model"], upstream);
    assert_eq!(row["endpoint"], "/v1/chat/completions");
    assert_eq!(row["upstream_cost_known"], true);
    let failed = reqwest::Client::new().post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token).json(&json!({"model": alias, "stream": false, "messages": [{"role": "user", "content": "reject"}]}))
        .send().await.unwrap();
    assert_eq!(failed.status().as_u16(), 400);
    let mut logged = false;
    for _ in 0..50 {
        let error: Option<Value> = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE payload->>'user_id' = $1 AND payload->>'log_type' = '5' ORDER BY id DESC LIMIT 1")
            .bind(bed.user_id.to_string()).fetch_optional(&bed.pg).await.unwrap();
        if let Some(error) = error {
            assert_eq!(error["requested_model"], alias);
            assert_eq!(error["upstream_model"], upstream);
            assert_eq!(error["upstream_endpoint"], "/v1/chat/completions");
            assert_eq!(error["upstream_cost_known"], false);
            logged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        logged,
        "failed upstream request must remain in the analysis dimensions"
    );
}
