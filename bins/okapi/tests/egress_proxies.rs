//! 出口代理验收（IMPLEMENTATION §11.41）：代理 / 代理组的管理面、固定分配与容量、轮换、
//! 被动熔断、全局默认出口、删除保护、属主范围——全部经控制台与网关端到端走一遍。
//!
//! 主密钥用固定测试值放进 state（同 `console_channel_writes`），不读环境变量。
//! 查询一律用运行期检查的 `sqlx::query*`：测试专用的查询不进 `.sqlx` 缓存。
//! 全局默认出口是全站状态：本文件串行执行，改过默认的用例结束前改回直连。

#[path = "support/published_pricing.rs"]
mod published_pricing;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

static SERIAL: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn master_key() -> String {
    hex::encode([0x6du8; 32])
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

/// 上游 mock：计数并回一个带 usage 的最小 chat 响应。
async fn spawn_upstream(hits: Arc<AtomicUsize>) -> SocketAddr {
    serve(Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({
                    "id": "c", "object": "chat.completion", "model": "up",
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "ok"}}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                }))
                .into_response()
            }
        }),
    ))
    .await
}

/// 最小 HTTP 正向代理：把绝对 URI 原样转发，计数经手次数。
async fn spawn_proxy(hits: Arc<AtomicUsize>) -> SocketAddr {
    serve(Router::new().fallback(move |req: Request| {
        let hits = Arc::clone(&hits);
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            let method = req.method().clone();
            let uri = req.uri().to_string();
            let headers = req.headers().clone();
            let body = to_bytes(req.into_body(), 16 * 1024 * 1024)
                .await
                .unwrap_or_default();
            let mut forward = reqwest::Client::new().request(method, uri);
            for (k, v) in &headers {
                if k != header::HOST && k != header::CONNECTION {
                    forward = forward.header(k, v);
                }
            }
            match forward.body(body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
                    let mut out = Response::new(Body::from(resp.bytes().await.unwrap_or_default()));
                    *out.status_mut() = status;
                    if let Some(ct) = content_type {
                        out.headers_mut().insert(header::CONTENT_TYPE, ct);
                    }
                    out
                }
                Err(_) => StatusCode::BAD_GATEWAY.into_response(),
            }
        }
    }))
    .await
}

/// 原始 TCP 代理：CONNECT 建隧道、绝对 URI 原样转发。目标连不上时直接断开——不少代理软件就是
/// 这样报「目标不可达」的，客户端看到的和「代理自己坏了」一模一样。
async fn spawn_tunnel_proxy() -> SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0u8; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match client.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&head)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let mut parts = line.split(' ');
                let (method, target) = (
                    parts.next().unwrap_or_default(),
                    parts.next().unwrap_or_default(),
                );
                let authority = if method == "CONNECT" {
                    target.to_owned()
                } else {
                    target
                        .trim_start_matches("http://")
                        .split('/')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                };
                let Ok(mut upstream) = tokio::net::TcpStream::connect(&authority).await else {
                    return;
                };
                let ready = if method == "CONNECT" {
                    client
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                } else {
                    upstream.write_all(&head).await
                };
                if ready.is_ok() {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                }
            });
        }
    });
    addr
}

/// 坏掉的代理：TCP 接得上，读到请求就断开。
async fn spawn_broken_proxy() -> SocketAddr {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
        }
    });
    addr
}

/// 一个此刻没人监听的地址（占住再放掉）。
async fn dead_addr() -> SocketAddr {
    tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
}

struct Bed {
    pg: PgPool,
    console: SocketAddr,
    gateway: SocketAddr,
    admin_token: String,
    token: String,
    model: String,
    /// 第二个模型：只挂在个别渠道上，看一个模型的故障会不会波及别的模型。
    model_b: String,
    suffix: String,
    upstream: SocketAddr,
    upstream_hits: Arc<AtomicUsize>,
}

