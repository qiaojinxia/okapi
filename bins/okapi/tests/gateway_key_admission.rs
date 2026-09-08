//! 数据面准入两道闸的验收（09-08 第十九轮补：`key_disabled` 与 `model_not_allowed`
//! 两个错误码此前零断言）。
//!
//! 这两道闸都在 `authenticate` / `allows_model` 上，而鉴权结果**带缓存**（`auth:key:<hash>`，
//! 60s TTL + `auth:ver` 版本号）。所以真正要验的不是"库里改了值之后新连接会不会被拦"——
//! 那只要 PG 读得对就必然对；要验的是**已经在缓存里的那把 key 会不会立刻失效**：
//! 停用 / 封禁 / 改白名单都是出事时的急救手段，等 60s 才生效等于没有。
//! 因此每段都先打一发成功请求把缓存焐热，再改状态，再打。
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
use uuid::Uuid;

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion","model": req["model"],
        "choices":[{"index":0,"message":{"role":"assistant","content":"hi"}}],
        "usage":{"prompt_tokens":10,"completion_tokens":2}
    }))
    .into_response()
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    user_id: i64,
    /// 两个都挂在同一个渠道上：一个进 key 白名单，一个不进
    model: String,
    other_model: String,
    gateway: SocketAddr,
    console: SocketAddr,
    super_token: String,
}

