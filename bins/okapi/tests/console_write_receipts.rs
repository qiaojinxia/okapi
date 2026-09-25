//! 写接口回执里的数据，以及池详情的聚合内容。
//!
//! 逐接口端到端探针（把某个接口的 2xx 响应体换成错误内容，看有没有用例察觉）对下面这些接口全绿：
//! 删分组 / 删模型回的 `requires_publish`、删 key 回的属主 `user_id` 或 `key_id`、池详情里聚合出来的
//! 成员 / 模型 / 分组，此前都没有任何用例核对过——它们只在 `console_manage.rs`（并行会话正在改）
//! 里被调用，且只验了状态码。故另起此文件。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。查询一律用运行期检查的 `sqlx::query*`：CI 以
//! `SQLX_OFFLINE=true` 编译，测试专用的查询不值得进 `.sqlx` 缓存。

use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

/// 本文件的用例串行执行：删 key 后"立刻失效"靠的是按 key 精确删缓存，而同一进程里并行用例
/// 触发的全局 `auth_flush()` 会顺带把它冲掉，掩盖"没删缓存"的回归（同 `console_users`）。
static SERIAL: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

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
    console: SocketAddr,
    admin_token: String,
    user_id: i64,
    suffix: String,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let admin_id = okapi_store::provision::create_user(&pg, &format!("cwr-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = 100 WHERE id = $1")
        .bind(admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-cwr-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-cwr-adm")
        .await
        .unwrap();
    let user_id = okapi_store::provision::create_user(&pg, &format!("cwr-u-{suffix}"))
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(1_000_000))
        .await
        .unwrap();
    let console = serve(console::router(state)).await;
    Bed {
        pg,
        console,
        admin_token,
        user_id,
        suffix,
    }
}

/// 给本用例的用户新建一把 key，返回 (token, key_id)。
async fn new_key(bed: &Bed, tag: &str) -> (String, i64) {
    let token = format!("sk-okapi-cwr-{tag}-{}", Uuid::new_v4().simple());
    let id = okapi_store::provision::create_api_key(&bed.pg, bed.user_id, &hash(&token), "sk-cwr")
        .await
        .unwrap();
    (token, id)
}

async fn call(
    bed: &Bed,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let client = reqwest::Client::new();
    let url = format!("http://{}{path}", bed.console);
    let mut rb = match method {
        "POST" => client.post(url),
        "DELETE" => client.delete(url),
        _ => client.get(url),
    }
    .bearer_auth(token);
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let r = rb.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

/// 删分组、删模型的回执都带 `requires_publish: true`：价簿要等下一次发布才跟上，
/// 前端据此提示去发布。回执之外，库里的行确实没了。
#[tokio::test]
async fn deleting_pricing_objects_flags_requires_publish() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;

    let group = format!("cwrg{}", bed.suffix);
    let (status, body) = call(
        &bed,
        "POST",
        "/admin/groups",
        &bed.admin_token,
        Some(json!({"group_code": group, "group_ratio": "1.5"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = call(
        &bed,
        "DELETE",
        &format!("/admin/groups/{group}"),
        &bed.admin_token,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"ok": true, "requires_publish": true}));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM price_groups WHERE group_code = $1")
        .bind(&group)
        .fetch_one(&bed.pg)
        .await
        .unwrap();
    assert_eq!(left, 0, "分组行应已删除");

    let model = format!("cwr-m-{}", bed.suffix);
    okapi_store::provision::create_model_ratio(&bed.pg, &model, "1", "1", "1")
        .await
        .unwrap();
    let (status, body) = call(
        &bed,
        "DELETE",
        &format!("/admin/models/{model}"),
        &bed.admin_token,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"ok": true, "requires_publish": true}));
}

/// 管理端删 key：回执指明属主；这把 key 立刻失效，不等鉴权缓存过期。
#[tokio::test]
async fn admin_key_delete_names_owner_and_revokes_immediately() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let (victim, victim_id) = new_key(&bed, "adm-del").await;
    // 先用一次，把它写进鉴权缓存
    assert_eq!(call(&bed, "GET", "/api/me", &victim, None).await.0, 200);

    let (status, body) = call(
        &bed,
        "DELETE",
        &format!("/admin/keys/{victim_id}"),
        &bed.admin_token,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"ok": true, "user_id": bed.user_id}));
    assert_eq!(
        call(&bed, "GET", "/api/me", &victim, None).await.0,
        401,
        "删掉的 key 应立刻失效"
    );
}

