//! key 级 IP 白名单验收（IMPLEMENTATION §11.17）：`api_keys.ip_allowlist` 此前只在库里、
//! 网关从不读。现在：来源 IP = CDN 头 → 中间件写入的对端地址；配了名单必须命中，拿不到
//! IP 按不在名单上处理；客户端伪造内部头无效；门户 PATCH 校验每条可解析并立即生效；
//! 白名单只约束 `/v1/*` 数据面——用同一把 key 登录门户不受限（否则用户会把自己锁在门外）。
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
    model: String,
    /// (token, key_id) × 三把：CIDR 白名单 / 无白名单 / 只放行本机对端
    cidr_key: (String, i64),
    open_key: (String, i64),
    loopback_key: (String, i64),
    gateway: SocketAddr,
    console: SocketAddr,
}

async fn key_with_allowlist(
    pg: &PgPool,
    user_id: i64,
    tag: &str,
    list: Option<Value>,
) -> (String, i64) {
    let token = format!("sk-okapi-ip-{tag}-{}", Uuid::new_v4().simple());
    let id = okapi_store::provision::create_api_key(pg, user_id, &hash(&token), "sk-ip")
        .await
        .unwrap();
    sqlx::query!(
        "UPDATE api_keys SET ip_allowlist = $2 WHERE id = $1",
        id,
        list
    )
    .execute(pg)
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
    let model = format!("ip-m-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1")
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &pg,
        &format!("ip-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("ip-u-{suffix}"))
        .await
        .unwrap();
    let cidr_key = key_with_allowlist(
        &pg,
        user_id,
        "cidr",
        Some(json!(["203.0.113.0/24", "2001:db8::/32"])),
    )
    .await;
    let open_key = key_with_allowlist(&pg, user_id, "open", None).await;
    let loopback_key = key_with_allowlist(&pg, user_id, "lo", Some(json!(["127.0.0.1"]))).await;

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(50_000_000))
        .await
        .unwrap();
    // 网关按生产形态挂 connect info：对端地址经中间件写成内部头
    let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = gw_listener.local_addr().unwrap();
    let gw_app = gateway::router(state.clone());
    tokio::spawn(async move {
        axum::serve(
            gw_listener,
            gw_app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
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
        cidr_key,
        open_key,
        loopback_key,
        gateway: gateway_addr,
        console: console_addr,
    }
}

async fn chat(bed: &Bed, token: &str, headers: &[(&str, &str)]) -> (u16, Value) {
    let mut req = reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(token)
        .json(&json!({"model": bed.model, "stream": false,
                      "messages": [{"role": "user", "content": "hello"}]}));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().await.unwrap();
    (
        resp.status().as_u16(),
        resp.json::<Value>().await.unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn allowlist_enforced_with_cdn_header_and_peer_fallback() {
    let bed = setup().await;
    let cidr = bed.cidr_key.0.as_str();

    // CDN 头在名单内 → 放行；名单外 → 403 且 param 回显识别到的 IP
    let (status, _) = chat(&bed, cidr, &[("x-real-ip", "203.0.113.9")]).await;
    assert_eq!(status, 200);
    let (status, body) = chat(&bed, cidr, &[("x-real-ip", "198.51.100.1")]).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "ip_not_allowed");
    assert_eq!(body["error"]["param"], "198.51.100.1");
    // IPv6 CIDR
    let (status, _) = chat(&bed, cidr, &[("x-real-ip", "2001:db8:1::7")]).await;
    assert_eq!(status, 200);
    // x-forwarded-for 取最右非信任跳（§14.2）：反代追加的那一段才是它亲眼看到的对端
    let (status, _) = chat(&bed, cidr, &[("x-forwarded-for", "10.0.0.1, 203.0.113.5")]).await;
    assert_eq!(status, 200);
    // 反过来就是伪造：调用方把名单内地址写进链首，真实对端 198.51.100.9 由反代追加在最右
    let (status, body) = chat(
        &bed,
        cidr,
        &[("x-forwarded-for", "203.0.113.5, 198.51.100.9")],
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        body["error"]["param"], "198.51.100.9",
        "认最右跳，不认链首自述"
    );

    // 无 CDN 头：来源 = 对端 socket（127.0.0.1）→ 不在 CIDR 名单 → 403；
    // 客户端伪造的内部头被中间件剥掉，不能借此冒充名单内地址
    let (status, body) = chat(&bed, cidr, &[]).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["param"], "127.0.0.1", "识别到的是真实对端");
    let (status, _) = chat(&bed, cidr, &[("x-okapi-peer-ip", "203.0.113.9")]).await;
    assert_eq!(status, 403, "伪造内部头无效");

    // 只放行本机对端的 key：无任何头、靠 connect info 兜底 → 200
    let (status, body) = chat(&bed, &bed.loopback_key.0, &[]).await;
    assert_eq!(status, 200, "{body}");

    // 无白名单的 key 不受影响
    let (status, _) = chat(&bed, &bed.open_key.0, &[("x-real-ip", "198.51.100.1")]).await;
    assert_eq!(status, 200);
}

/// 门户自助改白名单：非法条目 400 带 param；合法条目写入后鉴权缓存立即失效；null 解除。
#[tokio::test]
async fn portal_patch_validates_and_applies_immediately() {
    let bed = setup().await;
    let (token, key_id) = &bed.open_key;
    let client = reqwest::Client::new();
    let patch = |body: Value| {
        let client = client.clone();
        let url = format!("http://{}/api/me/keys/{key_id}", bed.console);
        let token = token.clone();
        async move {
            let resp = client
                .patch(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap();
            (
                resp.status().as_u16(),
                resp.json::<Value>().await.unwrap_or(Value::Null),
            )
        }
    };

    let (status, body) = patch(json!({"ip_allowlist": ["10.0.0.0/8", "example.com"]})).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "ip_allowlist:example.com");

    // 先用 open key 打通一次（鉴权缓存里有它），再改名单 → 下一次请求必须按新名单判定
    let (status, _) = chat(&bed, token, &[("x-real-ip", "198.51.100.1")]).await;
    assert_eq!(status, 200);
    let (status, _) = patch(json!({"ip_allowlist": ["203.0.113.0/24", " 203.0.113.0/24 "]})).await;
    assert_eq!(status, 200);
    let stored = sqlx::query_scalar!(r#"SELECT ip_allowlist FROM api_keys WHERE id = $1"#, key_id)
        .fetch_one(&bed.pg)
        .await
        .unwrap();
    assert_eq!(stored, Some(json!(["203.0.113.0/24"])), "去空白去重");
    let (status, body) = chat(&bed, token, &[("x-real-ip", "198.51.100.1")]).await;
    assert_eq!(status, 403, "改后立即生效：{body}");
    let (status, _) = chat(&bed, token, &[("x-real-ip", "203.0.113.77")]).await;
    assert_eq!(status, 200);

    // 门户列表透出名单；名单外来源照常能用门户 API（白名单只管数据面）；置 null 解除
    let list: Value = client
        .get(format!("http://{}/api/me/keys", bed.console))
        .bearer_auth(token)
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
        .find(|k| k["id"] == *key_id)
        .unwrap();
    assert_eq!(mine["ip_allowlist"], json!(["203.0.113.0/24"]));
    let (status, _) = patch(json!({"ip_allowlist": null})).await;
    assert_eq!(status, 200);
    let (status, _) = chat(&bed, token, &[("x-real-ip", "198.51.100.1")]).await;
    assert_eq!(status, 200, "解除后任意来源可用");
    let _ = bed.user_id;
}
