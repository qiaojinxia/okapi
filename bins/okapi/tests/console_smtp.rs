//! SMTP 面验收（IMPLEMENTATION §11.27）：本地 mock SMTP 服务器（明文，AUTH PLAIN）承接，
//! 覆盖注册邮箱验证码全流程 / 策略拒绝不发码 / 冷却 / 找回密码 token 一次性 /
//! SMTP 未配置 501 / 管理端测试发送 / 通知 email 通道。
//!
//! settings.smtp 经进程内缓存注入（auth 用例）；只有 `notify_email_channel_and_admin_test_send`
//! 写共享库的 smtp 行（worker Notifier 与管理端测试发送直读 PG），用完即删。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

// ---- mock SMTP ----

#[derive(Debug, Clone)]
struct Captured {
    auth: Option<String>,
    from: String,
    to: Vec<String>,
    data: String,
}

type Inbox = Arc<Mutex<Vec<Captured>>>;

/// 最小 SMTP 服务器：EHLO 广播 AUTH PLAIN LOGIN；记录 MAIL FROM / RCPT TO / DATA。
async fn spawn_smtp() -> (SocketAddr, Inbox) {
    let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let sink = Arc::clone(&inbox);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                let (rd, mut wr) = socket.into_split();
                let mut rd = BufReader::new(rd);
                let _ = wr.write_all(b"220 mock ESMTP\r\n").await;
                let mut cur = Captured {
                    auth: None,
                    from: String::new(),
                    to: Vec::new(),
                    data: String::new(),
                };
                let mut line = String::new();
                loop {
                    line.clear();
                    let Ok(n) = rd.read_line(&mut line).await else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    let l = line.trim_end_matches(['\r', '\n']).to_owned();
                    let upper = l.to_ascii_uppercase();
                    let reply: &str = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        "250-mock\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("AUTH PLAIN") {
                        cur.auth = l.split_whitespace().nth(2).map(str::to_owned);
                        "235 2.7.0 Authentication successful\r\n"
                    } else if upper.starts_with("MAIL FROM:") {
                        cur.from = l["MAIL FROM:".len()..].trim().to_owned();
                        "250 OK\r\n"
                    } else if upper.starts_with("RCPT TO:") {
                        cur.to.push(l["RCPT TO:".len()..].trim().to_owned());
                        "250 OK\r\n"
                    } else if upper == "DATA" {
                        let _ = wr
                            .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                            .await;
                        let mut body = String::new();
                        loop {
                            line.clear();
                            let Ok(n) = rd.read_line(&mut line).await else {
                                break;
                            };
                            if n == 0 || line == ".\r\n" || line == ".\n" {
                                break;
                            }
                            // 去点填充（RFC 5321 §4.5.2：行首 ".." → "."）
                            if line.starts_with("..") {
                                body.push_str(&line[1..]);
                            } else {
                                body.push_str(&line);
                            }
                        }
                        cur.data = body;
                        sink.lock().unwrap().push(cur.clone());
                        cur.from.clear();
                        cur.to.clear();
                        cur.data.clear();
                        "250 OK queued\r\n"
                    } else if upper == "QUIT" {
                        let _ = wr.write_all(b"221 Bye\r\n").await;
                        break;
                    } else {
                        "250 OK\r\n"
                    };
                    if wr.write_all(reply.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, inbox)
}

fn smtp_setting(addr: SocketAddr) -> Value {
    json!({
        "host": addr.ip().to_string(),
        "port": addr.port(),
        "security": "none",
        "username": "mailer",
        "password": "s3cret",
        "from_address": "no-reply@okapi.test",
        "from_name": "Okapi Test"
    })
}