async fn setup() -> Bed {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let upstream_hits = Arc::new(AtomicUsize::new(0));
    let upstream = spawn_upstream(Arc::clone(&upstream_hits)).await;
    let model = format!("eg-m-{suffix}");
    let model_b = format!("eg-mb-{suffix}");
    for name in [&model, &model_b] {
        okapi_store::provision::create_model_ratio(&pg, name, "1.0", "1.0", "1.0")
            .await
            .unwrap();
    }
    let user_id = okapi_store::provision::create_user(&pg, &format!("eg-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-eg-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-eg")
        .await
        .unwrap();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("eg-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = 100 WHERE id = $1")
        .bind(admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-eg-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-eg-adm")
        .await
        .unwrap();
    published_pricing::publish(&pg, admin_id).await;
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
        model_b,
        suffix,
        upstream,
        upstream_hits,
    }
}

impl Bed {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = reqwest::Client::new()
            .request(method, format!("http://{}{path}", self.console))
            .bearer_auth(token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        self.call(method, path, &self.admin_token, body).await
    }

    /// 直连上游的渠道（服务本用例的模型）。返回 (channel_id, key_id)。
    async fn channel(&self, name: &str, priority: i32) -> (i64, i64) {
        self.channel_at(name, priority, self.upstream).await
    }

    /// 指向给定上游的渠道。
    async fn channel_at(&self, name: &str, priority: i32, upstream: SocketAddr) -> (i64, i64) {
        self.channel_with(
            name,
            priority,
            &format!("http://{upstream}/v1"),
            &self.model,
        )
        .await
    }

    /// 任意 api_base、服务指定模型的渠道。
    async fn channel_with(
        &self,
        name: &str,
        priority: i32,
        api_base: &str,
        model: &str,
    ) -> (i64, i64) {
        let (channel_id, key_id) = okapi_store::provision::create_channel(
            &self.pg,
            &format!("eg-{name}-{}", self.suffix),
            "openai",
            api_base,
            "sk-upstream",
            &[model],
            true,
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE channels SET priority = $2 WHERE id = $1")
            .bind(channel_id)
            .bind(priority)
            .execute(&self.pg)
            .await
            .unwrap();
        (channel_id, key_id)
    }

    async fn proxy(&self, name: &str, addr: SocketAddr, max_keys: Option<i32>) -> i64 {
        let (status, body) = self
            .admin(
                reqwest::Method::POST,
                "/admin/proxies",
                Some(json!({"name": format!("{name}-{}", self.suffix),
                            "url": format!("http://{addr}"), "max_keys": max_keys})),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body["id"].as_i64().unwrap()
    }

    async fn bind(&self, channel_id: i64, binding: Value) -> Value {
        let (status, body) = self
            .admin(
                reqwest::Method::POST,
                &format!("/admin/channels/{channel_id}/egress"),
                Some(binding),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body
    }

    /// 每次内容都不同：会话粘性按前两条消息取键，同一句话会粘在上一次成功的 key 上，
    /// 本文件要看的是调度本身的选择。
    async fn chat(&self) -> u16 {
        self.chat_model(&self.model).await
    }

    async fn chat_model(&self, model: &str) -> u16 {
        let content = format!("hi {}", Uuid::new_v4().simple());
        let resp = reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", self.gateway))
            .bearer_auth(&self.token)
            .json(&json!({"model": model, "messages": [{"role": "user", "content": content}]}))
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let _ = resp.bytes().await;
        status
    }

    /// 本用例模型最近一条账单里的请求诊断（结算可能在后台，轮询等它落库）。
    async fn latest_diagnostics(&self) -> Value {
        for _ in 0..50 {
            let found: Option<Value> = sqlx::query_scalar(
                "SELECT usage_details->'diagnostics' FROM billing_records
                 WHERE model_name = $1 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(&self.model)
            .fetch_optional(&self.pg)
            .await
            .unwrap();
            if let Some(diagnostics) = found.filter(|d| !d.is_null()) {
                return diagnostics;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("请求诊断没有落库");
    }

    async fn assigned(&self, key_id: i64) -> Option<i64> {
        sqlx::query_scalar("SELECT egress_proxy_id FROM channel_keys WHERE id = $1")
            .bind(key_id)
            .fetch_one(&self.pg)
            .await
            .unwrap()
    }

    /// 路由诊断里这把 key 的淘汰原因。
    async fn key_reason(&self, key_id: i64) -> Value {
        let (status, body) = self
            .admin(
                reqwest::Method::GET,
                &format!("/admin/diagnose/route?model={}", self.model),
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        body["channels"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["keys"].as_array().unwrap().iter())
            .find(|k| k["key_id"] == key_id)
            .map_or(Value::Null, |k| k["reason"].clone())
    }

    async fn reset_default(&self) {
        let (status, body) = self
            .admin(
                reqwest::Method::PUT,
                "/admin/egress/default",
                Some(json!({"mode": "direct"})),
            )
            .await;
        assert_eq!(status, 200, "{body}");
    }
}

/// 代理地址含认证：校验形状、落库封信封、接口只回掩码；改名不动地址；非法状态拒绝。
#[tokio::test]
async fn proxy_crud_masks_credentials_and_seals_the_url() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let post = |body: Value| bed.admin(reqwest::Method::POST, "/admin/proxies", Some(body));
    let (status, body) = post(json!({"url": "ftp://x:21"})).await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (400, json!("url"))
    );
    let (status, body) = post(json!({"url": "http://h:1/path"})).await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (400, json!("url"))
    );
    let (status, body) =
        post(json!({"url": "socks5h://alice:s3cret@127.0.0.1:1080", "max_keys": 0})).await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (400, json!("max_keys"))
    );

    let name = format!("crud-{}", bed.suffix);
    let (status, body) =
        post(json!({"name": name, "url": " socks5h://alice:s3cret@127.0.0.1:1080 "})).await;
    assert_eq!(status, 200, "{body}");
    let id = body["id"].as_i64().unwrap();
    assert_eq!(body["url_masked"], "socks5h://alice:***@127.0.0.1:1080");

    let stored: Vec<u8> = sqlx::query_scalar("SELECT url_ciphertext FROM proxies WHERE id = $1")
        .bind(id)
        .fetch_one(&bed.pg)
        .await
        .unwrap();
    assert!(
        okapi_store::credential::is_sealed(&stored),
        "代理地址必须封信封"
    );
    assert_eq!(
        okapi_store::credential::open(Some(&master_key()), &stored).unwrap(),
        "socks5h://alice:s3cret@127.0.0.1:1080"
    );

    let resp = reqwest::Client::new()
        .get(format!("http://{}/admin/proxies?q={name}", bed.console))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!resp.contains("s3cret"), "列表不得带出密码：{resp}");
    let listed: Value = serde_json::from_str(&resp).unwrap();
    let row = &listed["data"][0];
    assert_eq!(row["id"], id);
    assert_eq!(row["url_masked"], "socks5h://alice:***@127.0.0.1:1080");
    assert_eq!(
        (row["scheme"].clone(), row["port"].clone()),
        (json!("socks5h"), json!(1080))
    );

    let path = format!("/admin/proxies/{id}");
    let (status, body) = bed
        .admin(
            reqwest::Method::PATCH,
            &path,
            Some(json!({"name": format!("{name}-2")})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let stored_after: Vec<u8> =
        sqlx::query_scalar("SELECT url_ciphertext FROM proxies WHERE id = $1")
            .bind(id)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert_eq!(stored_after, stored, "只改名不动地址");
    let (status, body) = bed
        .admin(reqwest::Method::PATCH, &path, Some(json!({"status": 3})))
        .await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (400, json!("status"))
    );
    let (status, _) = bed.admin(reqwest::Method::DELETE, &path, None).await;
    assert_eq!(status, 200);
    let (status, _) = bed.admin(reqwest::Method::DELETE, &path, None).await;
    assert_eq!(status, 404);
}

/// 渠道绑定单个代理：请求真的经它出去；代理停用后这把 key 不可调度——即便没有任何
/// 其他渠道，也绝不悄悄改走直连。
#[tokio::test]
async fn bound_proxy_is_used_and_never_falls_back_to_direct() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let proxy = bed
        .proxy("single", spawn_proxy(Arc::clone(&hits)).await, None)
        .await;
    let (channel, key) = bed.channel("single", 0).await;
    bed.bind(channel, json!({"mode": "proxy", "proxy_id": proxy}))
        .await;
    assert_eq!(bed.chat().await, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "请求必须经过绑定的代理");
    // 请求诊断记下这次尝试走的出口（管理端日志可见，门户白名单不含 attempts）
    let diagnostics = bed.latest_diagnostics().await;
    assert_eq!(
        diagnostics["attempts"][0]["egress_proxy_id"], proxy,
        "{diagnostics}"
    );

    let (status, body) = bed
        .admin(
            reqwest::Method::PATCH,
            &format!("/admin/proxies/{proxy}"),
            Some(json!({"status": 2})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let upstream_before = bed.upstream_hits.load(Ordering::SeqCst);
    assert_eq!(bed.chat().await, 503, "出口不可用 = 无可用渠道，不是直连");
    assert_eq!(
        bed.upstream_hits.load(Ordering::SeqCst),
        upstream_before,
        "上游一次都不能被直连打到"
    );
    assert_eq!(bed.key_reason(key).await, "egress_unavailable");
}

/// 固定分配：组内每把 key 分到一个代理并持久化；容量满了的 key 排队（不可调度）；
/// 放宽容量当场补分；代理停用时分配不动（等它恢复，不换 IP）；移出组才改分。
#[tokio::test]
// 分配 → 容量排队 → 放宽补分 → 停用不换 → 移出改分 → 手动改分是一条前后状态相连的场景，拆开要各自重建前序状态
#[allow(clippy::too_many_lines)]
async fn pinned_group_assigns_per_key_respects_capacity_and_waits_for_recovery() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let (hits_a, hits_b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let a = bed
        .proxy("pin-a", spawn_proxy(Arc::clone(&hits_a)).await, Some(1))
        .await;
    let b = bed
        .proxy("pin-b", spawn_proxy(Arc::clone(&hits_b)).await, Some(1))
        .await;
    let code = format!("pin-{}", bed.suffix);
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxy-groups",
            Some(json!({"code": code, "mode": "pinned",
                        "members": [{"proxy_id": a}, {"proxy_id": b}]})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let group = json!({"mode": "group", "group_code": code});
    let (c1, k1) = bed.channel("pin-1", 0).await;
    let (c2, k2) = bed.channel("pin-2", 0).await;
    let (c3, k3) = bed.channel("pin-3", 0).await;
    // 对账报告是全站口径（复用的库里可能有别的组在排队），这里核对具体 key 的分配
    assert!(bed.bind(c1, group.clone()).await["assignment"]["assigned"].as_u64() >= Some(1));
    assert!(bed.bind(c2, group.clone()).await["assignment"]["assigned"].as_u64() >= Some(1));
    let third = bed.bind(c3, group.clone()).await;
    assert!(
        third["assignment"]["unassigned"].as_u64() >= Some(1),
        "{third}"
    );
    let (p1, p2) = (bed.assigned(k1).await, bed.assigned(k2).await);
    assert!(
        p1.is_some() && p2.is_some() && p1 != p2,
        "一个代理一把 key：{p1:?} {p2:?}"
    );
    assert_eq!(bed.assigned(k3).await, None);
    assert_eq!(bed.key_reason(k3).await, "egress_unassigned");

    // 每把 key 只走自己的代理
    for _ in 0..6 {
        assert_eq!(bed.chat().await, 200);
    }
    assert_eq!(
        hits_a.load(Ordering::SeqCst) + hits_b.load(Ordering::SeqCst),
        6,
        "固定分配的 key 每次都经代理"
    );

    // 放宽容量：排队的 key 当场分到
    let (status, body) = bed
        .admin(
            reqwest::Method::PATCH,
            &format!("/admin/proxies/{a}"),
            Some(json!({"max_keys": 2})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["assignment"]["assigned"].as_u64() >= Some(1), "{body}");
    assert_eq!(bed.assigned(k3).await, Some(a));

    // 停用 b：分到 b 的 key 等它恢复，分配不动
    let on_b = if p1 == Some(b) { k1 } else { k2 };
    let (status, _) = bed
        .admin(
            reqwest::Method::PATCH,
            &format!("/admin/proxies/{b}"),
            Some(json!({"status": 2})),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(bed.assigned(on_b).await, Some(b), "停用不换 IP");
    assert_eq!(bed.key_reason(on_b).await, "egress_unavailable");

    // 把 b 移出组：它上面的 key 改分（a 容量 2 已满 → 排队）
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxy-groups",
            Some(json!({"code": code, "mode": "pinned", "members": [{"proxy_id": a}]})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["assignment"]["released"].as_u64() >= Some(1), "{body}");
    assert_eq!(bed.assigned(on_b).await, None);
    let (_, groups) = bed
        .admin(reqwest::Method::GET, "/admin/proxy-groups?limit=200", None)
        .await;
    let row = groups["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["code"] == code)
        .unwrap()
        .clone();
    assert_eq!(row["unassigned_keys"], 1, "{row}");

    // 手动改分：满了拒绝；不是组员拒绝
    let assignments = format!("/admin/proxy-groups/{code}/assignments");
    let assign = |key: i64, proxy: i64| {
        bed.admin(
            reqwest::Method::POST,
            &assignments,
            Some(json!({"key_id": key, "proxy_id": proxy})),
        )
    };
    let (status, body) = assign(on_b, a).await;
    assert_eq!(
        (status, body["error"]["code"].clone()),
        (409, json!("proxy_full"))
    );
    let (status, body) = assign(on_b, b).await;
    assert_eq!(
        (status, body["error"]["code"].clone()),
        (409, json!("proxy_not_in_group"))
    );
    let (_, list) = bed
        .admin(
            reqwest::Method::GET,
            &format!("/admin/proxy-groups/{code}/assignments"),
            None,
        )
        .await;
    assert_eq!(list["data"].as_array().unwrap().len(), 3, "{list}");
}

/// 轮换组：每次请求在健康成员里抽，流量分到所有成员（缓存的候选也逐次重抽）。
#[tokio::test]
async fn rotate_group_spreads_requests_across_members() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let (hits_a, hits_b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let a = bed
        .proxy("rot-a", spawn_proxy(Arc::clone(&hits_a)).await, None)
        .await;
    let b = bed
        .proxy("rot-b", spawn_proxy(Arc::clone(&hits_b)).await, None)
        .await;
    let code = format!("rot-{}", bed.suffix);
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxy-groups",
            Some(json!({"code": code, "mode": "rotate",
                        "members": [{"proxy_id": a, "weight": 1}, {"proxy_id": b, "weight": 1}]})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (channel, key) = bed.channel("rot", 0).await;
    bed.bind(channel, json!({"mode": "group", "group_code": code}))
        .await;
    assert_eq!(bed.assigned(key).await, None, "轮换组不做固定分配");
    for _ in 0..24 {
        assert_eq!(bed.chat().await, 200);
    }
    let (ha, hb) = (hits_a.load(Ordering::SeqCst), hits_b.load(Ordering::SeqCst));
    assert_eq!(ha + hb, 24);
    assert!(ha > 0 && hb > 0, "两个成员都该分到流量：{ha} / {hb}");
}

/// 代理死了：连接失败记在代理上（连续 3 次进冷却），key 健康不受影响；
/// 请求照常改投其他渠道；换成能用的地址即解除熔断、重新经它出去。
#[tokio::test]
async fn dead_proxy_trips_its_own_breaker_without_touching_the_key() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let proxy = bed.proxy("dead", dead_addr().await, None).await;
    let (bound, key) = bed.channel("dead", 10).await;
    let (_fallback, _) = bed.channel("direct", 0).await;
    bed.bind(bound, json!({"mode": "proxy", "proxy_id": proxy}))
        .await;
    for _ in 0..3 {
        assert_eq!(bed.chat().await, 200, "改投直连的低优先级渠道");
    }
    let (failed, cooling): (i32, bool) = sqlx::query_as(
        "SELECT failed_count, COALESCE(cooldown_until > now(), false) FROM proxies WHERE id = $1",
    )
    .bind(proxy)
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!((failed, cooling), (3, true));
    let (key_status, key_failed): (i16, i32) =
        sqlx::query_as("SELECT status, failed_count FROM channel_keys WHERE id = $1")
            .bind(key)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert_eq!((key_status, key_failed), (1, 0), "连接失败不算 key 的错");
    assert_eq!(bed.key_reason(key).await, "egress_cooling");

    // 换成活的代理：熔断清掉，下一次就经它出去
    let hits = Arc::new(AtomicUsize::new(0));
    let live = spawn_proxy(Arc::clone(&hits)).await;
    let (status, body) = bed
        .admin(
            reqwest::Method::PATCH,
            &format!("/admin/proxies/{proxy}"),
            Some(json!({"url": format!("http://{live}")})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(bed.key_reason(key).await.is_null());
    assert_eq!(bed.chat().await, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "高优先级渠道恢复经代理出站");
}

/// 被引用的代理 / 组不能删；全局默认只能经专用端点写，被它引用的同样不能删；
/// 继承默认的渠道随默认走，显式直连的不受影响。
#[tokio::test]
// 设默认 → 继承生效 → 删除保护 → 改回直连释放 → 解绑后可删，同一组代理与渠道贯穿始终
#[allow(clippy::too_many_lines)]
async fn default_egress_applies_to_inheriting_channels_and_guards_deletes() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let proxy = bed
        .proxy("dflt", spawn_proxy(Arc::clone(&hits)).await, None)
        .await;
    let code = format!("dflt-{}", bed.suffix);
    let (status, _) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxy-groups",
            Some(json!({"code": code, "mode": "pinned", "members": [{"proxy_id": proxy}]})),
        )
        .await;
    assert_eq!(status, 200);

    // 通用设置端点写不了全局默认
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/settings",
            Some(json!({"key": "egress_default", "value": {"mode": "proxy", "proxy_id": proxy}})),
        )
        .await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (400, json!("egress_default"))
    );
    let (status, body) = bed
        .admin(
            reqwest::Method::PUT,
            "/admin/egress/default",
            Some(json!({"mode": "proxy", "proxy_id": i64::MAX})),
        )
        .await;
    assert_eq!(status, 404, "{body}");

    let (inherit, inherit_key) = bed.channel("inherit", 0).await;
    let (direct, _) = bed.channel("explicit-direct", 0).await;
    bed.bind(direct, json!({"mode": "direct"})).await;
    let (status, body) = bed
        .admin(
            reqwest::Method::PUT,
            "/admin/egress/default",
            Some(json!({"mode": "group", "group_code": code})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        bed.assigned(inherit_key).await,
        Some(proxy),
        "继承默认的 key 当场分到"
    );
    // 只留继承的那条渠道可用，看它是否经默认出口
    sqlx::query("UPDATE channels SET status = 2 WHERE id = $1")
        .bind(direct)
        .execute(&bed.pg)
        .await
        .unwrap();
    bed.bind(inherit, json!({"mode": "inherit"})).await;
    assert_eq!(bed.chat().await, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "继承默认出口");

    // 被引用不能删
    let (status, body) = bed
        .admin(
            reqwest::Method::DELETE,
            &format!("/admin/proxy-groups/{code}"),
            None,
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].clone()),
        (409, json!("proxy_group_is_default"))
    );
    bed.reset_default().await;
    assert_eq!(
        bed.assigned(inherit_key).await,
        None,
        "默认改回直连即释放分配"
    );
    bed.bind(inherit, json!({"mode": "proxy", "proxy_id": proxy}))
        .await;
    let (status, body) = bed
        .admin(
            reqwest::Method::DELETE,
            &format!("/admin/proxies/{proxy}"),
            None,
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].clone()),
        (409, json!("proxy_in_use"))
    );
    bed.bind(inherit, json!({"mode": "group", "group_code": code}))
        .await;
    let (status, body) = bed
        .admin(
            reqwest::Method::DELETE,
            &format!("/admin/proxy-groups/{code}"),
            None,
        )
        .await;
    assert_eq!(
        (status, body["error"]["code"].clone()),
        (409, json!("proxy_group_in_use"))
    );
    // 解绑后可删；组员随组删除，key 分配随之释放
    bed.bind(inherit, json!({"mode": "direct"})).await;
    let (status, _) = bed
        .admin(
            reqwest::Method::DELETE,
            &format!("/admin/proxy-groups/{code}"),
            None,
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = bed
        .admin(
            reqwest::Method::DELETE,
            &format!("/admin/proxies/{proxy}"),
            None,
        )
        .await;
    assert_eq!(status, 200);
}

/// own 范围的渠道管理员：只看得见、绑得上自己的代理；全局默认要 all 范围。
#[tokio::test]
async fn own_scope_admin_only_sees_and_binds_its_own_proxies() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let foreign = bed.proxy("root", dead_addr().await, None).await;
    let role_code = format!("eg_own_{}", bed.suffix);
    let (status, role) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/roles",
            Some(json!({"role_code": role_code, "display_name": "渠道管理员",
                        "permissions": ["channel.read.own", "channel.write.own"]})),
        )
        .await;
    assert_eq!(status, 200, "{role}");
    let own_admin = okapi_store::provision::create_user(&bed.pg, &format!("eg-own-{}", bed.suffix))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = 10, admin_role_id = $2 WHERE id = $1")
        .bind(own_admin)
        .bind(role["admin_role_id"].as_i64().unwrap())
        .execute(&bed.pg)
        .await
        .unwrap();
    let own_token = format!("sk-okapi-eg-own-{}", bed.suffix);
    okapi_store::provision::create_api_key(&bed.pg, own_admin, &hash(&own_token), "sk-eg-own")
        .await
        .unwrap();
    let (channel, _) = bed.channel("own", 0).await;
    okapi_store::admin::set_channel_owner(&bed.pg, channel, own_admin)
        .await
        .unwrap();

    let (status, listed) = bed
        .call(
            reqwest::Method::GET,
            "/admin/proxies?limit=200",
            &own_token,
            None,
        )
        .await;
    assert_eq!(status, 200, "{listed}");
    assert!(
        listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["id"] != foreign),
        "看不到别人的代理：{listed}"
    );
    let path = format!("/admin/channels/{channel}/egress");
    let (status, body) = bed
        .call(
            reqwest::Method::POST,
            &path,
            &own_token,
            Some(json!({"mode": "proxy", "proxy_id": foreign})),
        )
        .await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (403, json!("owner"))
    );
    let (status, created) = bed
        .call(
            reqwest::Method::POST,
            "/admin/proxies",
            &own_token,
            Some(json!({"url": "http://10.1.2.3:3128"})),
        )
        .await;
    assert_eq!(status, 200, "{created}");
    let (status, body) = bed
        .call(
            reqwest::Method::POST,
            &path,
            &own_token,
            Some(json!({"mode": "proxy", "proxy_id": created["id"]})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = bed
        .call(
            reqwest::Method::PUT,
            "/admin/egress/default",
            &own_token,
            Some(json!({"mode": "direct"})),
        )
        .await;
    assert_eq!(status, 403, "{body}");
}

/// 慢上游：请求体里带 `slow` 的先睡一会儿再回，用来占住并发位。
async fn spawn_slow_upstream(hits: Arc<AtomicUsize>) -> SocketAddr {
    serve(Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::body::Bytes| {
            let hits = Arc::clone(&hits);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                if String::from_utf8_lossy(&body).contains("slow") {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                }
                axum::Json(json!({
                    "id": "c", "object": "chat.completion", "model": "up",
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "ok"}}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                }))
                .into_response()
            }
        }),
    ))
    .await
}

