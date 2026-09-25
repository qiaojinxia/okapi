//! 渠道写接口的返回内容与副作用：改凭据、复制、批量。
//!
//! 逐接口端到端探针（把某个接口的 2xx 响应体换成错误内容，看有没有用例察觉）对这三个接口都全绿：
//! 改凭据回的 `channel_key_id`、复制回的新 `id` 与 `status`、批量回的 `affected` 从没被核对过；
//! 改凭据的副作用——新凭据是否加密落库、网关是否真的换用它、key 状态机是否复位——也没有任何用例验过。
//! 渠道写接口原本的用例在 `console_manage.rs`（并行会话正在改），故另起此文件。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。主密钥用固定的测试值直接放进 state（同 `channel_credential`、
//! `console_auth_web`），不读 `OKAPI_MASTER_KEY`：CI 不设这个变量，读环境变量的写法本地能过、CI 必红。
//! 查询一律用运行期检查的 `sqlx::query*`：CI 以 `SQLX_OFFLINE=true` 编译，测试专用的查询不值得进 `.sqlx` 缓存。

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn master_key() -> String {
    hex::encode([0x5cu8; 32])
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// 本文件的用例串行执行。`cache_flush_scopes_take_effect` 要先确认"直接改库后鉴权缓存仍在出旧值"，
/// 而鉴权缓存在 Redis 里、全局生效：任何一次 `auth_flush()`（刷缓存的 `auth` 作用域；改分组、角色、
/// 池等写接口也会触发）都会让这个前提误红，也会替"刷 auth 什么都不做"的回归把活干了。串行保证本文件
/// 以后加的用例踩不到它（同 `console_users`、`console_write_receipts`）。路由候选缓存则是每个 state
/// 各一份（测试不连 NATS，不跨实例广播），不受并行影响。
static SERIAL: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

type Seen = Arc<Mutex<Vec<String>>>;
/// `channel_keys` 回读：(密文, 状态, 失败计数, 冷却至, 最后错误)。
type KeyRow = (
    Vec<u8>,
    i16,
    i32,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<String>,
);
/// `channels` 回读：(id, 名称, 状态, 供应商, api_base, 模型)。
type ChannelRow = (i64, String, i16, String, Option<String>, Value);

/// 上游 mock：记下每次收到的 Authorization，回一个带 usage 的最小 chat 响应。
async fn mock_chat(State(seen): State<Seen>, headers: HeaderMap) -> axum::response::Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    seen.lock().unwrap().push(auth);
    Json(json!({
        "id": "c", "object": "chat.completion", "model": "up",
        "choices": [{"index": 0, "finish_reason": "stop",
                     "message": {"role": "assistant", "content": "ok"}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    }))
    .into_response()
}

struct Bed {
    pg: PgPool,
    console: SocketAddr,
    gateway: SocketAddr,
    admin_token: String,
    token: String,
    model: String,
    channel_id: i64,
    channel_key_id: i64,
    seen: Seen,
    suffix: String,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let mock = serve(
        Router::new()
            .route("/v1/chat/completions", post(mock_chat))
            .with_state(seen.clone()),
    )
    .await;
    let model = format!("ccw-m-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel(
        &pg,
        &format!("ccw-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "sk-old-credential",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("ccw-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-ccw-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-ccw")
        .await
        .unwrap();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("ccw-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = 100 WHERE id = $1")
        .bind(admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-ccw-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-ccw-adm")
        .await
        .unwrap();

    let mut state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state.master_key = Some(Arc::from(master_key().as_str()));
    state
        .ledger
        .credit(user_id, Money::from_micros(100_000_000))
        .await
        .unwrap();
    let gateway = serve(gateway::router(state.clone())).await;
    let console = serve(console::router(state)).await;
    Bed {
        pg,
        console,
        gateway,
        admin_token,
        token,
        model,
        channel_id,
        channel_key_id,
        seen,
        suffix,
    }
}

async fn admin_post(bed: &Bed, path: &str, body: Value) -> (u16, Value) {
    let r = reqwest::Client::new()
        .post(format!("http://{}{path}", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

async fn chat(bed: &Bed) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&json!({"model": bed.model, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// 改凭据：回执带 `channel_key_id`；新凭据加密落库、解开等于新值；key 状态机复位；
/// 网关下一次打上游用的就是新凭据。多 key 渠道不指明哪把 key 时 400。
#[tokio::test]
async fn rotated_credential_is_sealed_used_upstream_and_resets_key_state() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    // 先走一次网关：确认上游收到的是旧凭据，同时把候选缓存灌热。候选缓存里存的是
    // **已解封的凭据本身**（TTL 5s）。第一版没有这一步，缓存是冷的，改完第一次请求自然
    // 从库里读到新凭据——变异测试删掉轮换后的 invalidate_routing_caches()，这条用例照样绿。
    assert_eq!(chat(&bed).await, 200);
    assert_eq!(
        bed.seen.lock().unwrap().last().map(String::as_str),
        Some("Bearer sk-old-credential")
    );

    // 再把这把 key 置于"冷却中"：轮换应当连同状态机一起复位
    sqlx::query(
        "UPDATE channel_keys SET status = 3, failed_count = 5,
               cooldown_until = now() + interval '1 hour', last_error = 'upstream 401'
           WHERE id = $1",
    )
    .bind(bed.channel_key_id)
    .execute(&bed.pg)
    .await
    .unwrap();

    let fresh = format!("sk-new-{}", Uuid::new_v4().simple());
    let (status, body) = admin_post(
        &bed,
        &format!("/admin/channels/{}/credential", bed.channel_id),
        json!({"credential": fresh}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({"ok": true, "channel_key_id": bed.channel_key_id}),
        "回执应指明轮换的是哪把 key"
    );

    let (ciphertext, status, failed_count, cooldown_until, last_error): KeyRow = sqlx::query_as(
        "SELECT credential_ciphertext, status, failed_count, cooldown_until, last_error
           FROM channel_keys WHERE id = $1",
    )
    .bind(bed.channel_key_id)
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert!(
        okapi_store::credential::is_sealed(&ciphertext),
        "新凭据必须加密落库，不能是明文"
    );
    let master = master_key();
    assert_eq!(
        okapi_store::credential::open(Some(&master), &ciphertext).unwrap(),
        fresh,
        "解开后应等于新凭据"
    );
    assert_eq!(
        (status, failed_count, cooldown_until, last_error),
        (1, 0, None, None),
        "轮换应复位 key 状态机：启用、失败计数清零、冷却与最后错误清空"
    );

    // 端到端：缓存是热的，网关下一次打上游，带的也必须是新凭据
    assert_eq!(chat(&bed).await, 200);
    assert_eq!(
        bed.seen.lock().unwrap().last().map(String::as_str),
        Some(format!("Bearer {fresh}").as_str()),
        "上游收到的仍是旧凭据——轮换没有生效到数据面"
    );

    // 多 key 渠道：不指明轮换哪把 → 400，而不是随便挑一把
    sqlx::query("INSERT INTO channel_keys (channel_id, credential_ciphertext) VALUES ($1, $2)")
        .bind(bed.channel_id)
        .bind(b"sk-second".as_slice())
        .execute(&bed.pg)
        .await
        .unwrap();
    let (status, body) = admin_post(
        &bed,
        &format!("/admin/channels/{}/credential", bed.channel_id),
        json!({"credential": "sk-whichever"}),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "channel_key_id");
}

/// 复制渠道：回执是新渠道的 `id` 与停用状态；新渠道确实落库、默认停用、配置与原渠道一致。
#[tokio::test]
async fn duplicate_returns_a_disabled_copy() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let name = format!("ccw-copy-{}", bed.suffix);
    let (status, body) = admin_post(
        &bed,
        &format!("/admin/channels/{}/duplicate", bed.channel_id),
        json!({"name": name}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let new_id = body["id"].as_i64().expect("回执应带新渠道 id");
    assert_ne!(new_id, bed.channel_id);
    assert_eq!(
        body["status"], 2,
        "复制出来的渠道默认停用，免得未经检查就接流量"
    );

    let pair: Vec<ChannelRow> = sqlx::query_as(
        "SELECT id, name, status, provider, api_base, models
           FROM channels WHERE id = ANY($1) ORDER BY id",
    )
    .bind(vec![bed.channel_id, new_id])
    .fetch_all(&bed.pg)
    .await
    .unwrap();
    assert_eq!(pair.len(), 2, "新渠道必须落库");
    let (src, copy) = (&pair[0], &pair[1]);
    assert_eq!(copy.0, new_id);
    assert_eq!(copy.1, name);
    assert_eq!(copy.2, 2);
    assert_eq!(
        (&copy.3, &copy.4, &copy.5),
        (&src.3, &src.4, &src.5),
        "复制应保留原渠道的配置"
    );

    let (status, body) = admin_post(
        &bed,
        &format!("/admin/channels/{}/duplicate", bed.channel_id),
        json!({"name": "   "}),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "name");
}

/// 批量：`affected` 只计真正改动的行，不存在的 id 不算；状态确实落库。
#[tokio::test]
async fn batch_reports_rows_actually_changed() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let (second, _) = okapi_store::provision::create_channel(
        &bed.pg,
        &format!("ccw-ch2-{}", bed.suffix),
        "openai",
        "http://127.0.0.1:1/v1",
        "cred",
        &[bed.model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    let ids = json!([bed.channel_id, second, 999_999_999_i64]);

    let (status, body) = admin_post(
        &bed,
        "/admin/channels/batch",
        json!({"ids": ids, "action": "disable"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["affected"], 2, "不存在的 id 不应计入：{body}");
    let statuses: Vec<i16> =
        sqlx::query_scalar("SELECT status FROM channels WHERE id = ANY($1) ORDER BY id")
            .bind(vec![bed.channel_id, second])
            .fetch_all(&bed.pg)
            .await
            .unwrap();
    assert_eq!(statuses, vec![2, 2]);

    let (status, body) = admin_post(
        &bed,
        "/admin/channels/batch",
        json!({"ids": ids, "action": "enable"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["affected"], 2);

    let (status, body) = admin_post(
        &bed,
        "/admin/channels/batch",
        json!({"ids": ids, "action": "explode"}),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "action");
}

/// 删渠道：该渠道立刻不再接流量，库里是软删除。
///
/// 副作用变异（handler 照常回 ok、跳过软删除）此前全绿——删掉的渠道照样接流量，没有用例察觉。
/// 先请求一次把候选缓存灌热，于是"删完没清路由缓存"的回归也会在这里红。
#[tokio::test]
async fn deleted_channel_stops_serving_immediately() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    assert_eq!(chat(&bed).await, 200, "删除前应可用（顺带灌热候选缓存）");

    let r = reqwest::Client::new()
        .delete(format!(
            "http://{}/admin/channels/{}",
            bed.console, bed.channel_id
        ))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .json(&json!({"model": bed.model, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    assert_ne!(status, 200, "删掉的渠道仍在接流量：{body}");
    assert_eq!(body["error"]["code"], "no_available_channel", "{body}");

    let deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM channels WHERE id = $1")
            .bind(bed.channel_id)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert!(deleted, "应为软删除（deleted_at 已置）");
}

/// 手动刷缓存的两个作用域都要真的生效。
///
/// 副作用变异把 `auth` / `routing` 两个分支改成空操作，此前全绿——只有 `pricebook` 分支有用例。
/// 用"直接改库"制造缓存与库不一致（这条路径绕开管理端自带的清缓存，有 TTL 滞后是设计，
/// 见第三十轮）；先断言缓存确实还在出旧值，再刷缓存，断言立刻变成新值。
#[tokio::test]
async fn cache_flush_scopes_take_effect() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let flush = |scope: &'static str| {
        let url = format!("http://{}/admin/cache/flush", bed.console);
        let token = bed.admin_token.clone();
        async move {
            let r = reqwest::Client::new()
                .post(url)
                .bearer_auth(token)
                .json(&json!({"scope": scope}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200, "flush {scope}");
        }
    };
    let last_auth = || bed.seen.lock().unwrap().last().cloned().unwrap_or_default();

    // routing：候选缓存里存着已解封的凭据
    assert_eq!(chat(&bed).await, 200);
    assert_eq!(last_auth(), "Bearer sk-old-credential");
    sqlx::query("UPDATE channel_keys SET credential_ciphertext = $2 WHERE id = $1")
        .bind(bed.channel_key_id)
        .bind(b"sk-direct-new".as_slice())
        .execute(&bed.pg)
        .await
        .unwrap();
    assert_eq!(chat(&bed).await, 200);
    assert_eq!(
        last_auth(),
        "Bearer sk-old-credential",
        "前提：直接改库后候选缓存仍在出旧凭据"
    );
    flush("routing").await;
    assert_eq!(chat(&bed).await, 200);
    assert_eq!(
        last_auth(),
        "Bearer sk-direct-new",
        "刷 routing 后网关仍在用旧凭据——这个作用域没有生效"
    );

    // auth：鉴权缓存里存着 key 的状态
    sqlx::query("UPDATE api_keys SET status = 2 WHERE key_hash = $1")
        .bind(hash(&bed.token))
        .execute(&bed.pg)
        .await
        .unwrap();
    assert_eq!(
        chat(&bed).await,
        200,
        "前提：直接改库后鉴权缓存仍认为 key 可用"
    );
    flush("auth").await;
    assert_eq!(
        chat(&bed).await,
        401,
        "刷 auth 后停用的 key 仍能用——这个作用域没有生效"
    );
}
