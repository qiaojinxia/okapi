//! 数据面无效 key 每 IP 固定窗限流（scope=invalid_api_key）。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::gateway;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

async fn setup(limit: i64) -> (SocketAddr, String) {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .settings_cache
        .insert(
            "critical_rate_limits".to_owned(),
            Arc::new(Some(json!({ "invalid_api_key": limit }))),
        )
        .await;
    let app = gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (addr, unique_ip())
}

/// 每次调用都给一个全新的客户端 IP（IPv6 文档前缀 2001:db8::/32，后缀取 64 位随机）。
///
/// 计数器 `crl:invalid_api_key:{ip}` 在共享 Redis 里、60s 固定窗口，跨用例也跨轮次。
/// 此前用的是 `203.0.113.{1..250}`（只有 250 个取值）和写死的 `198.51.100.9`：
/// 60s 内把本文件跑上三遍（变异 sweep、反复本地重跑都会），写死的那个 IP 就攒够了计数，
/// "另一个 IP 不受牵连"的断言拿到 429；随机的那个也会撞上还没过期的旧计数。
/// 于是这两条用例随执行顺序时红时绿，并在变异测试里被记成"假抓手"。
fn unique_ip() -> String {
    let u = Uuid::new_v4().as_u128();
    format!(
        "2001:db8:{:x}:{:x}:{:x}:{:x}::1",
        (u >> 48) & 0xffff,
        (u >> 32) & 0xffff,
        (u >> 16) & 0xffff,
        u & 0xffff
    )
}

async fn hit(addr: SocketAddr, ip: &str, key: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("http://{addr}/v1/dashboard/billing/subscription"))
        .bearer_auth(key)
        .header("x-real-ip", ip)
        .send()
        .await
        .unwrap()
}

/// 同一 IP 猜 key 超过窗口配额 → 429，而不是继续 401。
#[tokio::test]
async fn invalid_key_trips_per_ip_limit() {
    let (addr, ip) = setup(3).await;
    for _ in 0..3 {
        let resp = hit(addr, &ip, "sk-bogus-scan").await;
        assert_eq!(resp.status(), 401, "{}", resp.text().await.unwrap());
    }
    let blocked = hit(addr, &ip, "sk-bogus-scan").await;
    assert_eq!(blocked.status(), 429);
    let body: serde_json::Value = blocked.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["error"]["param"], "invalid_api_key");
}

/// 另一个 IP 不受牵连。
#[tokio::test]
async fn invalid_key_limit_is_per_ip() {
    let (addr, ip) = setup(2).await;
    assert_eq!(hit(addr, &ip, "sk-a").await.status(), 401);
    assert_eq!(hit(addr, &ip, "sk-a").await.status(), 401);
    assert_eq!(hit(addr, &ip, "sk-a").await.status(), 429);
    let other = unique_ip();
    assert_eq!(hit(addr, &other, "sk-a").await.status(), 401);
}