/// 用户删自己的 key：回执指明删的是哪把；这把 key 立刻失效；删别人的 key 404。
#[tokio::test]
async fn portal_key_delete_names_key_and_revokes_immediately() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let (keeper, _) = new_key(&bed, "keep").await;
    let (victim, victim_id) = new_key(&bed, "del").await;
    assert_eq!(call(&bed, "GET", "/api/me", &victim, None).await.0, 200);

    let (status, body) = call(
        &bed,
        "DELETE",
        &format!("/api/me/keys/{victim_id}"),
        &keeper,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"ok": true, "key_id": victim_id}));
    assert_eq!(
        call(&bed, "GET", "/api/me", &victim, None).await.0,
        401,
        "删掉的 key 应立刻失效"
    );

    // 删别人的 key：404，而不是删成功
    let other = okapi_store::provision::create_user(&bed.pg, &format!("cwr-o-{}", bed.suffix))
        .await
        .unwrap();
    let other_key = okapi_store::provision::create_api_key(
        &bed.pg,
        other,
        &hash(&format!("sk-okapi-cwr-other-{}", bed.suffix)),
        "sk-cwr",
    )
    .await
    .unwrap();
    let (status, _) = call(
        &bed,
        "DELETE",
        &format!("/api/me/keys/{other_key}"),
        &keeper,
        None,
    )
    .await;
    assert_eq!(status, 404);
}

/// 池详情：成员渠道（含可用 key 数）、池内模型、引用该池的分组都要聚合对。
#[tokio::test]
async fn pool_detail_aggregates_members_models_and_groups() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let pool = format!("cwrp{}", bed.suffix);
    let (status, body) = call(
        &bed,
        "POST",
        "/admin/pools",
        &bed.admin_token,
        Some(json!({"pool_code": pool, "description": "detail probe",
                    "routing_strategy": "least_latency"})),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (m1, m2) = (
        format!("cwr-a-{}", bed.suffix),
        format!("cwr-b-{}", bed.suffix),
    );
    for m in [&m1, &m2] {
        okapi_store::provision::create_model_ratio(&bed.pg, m, "1", "1", "1")
            .await
            .unwrap();
    }
    let name = format!("cwr-ch-{}", bed.suffix);
    let (channel_id, _) = okapi_store::provision::create_channel(
        &bed.pg,
        &name,
        "openai",
        "http://127.0.0.1:1/v1",
        "cred",
        &[m1.as_str(), m2.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    let (status, body) = call(
        &bed,
        "POST",
        &format!("/admin/channels/{channel_id}/pools"),
        &bed.admin_token,
        Some(json!({"pools": [pool]})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let group = format!("cwrq{}", bed.suffix);
    let (status, body) = call(
        &bed,
        "POST",
        "/admin/groups",
        &bed.admin_token,
        Some(json!({"group_code": group, "group_ratio": "1", "pool_code": pool})),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, d) = call(
        &bed,
        "GET",
        &format!("/admin/pools/{pool}"),
        &bed.admin_token,
        None,
    )
    .await;
    assert_eq!(status, 200, "{d}");
    assert_eq!(d["pool_code"], pool.as_str());
    assert_eq!(d["description"], "detail probe");
    assert_eq!(d["routing_strategy"], "least_latency");
    let members = d["members"].as_array().expect("应有成员列表");
    assert_eq!(members.len(), 1, "{d}");
    let m = &members[0];
    assert_eq!(m["channel_id"], channel_id);
    assert_eq!(m["name"], name.as_str());
    assert_eq!(m["provider"], "openai");
    assert_eq!(m["status"], 1);
    assert_eq!(m["active_keys"], 1);
    let mut member_models: Vec<String> =
        serde_json::from_value(m["models"].clone()).unwrap_or_default();
    member_models.sort();
    assert_eq!(member_models, vec![m1.clone(), m2.clone()]);
    let mut pool_models: Vec<String> =
        serde_json::from_value(d["models"].clone()).unwrap_or_default();
    pool_models.sort();
    assert_eq!(pool_models, vec![m1, m2], "池内模型应是成员模型的并集");
    assert_eq!(d["groups"], json!([group]), "引用该池的分组");

    // 清理：分组引用着池，先删分组
    call(
        &bed,
        "DELETE",
        &format!("/admin/groups/{group}"),
        &bed.admin_token,
        None,
    )
    .await;
}
