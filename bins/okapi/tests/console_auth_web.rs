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
    setup_with_policy(None).await
}

/// 注册策略经进程内 settings 缓存注入而不写库：其它用例并行注册时不受影响
/// （写共享库里的 registration_policy = closed 会让同时在跑的注册用例莫名 403）。
async fn setup_with_policy(policy: Option<Value>) -> TestEnv {
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
    state
        .settings_cache
        .insert(
            "registration_policy".to_owned(),
            std::sync::Arc::new(policy),
        )
        .await;
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

/// 自选档位（IMPLEMENTATION §11.14 R4）：用户只能在 已分配 ∪ self_select ∪ 默认组 里
/// 给自己的 key 选分组；不可选的组 403 `group_not_selectable`（不是 404——组存在，
/// 只是他没资格）；选定后 /api/me 的生效分组随之变化，改回 null 则跟随用户分组。
#[tokio::test]
// 端到端场景脚本：建组 → 注册登录 → 建 key 选组 → 校验生效 → 改组 → 改回，拆分会割裂叙事
#[allow(clippy::too_many_lines)]
async fn self_select_group_on_own_keys() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let suffix = Uuid::new_v4().simple().to_string();
    let pg = okapi_store::connect_pg(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    let open = format!("open-{}", &suffix[..8]);
    let closed = format!("closed-{}", &suffix[..8]);
    for (code, self_select) in [(&open, true), (&closed, false)] {
        okapi_store::admin::upsert_price_group(
            &pg,
            okapi_store::admin::PriceGroupInput {
                group_code: code,
                group_ratio: "2",
                description: "tier",
                pool_code: None,
                self_select,
            },
        )
        .await
        .unwrap();
    }

    let email = format!("g-{suffix}@ok.test");
    client
        .post(format!("http://{}/auth/register", env.addr))
        .json(&json!({"email": email, "username": format!("g-{suffix}"), "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    let login = client
        .post(format!("http://{}/auth/login", env.addr))
        .json(&json!({"email": email, "password": "hunter2-strong"}))
        .send()
        .await
        .unwrap();
    let cookie = cookie_of(&login);

    // 不可选的组 → 403 带码
    let denied = client
        .post(format!("http://{}/auth/keys", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({"name": "k", "group_code": closed}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    assert_eq!(
        denied.json::<Value>().await.unwrap()["error"]["code"],
        "group_not_selectable"
    );

    // 可自选的组 → 建成，且生效分组即该组
    let created: Value = client
        .post(format!("http://{}/auth/keys", env.addr))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({"name": "k", "group_code": open}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let api_key = created["api_key"].as_str().unwrap();
    let key_id = created["key_id"].as_i64().unwrap();
    let me: Value = client
        .get(format!("http://{}/api/me", env.addr))
        .bearer_auth(api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["group"], open, "选定档位后生效分组即该组：{me}");

    // 可选清单：含 open（self_select）与 default（默认组），不含 closed
    let groups: Value = client
        .get(format!("http://{}/api/me/groups", env.addr))
        .bearer_auth(api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(groups["current"], open);
    let codes: Vec<&str> = groups["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&open.as_str()), "{codes:?}");
    assert!(codes.contains(&"default"), "{codes:?}");
    assert!(!codes.contains(&closed.as_str()), "{codes:?}");
    let open_entry = groups["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["code"] == open)
        .unwrap();
    assert_eq!(open_entry["source"], "self_select");

    // PATCH 改到不可选的组 → 403；改回 null → 跟随用户分组（default）
    let patch = client
        .patch(format!("http://{}/api/me/keys/{key_id}", env.addr))
        .bearer_auth(api_key)
        .json(&json!({"group_code": closed}))
        .send()
        .await
        .unwrap();
    assert_eq!(patch.status(), 403);
    let patch = client
        .patch(format!("http://{}/api/me/keys/{key_id}", env.addr))
        .bearer_auth(api_key)
        .json(&json!({"group_code": null}))
        .send()
        .await
        .unwrap();
    assert_eq!(patch.status(), 200);
    let me: Value = client
        .get(format!("http://{}/api/me", env.addr))
        .bearer_auth(api_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(me["group"], "default", "置 null 后回到用户分组：{me}");
}

/// 注册策略（§11.16）：关闭 → 403；邀请制 → 无码 403、有码放行并给新用户 / 邀请人入账；
/// 邮箱域名黑白名单；公开端点只透出登录页需要的字段。策略经缓存注入，不碰共享库。
#[tokio::test]
// 场景脚本：三种策略各起一个实例，拆成三个用例要重复三遍注册样板
#[allow(clippy::too_many_lines)]
async fn registration_policy_gates_signup() {
    let client = reqwest::Client::new();
    let suffix = Uuid::new_v4().simple().to_string();
    // 注册限流 5/分钟/IP：本用例要发八次注册，每次换一个来源 IP
    let register = |addr: std::net::SocketAddr, email: String, aff: Option<String>| {
        let client = client.clone();
        let ip = format!(
            "198.51.{}.{}",
            1 + (rand::random::<u8>() % 250),
            1 + (rand::random::<u8>() % 250)
        );
        async move {
            client
                .post(format!("http://{addr}/auth/register"))
                .header("x-real-ip", ip)
                .json(
                    &json!({"email": email, "username": email.split('@').next().unwrap(),
                               "password": "hunter2-strong", "aff_code": aff}),
                )
                .send()
                .await
                .unwrap()
        }
    };

    // 1) 关闭注册
    let closed = setup_with_policy(Some(json!({"mode": "closed"}))).await;
    let resp = register(closed.addr, format!("c-{suffix}@ok.test"), None).await;
    assert_eq!(resp.status(), 403);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "registration_closed"
    );
    let pub_policy: Value = client
        .get(format!("http://{}/api/registration", closed.addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pub_policy["mode"], "closed");

    // 2) 邀请制 + 赠送：无码拒绝；有码放行，新用户 1.0 + 0.5，邀请人 0.25
    let pg = okapi_store::connect_pg(&std::env::var("DATABASE_URL").unwrap())
        .await
        .unwrap();
    let inviter_id = okapi_store::provision::create_user(&pg, &format!("inv-{suffix}"))
        .await
        .unwrap();
    let aff = format!("aff{}", &suffix[..6]);
    sqlx::query!(
        "UPDATE users SET aff_code = $2 WHERE id = $1",
        inviter_id,
        aff
    )
    .execute(&pg)
    .await
    .unwrap();
    let invite = setup_with_policy(Some(json!({
        "mode": "invite_only",
        "new_user_credit_micro": 1_000_000,
        "invitee_credit_micro": 500_000,
        "inviter_credit_micro": 250_000
    })))
    .await;
    let resp = register(invite.addr, format!("i0-{suffix}@ok.test"), None).await;
    assert_eq!(resp.status(), 403);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "invite_required"
    );
    let resp = register(
        invite.addr,
        format!("i1-{suffix}@ok.test"),
        Some("no-such-code".into()),
    )
    .await;
    assert_eq!(resp.status(), 403, "无效邀请码在邀请制下同样拒绝");
    let resp = register(
        invite.addr,
        format!("i2-{suffix}@ok.test"),
        Some(aff.clone()),
    )
    .await;
    assert_eq!(resp.status(), 200, "{:?}", resp.text().await);
    let new_user = sqlx::query!(
        r#"SELECT id, inviter_id FROM users WHERE email = $1"#,
        format!("i2-{suffix}@ok.test")
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(new_user.inviter_id, Some(inviter_id), "邀请关系绑定");
    let gift = sqlx::query!(
        r#"SELECT delta_micro, actor, payload FROM billing_events WHERE user_id = $1"#,
        new_user.id
    )
    .fetch_all(&pg)
    .await
    .unwrap();
    assert_eq!(gift.len(), 1, "新用户赠送 + 被邀请奖励合成一笔");
    assert_eq!(gift[0].delta_micro, 1_500_000);
    assert_eq!(gift[0].actor, "system:register");
    assert_eq!(
        gift[0].payload.as_ref().unwrap()["tags"][0],
        "new_user_gift"
    );
    let reward = sqlx::query!(
        r#"SELECT delta_micro, payload FROM billing_events
           WHERE user_id = $1 AND actor = 'system:register'"#,
        inviter_id
    )
    .fetch_all(&pg)
    .await
    .unwrap();
    assert_eq!(reward.len(), 1);
    assert_eq!(reward[0].delta_micro, 250_000);
    assert_eq!(
        reward[0].payload.as_ref().unwrap()["tags"][0],
        "invite_reward"
    );
    let pub_policy: Value = client
        .get(format!("http://{}/api/registration", invite.addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pub_policy["mode"], "invite_only");
    assert_eq!(pub_policy["new_user_credit_micro"], 1_000_000);

    // 3) 邮箱域名：黑名单拒 tempmail、放 ok.test；白名单只放 ok.test 且透出清单
    let block = setup_with_policy(Some(json!({
        "email_domain_mode": "blocklist", "email_domains": ["tempmail.io"]
    })))
    .await;
    let resp = register(block.addr, format!("b1-{suffix}@tempmail.io"), None).await;
    assert_eq!(resp.status(), 403);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "email_domain_rejected"
    );
    let resp = register(block.addr, format!("b2-{suffix}@ok.test"), None).await;
    assert_eq!(resp.status(), 200);
    let allow = setup_with_policy(Some(json!({
        "email_domain_mode": "allowlist", "email_domains": ["ok.test", "*.edu.cn"]
    })))
    .await;
    let resp = register(allow.addr, format!("a1-{suffix}@gmail.com"), None).await;
    assert_eq!(resp.status(), 403);
    let resp = register(
        allow.addr,
        format!("a2-{suffix}@mail.tsinghua.edu.cn"),
        None,
    )
    .await;
    assert_eq!(resp.status(), 200, "通配后缀放行");
    let pub_policy: Value = client
        .get(format!("http://{}/api/registration", allow.addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        pub_policy["allowed_domains"],
        json!(["ok.test", "*.edu.cn"])
    );
}