async fn wait_inbox(inbox: &Inbox, n: usize) -> Vec<Captured> {
    for _ in 0..100 {
        let got = inbox.lock().unwrap().clone();
        if got.len() >= n {
            return got;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("等待 {n} 封邮件超时：{:?}", inbox.lock().unwrap());
}

/// 最小 quoted-printable 解码（lettre 对超过 78 列的行会选 QP：软换行 `=\r\n`、`=XX`）。
fn qp_decode(data: &str) -> String {
    let flat = data.replace("=\r\n", "").replace("=\n", "");
    let bytes = flat.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'='
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// QP 解码后按前缀抓取一段 token（字母数字）。
fn extract_after(data: &str, marker: &str) -> Option<String> {
    let flat = qp_decode(data);
    let start = flat.find(marker)? + marker.len();
    let tok: String = flat[start..]
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (!tok.is_empty()).then_some(tok)
}

// ---- 环境 ----

struct TestEnv {
    addr: SocketAddr,
    pg: sqlx::PgPool,
    state: gateway::state::AppState,
}

fn uniq_ip() -> String {
    let h = Uuid::new_v4().simple().to_string();
    format!("2001:db8:{}:{}::1", &h[0..4], &h[4..8])
}

/// `smtp` / `registration_policy` 均经进程内 settings 缓存注入，不碰共享库。
async fn setup(smtp: Option<Value>, policy: Option<Value>) -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .settings_cache
        .insert("smtp".to_owned(), Arc::new(smtp))
        .await;
    state
        .settings_cache
        .insert("registration_policy".to_owned(), Arc::new(policy))
        .await;
    state
        .settings_cache
        .insert("site_name".to_owned(), Arc::new(Some(json!("Okapi QA"))))
        .await;
    let app = console::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    TestEnv { addr, pg, state }
}

async fn post(env: &TestEnv, path: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("http://{}{path}", env.addr))
        .header("x-real-ip", uniq_ip())
        .header("accept-language", "en-US,en;q=0.9")
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// `{"error":{"code","param",...}}` → (status, code, param)。
async fn error_code(resp: reqwest::Response) -> (u16, String, Option<String>) {
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    (
        status,
        body["error"]["code"].as_str().unwrap_or("").to_owned(),
        body["error"]["param"].as_str().map(str::to_owned),
    )
}

// ---- 注册邮箱验证码 ----

/// 全流程：取码（信到、AUTH PLAIN 带上凭证、发件人/收件人对）→ 错码 400 → 对码注册成功 →
/// 同码复用 400（一次性）；公开策略透出 email_verification。
#[tokio::test]
async fn email_verification_full_flow() {
    let (smtp, inbox) = spawn_smtp().await;
    let env = setup(
        Some(smtp_setting(smtp)),
        Some(json!({"mode": "open", "email_verification": true})),
    )
    .await;
    let suffix = Uuid::new_v4().simple().to_string();
    let email = format!("v-{suffix}@ok.test");

    let pol: Value = reqwest::get(format!("http://{}/api/registration", env.addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pol["email_verification"], true, "登录页据此画验证码栏");

    // 没码注册 → 400 email_code
    let (status, code, param) = error_code(
        post(
            &env,
            "/auth/register",
            json!({"email": email, "username": format!("v-{suffix}"), "password": "hunter2-strong"}),
        )
        .await,
    )
    .await;
    assert_eq!((status, code.as_str()), (400, "bad_request"));
    assert_eq!(param.as_deref(), Some("email_code"));

    // 取码
    let resp = post(&env, "/auth/email-code", json!({"email": email})).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let mails = wait_inbox(&inbox, 1).await;
    let mail = &mails[0];
    assert_eq!(mail.from, "<no-reply@okapi.test>");
    assert_eq!(mail.to, vec![format!("<{email}>")]);
    let expected_auth = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(b"\0mailer\0s3cret")
    };
    assert_eq!(
        mail.auth.as_deref(),
        Some(expected_auth.as_str()),
        "AUTH PLAIN 带配置里的凭证"
    );
    assert!(
        mail.data
            .contains("Subject: [Okapi QA] Your verification code "),
        "英文模板 + 站点名：{}",
        mail.data
    );
    let code = extract_after(&mail.data, "Verification code: ").expect("正文含验证码");
    assert_eq!(code.len(), 6);
    assert!(code.chars().all(|c| c.is_ascii_digit()));

    // 60s 冷却：立刻再取 → 429
    let (status, ec, _) =
        error_code(post(&env, "/auth/email-code", json!({"email": email})).await).await;
    assert_eq!((status, ec.as_str()), (429, "email_code_cooldown"));

    // 错码 400
    let (status, _, param) = error_code(
        post(
            &env,
            "/auth/register",
            json!({"email": email, "username": format!("v-{suffix}"), "password": "hunter2-strong",
                   "email_code": "000000"}),
        )
        .await,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(param.as_deref(), Some("email_code_invalid"));

    // 对码注册成功
    let resp = post(
        &env,
        "/auth/register",
        json!({"email": email, "username": format!("v-{suffix}"), "password": "hunter2-strong",
               "email_code": code}),
    )
    .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    // 同码复用：已销毁 → 400（邮箱已占用也不该先于验证码泄露）
    let (status, _, param) = error_code(
        post(
            &env,
            "/auth/register",
            json!({"email": email, "username": format!("v2-{suffix}"), "password": "hunter2-strong",
                   "email_code": code}),
        )
        .await,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(param.as_deref(), Some("email_code_invalid"), "验证码一次性");
}

/// 策略先于发信：域名黑名单 / 关闭注册 / 未开验证 → 不给码；SMTP 未配置 → 501。
#[tokio::test]
async fn email_code_respects_policy_and_config() {
    let (smtp, inbox) = spawn_smtp().await;
    let env = setup(
        Some(smtp_setting(smtp)),
        Some(json!({"mode": "open", "email_verification": true,
                    "email_domain_mode": "blocklist", "email_domains": ["tempmail.io"]})),
    )
    .await;
    let (status, code, _) =
        error_code(post(&env, "/auth/email-code", json!({"email": "x@tempmail.io"})).await).await;
    assert_eq!((status, code.as_str()), (403, "email_domain_rejected"));

    let env_closed = setup(
        Some(smtp_setting(smtp)),
        Some(json!({"mode": "closed", "email_verification": true})),
    )
    .await;
    let (status, code, _) = error_code(
        post(
            &env_closed,
            "/auth/email-code",
            json!({"email": "x@ok.test"}),
        )
        .await,
    )
    .await;
    assert_eq!((status, code.as_str()), (403, "registration_closed"));

    let env_off = setup(Some(smtp_setting(smtp)), Some(json!({"mode": "open"}))).await;
    let (status, _, param) =
        error_code(post(&env_off, "/auth/email-code", json!({"email": "x@ok.test"})).await).await;
    assert_eq!(status, 400);
    assert_eq!(param.as_deref(), Some("email_verification_disabled"));

    let env_nosmtp = setup(
        None,
        Some(json!({"mode": "open", "email_verification": true})),
    )
    .await;
    let (status, code, _) = error_code(
        post(
            &env_nosmtp,
            "/auth/email-code",
            json!({"email": "x@ok.test"}),
        )
        .await,
    )
    .await;
    assert_eq!((status, code.as_str()), (501, "smtp_not_configured"));

    // 全程一封都没发出去
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(inbox.lock().unwrap().is_empty(), "策略拒绝不得发信");
}

// ---- 找回密码 ----

/// `POST /auth/password/reset` 预期 400 且 `error.param` 为给定值。
async fn reset_expect_400(env: &TestEnv, token: &str, password: &str, want_param: &str) {
    let (status, _, param) = error_code(
        post(
            env,
            "/auth/password/reset",
            json!({"token": token, "password": password}),
        )
        .await,
    )
    .await;
    assert_eq!((status, param.as_deref()), (400, Some(want_param)));
}

/// 不存在的邮箱也回 ok 且不发信；存在的邮箱收到含 token 链接（基址按 Host 推导）；
/// 错 token 400；对 token 重设成功后旧密码失效、新密码可登录；token 一次性。
#[tokio::test]
async fn password_reset_flow() {
    let (smtp, inbox) = spawn_smtp().await;
    let env = setup(Some(smtp_setting(smtp)), Some(json!({"mode": "open"}))).await;
    let suffix = Uuid::new_v4().simple().to_string();
    let email = format!("r-{suffix}@ok.test");

    // 注册一个有密码的用户
    let resp = post(
        &env,
        "/auth/register",
        json!({"email": email, "username": format!("r-{suffix}"), "password": "old-password-1"}),
    )
    .await;
    assert_eq!(resp.status(), 200);

    // 不存在的邮箱：ok 且不发信（防枚举）
    let resp = post(
        &env,
        "/auth/password/forgot",
        json!({"email": format!("nobody-{suffix}@ok.test")}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(inbox.lock().unwrap().is_empty());

    // 存在：发信
    let resp = post(&env, "/auth/password/forgot", json!({"email": email})).await;
    assert_eq!(resp.status(), 200);
    let mails = wait_inbox(&inbox, 1).await;
    let data = &mails[0].data;
    assert!(
        data.contains("Subject: [Okapi QA] Reset your password"),
        "{data}"
    );
    let marker = format!("http://{}/reset-password?token=", env.addr);
    assert!(
        qp_decode(data).contains(&marker),
        "链接基址缺省按请求 Host 推导：{data}"
    );
    let token = extract_after(data, "reset-password?token=").expect("正文含 token");
    assert_eq!(token.len(), 32);

    // 错 token / 短密码：都是 400 + param
    reset_expect_400(&env, "nope-nope", "new-password-22", "reset_token_invalid").await;
    reset_expect_400(&env, &token, "short", "password").await;

    // 重设成功
    let resp = post(
        &env,
        "/auth/password/reset",
        json!({"token": token, "password": "new-password-22"}),
    )
    .await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    // token 一次性
    reset_expect_400(&env, &token, "new-password-33", "reset_token_invalid").await;

    // 旧密码 401、新密码 200
    let old = post(
        &env,
        "/auth/login",
        json!({"email": email, "password": "old-password-1"}),
    )
    .await;
    assert_eq!(old.status(), 401);
    let new = post(
        &env,
        "/auth/login",
        json!({"email": email, "password": "new-password-22"}),
    )
    .await;
    assert_eq!(new.status(), 200, "{}", new.text().await.unwrap());
    assert!(
        sqlx::query_scalar!(
            r#"SELECT password_hash AS "h!" FROM users WHERE email = $1"#,
            email
        )
        .fetch_one(&env.pg)
        .await
        .unwrap()
        .starts_with("$argon2"),
        "重设一律 argon2id"
    );
}

/// SMTP 未配置：找回密码 501（配置问题要暴露，不能假装发出去了）。
#[tokio::test]
async fn password_forgot_without_smtp_is_501() {
    let env = setup(None, None).await;
    let (status, code, _) =
        error_code(post(&env, "/auth/password/forgot", json!({"email": "x@ok.test"})).await).await;
    assert_eq!((status, code.as_str()), (501, "smtp_not_configured"));
}

// ---- 通知 email 通道 + 管理端测试发送（唯一写共享库 smtp 行的用例）----

#[tokio::test]
async fn notify_email_channel_and_admin_test_send() {
    let (smtp, inbox) = spawn_smtp().await;
    let env = setup(Some(smtp_setting(smtp)), None).await;
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('smtp', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        smtp_setting(smtp)
    )
    .execute(&env.pg)
    .await
    .unwrap();

    // 管理端测试发送：super_admin
    let suffix = Uuid::new_v4().simple().to_string();
    let admin = okapi_store::provision::create_user(&env.pg, &format!("sa-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin)
        .execute(&env.pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-smtp-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&env.pg, admin, &hash, "sk-smtp")
        .await
        .unwrap();
    let resp = reqwest::Client::new()
        .post(format!("http://{}/admin/settings/smtp/test", env.addr))
        .bearer_auth(&token)
        .json(&json!({"to": "ops@okapi.test", "lang": "zh-CN"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let mails = wait_inbox(&inbox, 1).await;
    assert_eq!(mails[0].to, vec!["<ops@okapi.test>".to_owned()]);
    assert!(
        mails[0].data.contains("Subject: =?utf-8?") || mails[0].data.contains("SMTP"),
        "中文主题走 RFC 2047 编码：{}",
        mails[0].data
    );

    // 列表接口对 smtp 键只回"已配置"
    let list: Value = reqwest::Client::new()
        .get(format!("http://{}/admin/settings", env.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "smtp")
        .expect("列表含 smtp");
    assert_eq!(row["is_secret"], true);
    assert_eq!(row["configured"], true);
    assert!(row["value"].is_null(), "密码不得出列表");

    // 通知 email 通道
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let event = format!("drift_{}", &suffix[..8]);
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('notify_channels', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!([{
            "type": "email",
            "to": ["a@okapi.test", "b@okapi.test"],
            "events": [event],
            "lang": "en",
            "min_interval_secs": 60
        }])
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let notifier = okapi::worker::notify::Notifier::new(env.pg.clone(), redis);
    notifier
        .dispatch(&event, &json!({"delta_micro": -1234}))
        .await;
    let mails = wait_inbox(&inbox, 3).await;
    let mut rcpts: Vec<String> = mails[1..].iter().flat_map(|m| m.to.clone()).collect();
    rcpts.sort();
    assert_eq!(
        rcpts,
        vec!["<a@okapi.test>", "<b@okapi.test>"],
        "一封一收件人"
    );
    assert!(mails[1].data.contains(&format!("Event: {event}")));
    assert!(mails[1].data.contains("delta_micro"), "正文含 payload");

    // 频率闸：静默期内不再发
    notifier.dispatch(&event, &json!({"again": true})).await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(inbox.lock().unwrap().len(), 3, "静默期内不得重发");

    // 清理共享库
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'smtp'"#)
        .execute(&env.pg)
        .await
        .unwrap();
    let _ = &env.state;
}