/// 单代理并发上限跨渠道、跨 key 共享：两条渠道走同一个代理（上限 1），一个请求占着时，
/// 另一个请求的两条代理渠道都按「忙」跳过、改投直连渠道；代理始终只经手一个在途请求。
#[tokio::test]
async fn proxy_concurrency_cap_is_shared_across_channels() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let upstream_hits = Arc::new(AtomicUsize::new(0));
    let slow = spawn_slow_upstream(Arc::clone(&upstream_hits)).await;
    let proxy_hits = Arc::new(AtomicUsize::new(0));
    let proxy = bed
        .proxy("cap", spawn_proxy(Arc::clone(&proxy_hits)).await, None)
        .await;
    let (status, body) = bed
        .admin(
            reqwest::Method::PATCH,
            &format!("/admin/proxies/{proxy}"),
            Some(json!({"max_concurrency": 1})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (a, _) = bed.channel_at("cap-a", 10, slow).await;
    let (b, _) = bed.channel_at("cap-b", 10, slow).await;
    bed.channel_at("cap-direct", 0, slow).await;
    for channel in [a, b] {
        bed.bind(channel, json!({"mode": "proxy", "proxy_id": proxy}))
            .await;
    }
    let send = |content: &'static str| {
        let (gateway, token, model) = (bed.gateway, bed.token.clone(), bed.model.clone());
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://{gateway}/v1/chat/completions"))
                .bearer_auth(token)
                .json(&json!({"model": model,
                              "messages": [{"role": "user", "content": content}]}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        })
    };
    let first = send("slow first");
    // 等第一个请求经代理到达上游、占住代理的并发位
    for _ in 0..50 {
        if proxy_hits.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 1);
    let second = send("second while busy");
    assert_eq!(second.await.unwrap(), 200, "代理满了改投直连渠道");
    assert_eq!(
        proxy_hits.load(Ordering::SeqCst),
        1,
        "代理在第一个请求完成前不能再经手"
    );
    assert_eq!(first.await.unwrap(), 200);
    assert_eq!(upstream_hits.load(Ordering::SeqCst), 2);
    let (status, body) = bed
        .admin(
            reqwest::Method::PATCH,
            &format!("/admin/proxies/{proxy}"),
            Some(json!({"max_concurrency": 0})),
        )
        .await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (400, json!("max_concurrency"))
    );
}