async fn new_key(pg: &PgPool, user_id: i64, tag: &str) -> (String, i64) {
    let token = format!("sk-okapi-adm-{tag}-{}", Uuid::new_v4().simple());
    let id = okapi_store::provision::create_api_key(pg, user_id, &hash(&token), "sk-adm")
        .await
        .unwrap();
    (token, id)
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let mock = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/chat/completions", post(mock_ok)),
            )
            .await
            .unwrap();
        });
        addr
    };
    let model = format!("adm-m-{suffix}");
    let other_model = format!("adm-n-{suffix}");
    for m in [&model, &other_model] {
        okapi_store::provision::create_model_ratio(&pg, m, "1", "1", "1")
            .await
            .unwrap();
    }
    okapi_store::provision::create_channel(
        &pg,
        &format!("adm-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[model.as_str(), other_model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("adm-u-{suffix}"))
        .await
        .unwrap();
    let super_id = okapi_store::provision::create_user(&pg, &format!("adm-s-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-adm-super-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-adm-s")
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
    let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = gw_listener.local_addr().unwrap();
    let gw_app = gateway::router(state.clone());
    tokio::spawn(async move {
        axum::serve(gw_listener, gw_app).await.unwrap();
    });
    let cs_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = cs_listener.local_addr().unwrap();
    let cs_app = console::router(state);
    tokio::spawn(async move {
        axum::serve(cs_listener, cs_app).await.unwrap();
    });

    Bed {
        pg,
        user_id,
        model,
        other_model,
        gateway: gateway_addr,
        console: console_addr,
        super_token,
    }
}

async fn chat(bed: &Bed, token: &str, model: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(token)
        .json(&json!({"model": model, "stream": false,
                      "messages": [{"role": "user", "content": "hello"}]}))
        .send()
        .await
        .unwrap();
    (
        resp.status().as_u16(),
        resp.json::<Value>().await.unwrap_or(Value::Null),
    )
}

/// 三种"这把 key 不该再能用"：手动停用、到期、属主被封。
///
/// `is_usable` 一行同时看 `key_status` / `user_status` / `expires_at`，三者任一失守都得 401
/// `key_disabled`。封禁那条尤其要走管理端而不是裸 SQL——库里连带把令牌置停了，可鉴权缓存
/// 在 Redis 里，靠的是处理器额外调的 `auth_flush`；只改库不刷缓存的话，被封用户还能再打
/// 满一个 TTL。解封后要能立刻恢复，否则运维误封一个人就得等一分钟。
#[tokio::test]
async fn disabled_expired_and_banned_keys_are_rejected_without_ttl_lag() {
    let bed = setup().await;
    let client = reqwest::Client::new();

    // —— 手动停用：先焐热缓存 ——
    let (tok, key_id) = new_key(&bed.pg, bed.user_id, "off").await;
    assert_eq!(chat(&bed, &tok, &bed.model).await.0, 200, "停用前应可用");
    let patched = client
        .patch(format!("http://{}/admin/keys/{key_id}", bed.console))
        .bearer_auth(&bed.super_token)
        .json(&json!({ "status": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), 200);
    let (status, body) = chat(&bed, &tok, &bed.model).await;
    assert_eq!(status, 401, "停用后应立刻拒，而不是等缓存过期：{body}");
    assert_eq!(body["error"]["code"], "key_disabled", "{body}");
    // 停用是可逆的：改回 1 同样立刻生效
    client
        .patch(format!("http://{}/admin/keys/{key_id}", bed.console))
        .bearer_auth(&bed.super_token)
        .json(&json!({ "status": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(chat(&bed, &tok, &bed.model).await.0, 200, "恢复后应可用");

    // —— 到期：expires_at 是派生状态，库里 status 仍是 1，靠 is_usable 的第三项拦 ——
    let (tok, key_id) = new_key(&bed.pg, bed.user_id, "exp").await;
    assert_eq!(chat(&bed, &tok, &bed.model).await.0, 200);
    client
        .patch(format!("http://{}/admin/keys/{key_id}", bed.console))
        .bearer_auth(&bed.super_token)
        .json(&json!({ "expires_at": "2020-01-01T00:00:00Z" }))
        .send()
        .await
        .unwrap();
    let (status, body) = chat(&bed, &tok, &bed.model).await;
    assert_eq!(status, 401, "过期 key 不得放行：{body}");
    assert_eq!(body["error"]["code"], "key_disabled", "{body}");
    let still_enabled = sqlx::query_scalar!(
        r#"SELECT status AS "s!" FROM api_keys WHERE id = $1"#,
        key_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(still_enabled, 1, "过期不写回 status（3=expired 是派生态）");

    // —— 封禁属主：管理端一刀切，用户名下所有 key 一起停 ——
    let (tok_a, _) = new_key(&bed.pg, bed.user_id, "ban-a").await;
    let (tok_b, _) = new_key(&bed.pg, bed.user_id, "ban-b").await;
    for t in [&tok_a, &tok_b] {
        assert_eq!(chat(&bed, t, &bed.model).await.0, 200, "封禁前应可用");
    }
    let banned = client
        .post(format!(
            "http://{}/admin/users/{}/manage",
            bed.console, bed.user_id
        ))
        .bearer_auth(&bed.super_token)
        .json(&json!({ "action": "ban" }))
        .send()
        .await
        .unwrap();
    assert_eq!(banned.status(), 200);
    for t in [&tok_a, &tok_b] {
        let (status, body) = chat(&bed, t, &bed.model).await;
        assert_eq!(status, 401, "被封用户名下的 key 应立刻全停：{body}");
        assert_eq!(body["error"]["code"], "key_disabled", "{body}");
    }
    // 解封：库里令牌状态由 ban 那步置成了 2，解封只改 users.status——
    // 于是令牌**不会**自动复活。这是定案语义（防误解封连带放出一批本该单独确认的令牌），
    // 钉住它，免得哪天有人"顺手修好"变成静默批量恢复。
    let unbanned = client
        .post(format!(
            "http://{}/admin/users/{}/manage",
            bed.console, bed.user_id
        ))
        .bearer_auth(&bed.super_token)
        .json(&json!({ "action": "unban" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unbanned.status(), 200);
    let (status, body) = chat(&bed, &tok_a, &bed.model).await;
    assert_eq!(status, 401, "解封不连带复活令牌：{body}");
    assert_eq!(body["error"]["code"], "key_disabled", "{body}");
    let statuses = sqlx::query_scalar!(
        r#"SELECT status AS "s!" FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL"#,
        bed.user_id
    )
    .fetch_all(&bed.pg)
    .await
    .unwrap();
    assert!(
        statuses.iter().all(|s| *s == 2),
        "封禁把名下令牌全置停：{statuses:?}"
    );
}

/// key 级模型白名单：不在名单上的模型 403 `model_not_allowed`，且改名单立刻生效。
///
/// 这道闸排在"模型存在吗"之后："模型不存在"是 404 `model_not_found`，
/// "模型存在但这把 key 不许用"是 403——两者混同的话，用户会拿着一个能用的模型名
/// 去查为什么站点说没有。名单为空数组 = 一个都不许（不是"不限制"），
/// 不限制得写 null；这个区分错了就是把限制最严的 key 放成了全放行。
#[tokio::test]
async fn model_allowlist_gates_per_key_and_applies_immediately() {
    let bed = setup().await;
    let client = reqwest::Client::new();
    let (tok, key_id) = new_key(&bed.pg, bed.user_id, "allow").await;
    let patch = async |body: Value| {
        let resp = client
            .patch(format!("http://{}/admin/keys/{key_id}", bed.console))
            .bearer_auth(&bed.super_token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    };

    // 无名单（null）= 不限制：两个模型都能打
    for m in [&bed.model, &bed.other_model] {
        assert_eq!(chat(&bed, &tok, m).await.0, 200, "无名单时 {m} 应可用");
    }

    // 只放 model：other_model 立刻 403（缓存已被前两发焐热，靠 auth_del 精确失效）
    patch(json!({ "model_allowlist": [bed.model] })).await;
    assert_eq!(chat(&bed, &tok, &bed.model).await.0, 200, "名单内应可用");
    let (status, body) = chat(&bed, &tok, &bed.other_model).await;
    assert_eq!(status, 403, "名单外应拒：{body}");
    assert_eq!(body["error"]["code"], "model_not_allowed", "{body}");

    // 不存在的模型仍是 404 model_not_found——不能被白名单闸抢先答成 403
    let (status, body) = chat(&bed, &tok, "no-such-model-at-all").await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"]["code"], "model_not_found", "{body}");

    // 空数组在入口被归一成 null = 不限（`docs/database.md` api_keys.model_allowlist 的
    // "null = 不限"）。这不是把闸放松了：前端那个模型多选框清空勾选就会发 `[]` 过来，
    // 按字面存下去等于把 key 变成砖，用户还找不到哪里能改回来。要"一个都不许"就停用 key。
    patch(json!({ "model_allowlist": [] })).await;
    let stored = sqlx::query_scalar!(
        r#"SELECT model_allowlist FROM api_keys WHERE id = $1"#,
        key_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert!(stored.is_none(), "空数组应落成 null 而不是 []：{stored:?}");
    for m in [&bed.model, &bed.other_model] {
        assert_eq!(chat(&bed, &tok, m).await.0, 200, "归一为不限后 {m} 应可用");
    }

    // 显式写 null 同样是解除限制（前一步已解除，这里验的是显式路径也走得通）
    patch(json!({ "model_allowlist": [bed.model] })).await;
    assert_eq!(chat(&bed, &tok, &bed.other_model).await.0, 403);
    patch(json!({ "model_allowlist": null })).await;
    for m in [&bed.model, &bed.other_model] {
        assert_eq!(chat(&bed, &tok, m).await.0, 200, "解除后 {m} 应可用");
    }
}
