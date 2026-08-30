//! /auth/* 自助面验收（§6.4）：注册→登录（cookie 会话）→兑换 key→
//! TOTP 两段式注册→带码登录→登出失效。依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

struct TestEnv {
    addr: SocketAddr,
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let mut state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    // TOTP 信封主密钥（测试固定值，直接注入 state）
    state.master_key = Some(std::sync::Arc::from(hex::encode([9u8; 32]).as_str()));
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestEnv { addr }
}

fn cookie_of(resp: &reqwest::Response) -> String {
    resp.headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .expect("登录必须发会话 cookie")
        .to_owned()
}

/// RFC 6238 本地生成当前码（与服务端同算法，测试专用）。
fn totp_now(secret: &[u8]) -> String {
    use hmac::{Hmac, KeyInit as _, Mac};
    let counter = u64::try_from(chrono::Utc::now().timestamp() / 30).unwrap();
    let mut mac = <Hmac<sha1::Sha1>>::new_from_slice(secret).unwrap();
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[19] & 0x0f);
    let bin = (u32::from(digest[offset]) & 0x7f) << 24
        | u32::from(digest[offset + 1]) << 16
        | u32::from(digest[offset + 2]) << 8
        | u32::from(digest[offset + 3]);
    format!("{:06}", bin % 1_000_000)
}

#[tokio::test]
// 端到端场景脚本：分阶段拆函数割裂 auth 全流程语义
#[allow(clippy::too_many_lines)]
async fn register_login_key_totp_full_flow() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let suffix = Uuid::new_v4().simple().to_string();
    let email = format!("u-{suffix}@ok.test");

    // 注册
    let resp = client
        .post(format!("http://{}/auth/register", env.addr))
        .json(&json!({"email": email, "username": format!("web-{suffix}"), "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);

    // 重复邮箱 409
    let dup = client
        .post(format!("http://{}/auth/register", env.addr))
        .json(&json!({"email": email, "username": format!("web2-{suffix}"), "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), 409);

    // 错密码 401
    let bad = client
        .post(format!("http://{}/auth/login", env.addr))
        .json(&json!({"email": email, "password": "wrong-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);

    // 登录 → cookie
    let login = client
        .post(format!("http://{}/auth/login", env.addr))
        .json(&json!({"email": email, "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = cookie_of(&login);

    // 兑换 key → 门户可用（key 单轨打通）
    let key_resp: Value = client
        .post(format!("http://{}/auth/keys", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({"name": "cli"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let api_key = key_resp["api_key"].as_str().expect("明文一次返回");
    let me = client
        .get(format!("http://{}/api/me", env.addr))
        .bearer_auth(api_key)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200, "兑换的 key 必须直接可用");

    // TOTP 两段式
    let enroll: Value = client
        .post(format!("http://{}/auth/totp/enroll", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let otpauth = enroll["otpauth_url"].as_str().unwrap();
    assert!(otpauth.starts_with("otpauth://totp/Okapi:"));
    let secret_b32 = otpauth
        .split("secret=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let secret = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, secret_b32).unwrap();
    let pending = enroll["pending"].as_str().unwrap();

    // 错码拒绝
    let bad_code = client
        .post(format!("http://{}/auth/totp/confirm", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({"pending": pending, "code": "000000"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_code.status(), 400);

    let confirm = client
        .post(format!("http://{}/auth/totp/confirm", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({"pending": pending, "code": totp_now(&secret)}))
        .send()
        .await
        .unwrap();
    assert_eq!(confirm.status(), 200);

    // 启用后：无码登录 401 totp_required；带码成功
    let no_code = client
        .post(format!("http://{}/auth/login", env.addr))
        .json(&json!({"email": email, "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(no_code.status(), 401);
    let body: Value = no_code.json().await.unwrap();
    assert_eq!(body["error"]["code"], "totp_required");

    let with_code = client
        .post(format!("http://{}/auth/login", env.addr))
        .json(&json!({"email": email, "password": "hunter2-strong",
            "totp_code": totp_now(&secret)}))
        .send()
        .await
        .unwrap();
    assert_eq!(with_code.status(), 200, "{:?}", with_code.text().await);
    let cookie2 = cookie_of(&with_code);

    // 登出后会话失效
    client
        .post(format!("http://{}/auth/logout", env.addr))
        .header(reqwest::header::COOKIE, &cookie2)
        .send()
        .await
        .unwrap();
    let after = client
        .post(format!("http://{}/auth/keys", env.addr))
        .header(reqwest::header::COOKIE, &cookie2)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), 401, "登出后会话必须失效");
}

/// 关键接口每 IP 限流（对齐 new-api rc.24）：同 IP 超限 429、换 IP 复位、
/// 无法识别 IP（测试裸 serve 且无 CDN 头）放行不阻断。
#[tokio::test]
async fn critical_rate_limit_guards_login() {
    let env = setup().await;
    let client = reqwest::Client::new();

    // 收紧 login 配额便于断言（其余测试不带 IP 头，天然放行不受影响）
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('critical_rate_limits', '{"login": 3}'::jsonb)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#
    )
    .execute(&pg)
    .await
    .unwrap();

    // 每次跑用唯一 IP，避免 60s 窗口内的历史计数残留
    let suffix = Uuid::new_v4().simple().to_string();
    let ip = format!(
        "198.51.100.{}",
        1 + (u32::from_str_radix(&suffix[..2], 16).unwrap() % 250)
    );
    let bad_login =
        json!({"email": format!("nobody-{suffix}@test.local"), "password": "wrong-password"});

    // 前 3 次：账号不存在 → 401（未触发限流）
    for i in 0..3 {
        let resp = client
            .post(format!("http://{}/auth/login", env.addr))
            .header("x-real-ip", &ip)
            .json(&bad_login)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401, "第 {} 次应为凭证错误", i + 1);
    }
    // 第 4 次：同 IP 超限 → 429
    let limited = client
        .post(format!("http://{}/auth/login", env.addr))
        .header("x-real-ip", &ip)
        .json(&bad_login)
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), 429, "同 IP 第 4 次必须限流");
    let body: Value = limited.json().await.unwrap();
    assert_eq!(body["error"]["code"], "rate_limited");
    assert_eq!(body["error"]["param"], "login");

    // 换 IP：独立计数，仍是 401
    let other = client
        .post(format!("http://{}/auth/login", env.addr))
        .header("x-real-ip", "198.51.100.251")
        .json(&bad_login)
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 401, "不同 IP 独立计数");

    // 无 IP（测试裸 serve 无 ConnectInfo、无 CDN 头）：放行
    let no_ip = client
        .post(format!("http://{}/auth/login", env.addr))
        .json(&bad_login)
        .send()
        .await
        .unwrap();
    assert_eq!(no_ip.status(), 401, "无法识别 IP 时不阻断");

    sqlx::query!(r#"DELETE FROM settings WHERE key = 'critical_rate_limits'"#)
        .execute(&pg)
        .await
        .unwrap();
}