/// 批量导入：完整 URL 与代理商格式都认，密码按原文编码；同批与已有的按地址查重；
/// 认不出的逐行报原因；可选直接加进代理组并当场对账。
#[tokio::test]
async fn import_accepts_vendor_formats_dedupes_and_joins_a_group() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let code = format!("imp-{}", bed.suffix);
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxy-groups",
            Some(json!({"code": code, "mode": "pinned", "members": []})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let host = format!("imp-{}.example.com", bed.suffix);
    let text = format!(
        "# 香港静态\nhttp://alice:pw@{host}:8001\n{host}:8002\n{host}:8003:bob:p@ss:w0rd\n\
         carol:secret@{host}:8004\n{host}:8002\nnot-a-proxy\n\nftp://{host}:21\n"
    );
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxies/import",
            Some(
                json!({"text": text, "default_scheme": "socks5h", "max_keys": 2,
                        "name_prefix": "hk", "group_code": code}),
            ),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let created = body["created"].as_array().unwrap();
    assert_eq!(created.len(), 4, "{body}");
    assert_eq!(created[0]["name"], "hk-1");
    assert_eq!(
        created[0]["url_masked"],
        format!("http://alice:***@{host}:8001")
    );
    assert_eq!(created[1]["url_masked"], format!("socks5h://{host}:8002"));
    assert_eq!(
        created[2]["url_masked"],
        format!("socks5h://bob:***@{host}:8003")
    );
    assert_eq!(
        body["skipped"],
        json!([{"line": 6, "reason": "duplicate"}, {"line": 7, "reason": "invalid"},
               {"line": 9, "reason": "invalid"}])
    );
    // 密码按原文编码后封存：解出来的 URL 能被代理客户端原样使用
    let id = created[2]["id"].as_i64().unwrap();
    let url = okapi_store::egress::proxy_url(&bed.pg, id, Some(&master_key()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(url, format!("socks5h://bob:p%40ss%3Aw0rd@{host}:8003"));
    // 进了组，容量随导入落库
    let (_, groups) = bed
        .admin(reqwest::Method::GET, "/admin/proxy-groups?limit=200", None)
        .await;
    let group = groups["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["code"] == code)
        .unwrap()
        .clone();
    let members = group["members"].as_array().unwrap();
    assert_eq!(members.len(), 4, "{group}");
    assert!(members.iter().all(|m| m["max_keys"] == 2));
    // 再导一遍：全是重复
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxies/import",
            Some(json!({"text": format!("{host}:8002\nhttp://alice:pw@{host}:8001")})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert!(body["created"].as_array().unwrap().is_empty());
    assert_eq!(
        body["skipped"],
        json!([{"line": 1, "reason": "duplicate"}, {"line": 2, "reason": "duplicate"}])
    );
    let (status, body) = bed
        .admin(
            reqwest::Method::POST,
            "/admin/proxies/import",
            Some(json!({"text": "\n# nothing\n"})),
        )
        .await;
    assert_eq!(
        (status, body["error"]["param"].clone()),
        (400, json!("text"))
    );
}

/// 建一个代理行（测试直写库，绕过控制台）。
async fn insert_proxy(pg: &PgPool, name: &str, addr: SocketAddr) -> i64 {
    let url = format!("http://{addr}");
    let endpoint = okapi_providers::http::ProxyEndpoint::parse(&url).unwrap();
    okapi_store::egress::create_proxy(
        pg,
        &okapi_store::egress::NewProxy {
            name,
            url: &url,
            endpoint: okapi_store::egress::Endpoint {
                scheme: &endpoint.scheme,
                host: &endpoint.host,
                port: i32::from(endpoint.port),
                username: None,
            },
            max_keys: None,
            max_concurrency: None,
            note: None,
            status: 1,
            owner_id: None,
        },
        Some(&master_key()),
    )
    .await
    .unwrap()
}

/// 后台探测：经代理请求探测地址，记出口 IP；IP 变了记下变化前后并发 `egress_ip_changed`；
/// 在用的代理不可达发 `egress_down`（没人用的不吵）；只记事实，不碰熔断。
/// `notify_channels` / `ssrf_policy` 是全站设置，放在用完即删的临时库里。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 两轮探测前后状态相连，拆开要重建代理、渠道与通知通道
async fn background_probe_tracks_exit_ip_changes_and_alerts_on_down_proxies() {
    use okapi::worker::{egress_probe, notify};
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let admin = okapi_store::connect_pg(&database_url).await.unwrap();
    let name = format!("okapi_egp_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!(r#"CREATE DATABASE "{name}""#)))
        .execute(&admin)
        .await
        .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let temp_url = format!("{base}/{name}");
    let pg = okapi_store::connect_pg(&temp_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 探测地址：第一轮回 .10，之后回 .11
    let rounds = Arc::new(AtomicUsize::new(0));
    let target = {
        let rounds = Arc::clone(&rounds);
        serve(Router::new().route(
            "/trace",
            axum::routing::get(move || {
                let rounds = Arc::clone(&rounds);
                async move {
                    let ip = if rounds.load(Ordering::SeqCst) == 0 {
                        10
                    } else {
                        11
                    };
                    format!("fl=1\nip=203.0.113.{ip}\nloc=US\n")
                }
            }),
        ))
        .await
    };
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let sink = {
        let store = Arc::clone(&bodies);
        serve(Router::new().route(
            "/hook",
            post(move |axum::Json(v): axum::Json<Value>| {
                let store = Arc::clone(&store);
                async move {
                    store.lock().unwrap().push(v);
                    axum::Json(json!({"ok": true}))
                }
            }),
        ))
        .await
    };
    for (key, value) in [
        (
            "notify_channels",
            json!([{"type": "webhook", "url": format!("http://{sink}/hook"),
                    "events": ["egress_ip_changed", "egress_down"], "min_interval_secs": 1}]),
        ),
        (
            "ssrf_policy",
            json!({"allow_http": true, "allow_private": true}),
        ),
    ] {
        sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2)")
            .bind(key)
            .bind(value)
            .execute(&pg)
            .await
            .unwrap();
    }
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    for event in ["egress_ip_changed", "egress_down"] {
        let _: Option<i64> =
            fred::interfaces::KeysInterface::del(&redis, format!("notify:mute:0:{event}"))
                .await
                .unwrap();
    }
    let mut state = gateway::build_state(&temp_url, &redis_url, "probe-node", None, None)
        .await
        .unwrap();
    state.master_key = Some(Arc::from(master_key().as_str()));
    let notifier = notify::Notifier::new(pg.clone(), redis.clone());

    // live：活代理、渠道在用；dead：死代理、渠道在用；idle：死代理、没人用
    let live_hits = Arc::new(AtomicUsize::new(0));
    let live = insert_proxy(&pg, "live", spawn_proxy(Arc::clone(&live_hits)).await).await;
    let dead = insert_proxy(&pg, "dead", dead_addr().await).await;
    insert_proxy(&pg, "idle", dead_addr().await).await;
    for proxy in [live, dead] {
        let (channel, _) = okapi_store::provision::create_channel(
            &pg,
            &format!("egp-{proxy}"),
            "openai",
            "https://api.openai.com/v1",
            "sk-x",
            &["egp-model"],
            false,
            Some(&master_key()),
        )
        .await
        .unwrap();
        okapi_store::egress::set_channel_binding(
            &pg,
            channel,
            &okapi_store::egress::Binding::Proxy { proxy_id: proxy },
        )
        .await
        .unwrap()
        .unwrap();
    }
    let policy = egress_probe::ProbePolicy::parse(&json!({
        "target": format!("http://{target}/trace"), "concurrency": 2
    }))
    .unwrap();

    let first = egress_probe::probe_round(&state, &policy, &notifier)
        .await
        .unwrap();
    assert_eq!((first.probed, first.failed, first.changed), (3, 2, 0));
    let row: (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT exit_ip, exit_country, previous_exit_ip FROM proxies WHERE id = $1")
            .bind(live)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(row, (Some("203.0.113.10".into()), Some("US".into()), None));
    let sent: Vec<Value> = std::mem::take(&mut *bodies.lock().unwrap());
    let down = sent
        .iter()
        .find(|b| b["event"] == "egress_down")
        .expect("在用的死代理要告警");
    let names: Vec<&str> = down["payload"]["proxies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["dead"], "没人用的 idle 不告警：{down}");

    rounds.store(1, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let second = egress_probe::probe_round(&state, &policy, &notifier)
        .await
        .unwrap();
    assert_eq!(second.changed, 1);
    let row: (Option<String>, Option<String>, bool) = sqlx::query_as(
        "SELECT exit_ip, previous_exit_ip, exit_ip_changed_at IS NOT NULL FROM proxies WHERE id = $1",
    )
    .bind(live)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            Some("203.0.113.11".into()),
            Some("203.0.113.10".into()),
            true
        )
    );
    let sent: Vec<Value> = std::mem::take(&mut *bodies.lock().unwrap());
    let changed = sent
        .iter()
        .find(|b| b["event"] == "egress_ip_changed")
        .expect("出口 IP 变化要告警");
    assert_eq!(
        changed["payload"]["proxies"][0],
        json!({"proxy_id": live, "name": "live", "previous_ip": "203.0.113.10",
               "current_ip": "203.0.113.11", "channels": 1, "assigned_keys": 0})
    );
    // 只记事实：后台失败不进熔断
    let breaker: (i32, bool) =
        sqlx::query_as("SELECT failed_count, cooldown_until IS NULL FROM proxies WHERE id = $1")
            .bind(dead)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(breaker, (0, true));
    assert!(live_hits.load(Ordering::SeqCst) >= 2);

    pg.close().await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#
    )))
    .execute(&admin)
    .await;
}

/// 分不清是代理还是目标的连接失败（隧道建立中断），先由网关后台经同一代理探一次探测地址再定：
/// 探得通，代理不背锅——一个上游挂了，不能把同一代理上的其他渠道一起熔断（全局默认出口是代理时就是整站）；
/// 代理本身坏了，探测同样失败，确认后直接熔断，候选缓存当场失效，后续请求立刻绕开它。
#[tokio::test]
async fn tunnel_failures_are_verified_before_tripping_a_shared_proxy() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let trace = serve(Router::new().route(
        "/cdn-cgi/trace",
        axum::routing::get(|| async { "ip=203.0.113.9\nloc=JP\n" }),
    ))
    .await;
    // 探测地址指向本地（不出公网）；用例结束删掉，别影响同库后面的用例
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('egress_probe_policy', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(json!({"enabled": true, "interval_secs": 600,
                 "target": format!("http://{trace}/cdn-cgi/trace"), "concurrency": 4}))
    .execute(&bed.pg)
    .await
    .unwrap();
    let proxy_state = |id: i64| {
        let pg = bed.pg.clone();
        async move {
            sqlx::query_as::<_, (i32, bool)>(
                "SELECT failed_count, COALESCE(cooldown_until > now(), false) FROM proxies WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pg)
            .await
            .unwrap()
        }
    };

    // 健康代理上：模型 B 只在一个上游已挂（https，经隧道）的渠道上，模型 A 在健康渠道上
    let shared = bed.proxy("shared", spawn_tunnel_proxy().await, None).await;
    let dead_upstream = format!("https://{}/v1", dead_addr().await);
    let (dead, dead_key) = bed
        .channel_with("deadup", 0, &dead_upstream, &bed.model_b)
        .await;
    let (good, _) = bed.channel("good", 0).await;
    for channel in [dead, good] {
        bed.bind(channel, json!({"mode": "proxy", "proxy_id": shared}))
            .await;
    }
    for _ in 0..4 {
        assert_eq!(bed.chat_model(&bed.model_b).await, 502);
    }
    // 核实在后台跑：给它时间，期间代理一直不该熔断
    for _ in 0..15 {
        assert_eq!(
            proxy_state(shared).await,
            (0, false),
            "目标连不上不是代理的错"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(bed.chat().await, 200, "同一代理上的健康渠道不受牵连");
    let dead_key_state: (i16, i32) =
        sqlx::query_as("SELECT status, failed_count FROM channel_keys WHERE id = $1")
            .bind(dead_key)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
    assert_eq!(dead_key_state, (1, 0), "连接阶段失败不动 key");

    // 代理本身坏了（读到请求就断）：隧道同样中断，核实探测也失败 → 直接熔断
    let broken = bed.proxy("broken", spawn_broken_proxy().await, None).await;
    let (bad, bad_key) = bed
        .channel_with("broken", 10, &dead_upstream, &bed.model)
        .await;
    bed.bind(bad, json!({"mode": "proxy", "proxy_id": broken}))
        .await;
    assert_eq!(bed.chat().await, 200, "高优先级渠道连不上，改投健康渠道");
    let mut tripped = false;
    for _ in 0..50 {
        let (failed, cooling) = proxy_state(broken).await;
        if cooling {
            assert!(failed >= 3, "确认坏了直接熔断，不等凑满连续次数");
            tripped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(tripped, "代理自己坏了要熔断");
    assert_eq!(bed.key_reason(bad_key).await, "egress_cooling");
    assert_eq!(proxy_state(shared).await, (0, false));

    sqlx::query("DELETE FROM settings WHERE key = 'egress_probe_policy'")
        .execute(&bed.pg)
        .await
        .unwrap();
}
