//! 兑换码验收：批量生成（明文一次）→ 门户核销入账 → 重放/并发拒绝 → 过期拒绝。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct TestEnv {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    addr: SocketAddr,
    admin_token: String,
    user_token: String,
    user_id: i64,
}

/// 每次调用给一个独一无二的来源 IP。
///
/// 核销走 `critical_rate_guard`（每 IP 10 次/分，对齐 new-api rc.24）。此前测试不带任何
/// 转发头、服务器也没挂 connect info，识别不到来源 IP 于是限流整段跳过；现在信任闸以
/// socket 对端兜底（§14.2），同一条环回地址上跑十几次核销就会撞上限流。用例要验的是
/// 一次性 / 并发原子性，不是限流，故逐请求换 IP——真正验限流的用例自己固定同一个 IP。
fn uniq_ip() -> String {
    let h = Uuid::new_v4().simple().to_string();
    format!("2001:db8:{}:{}::1", &h[0..4], &h[4..8])
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let mk = |role: i16| {
        let pg = pg.clone();
        async move {
            let suffix = Uuid::new_v4().simple().to_string();
            let id = okapi_store::provision::create_user(&pg, &format!("rd-{suffix}"))
                .await
                .unwrap();
            sqlx::query!("UPDATE users SET role = $2 WHERE id = $1", id, role)
                .execute(&pg)
                .await
                .unwrap();
            let token = format!("sk-okapi-rd-{suffix}");
            let hash = {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(token.as_bytes()))
            };
            okapi_store::provision::create_api_key(&pg, id, &hash, "sk-okapi-rd")
                .await
                .unwrap();
            (id, token)
        }
    };
    let (_, admin_token) = mk(100).await;
    let (user_id, user_token) = mk(1).await;

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let ledger = state.ledger.clone();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // 按生产形态挂 connect info：转发头信任闸要拿 socket 对端做锚点（§14.2）
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    TestEnv {
        pg,
        ledger,
        addr,
        admin_token,
        user_token,
        user_id,
    }
}

#[tokio::test]
// 生命周期场景脚本：生成→核销→重放→并发→过期 一体时序
#[allow(clippy::too_many_lines)]
async fn redemption_lifecycle() {
    let env = setup().await;
    let client = reqwest::Client::new();

    // 生成 3 张 $2 面值
    let created: Value = client
        .post(format!("http://{}/admin/redemptions", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({"count": 3, "amount_micro": 2_000_000}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let codes: Vec<String> = created["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(codes.len(), 3);
    assert!(codes[0].starts_with("okapi-"));

    // 核销第一张：入账 + 事件
    let redeemed: Value = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": codes[0]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(redeemed["amount_micro"], 2_000_000);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        2_000_000
    );
    let actor = sqlx::query_scalar!(
        r#"SELECT actor FROM billing_events WHERE user_id = $1"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(actor, "system:redeem");

    // 重放拒绝（一次性）
    let replay = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": codes[0]}))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 404);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        2_000_000,
        "重放不得重复入账"
    );

    // 并发核销同一张：恰好一人成功
    let mut handles = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        let url = format!("http://{}/api/me/redeem", env.addr);
        let token = env.user_token.clone();
        let code = codes[1].clone();
        handles.push(tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth(token)
                .header("x-real-ip", uniq_ip())
                .json(&json!({"code": code}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }));
    }
    let mut success = 0;
    for h in handles {
        if h.await.unwrap() == 200 {
            success += 1;
        }
    }
    assert_eq!(success, 1, "并发核销必须恰好一次成功");

    // 过期码拒绝
    let hash2 = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(codes[2].as_bytes()))
    };
    sqlx::query!(
        r#"UPDATE redemption_codes SET expires_at = now() - interval '1 hour' WHERE code_hash = $1"#,
        hash2
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let expired = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": codes[2]}))
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 404);

    // 无权限用户不能生成
    let denied = client
        .post(format!("http://{}/admin/redemptions", env.addr))
        .bearer_auth(&env.user_token)
        .json(&json!({"count": 1, "amount_micro": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
}

/// 套餐×兑换码（#1790-5）：套餐核销覆盖面值 + 加组 + 余额有效期；
/// bind_user 限定他人 404；per-IP 上限拒绝。
#[tokio::test]
// 增强场景脚本：套餐→绑定→IP 限一体时序
#[allow(clippy::too_many_lines)]
async fn redemption_plan_bind_and_ip_cap() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let suffix = Uuid::new_v4().simple().to_string();

    // 建分组（套餐加组目标）+ 套餐：$5 赠额 / 加组 / 余额 30 天有效
    let group = format!("plan-g-{}", &suffix[..8]);
    sqlx::query!(
        r#"INSERT INTO price_groups (group_code, group_ratio, description)
           VALUES ($1, 1, 'plan test') ON CONFLICT (group_code) DO NOTHING"#,
        group
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let plan_resp = client
        .post(format!("http://{}/admin/plans", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({
            "plan_code": format!("plan-{}", &suffix[..8]),
            "display_name": "测试套餐",
            "grant_micro": 5_000_000,
            "group_code": group,
            "balance_valid_days": 30
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(plan_resp.status(), 200);

    // 生成绑套餐的码（面值 1 会被套餐 grant 覆盖）
    let created: Value = client
        .post(format!("http://{}/admin/redemptions", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({
            "count": 1, "amount_micro": 1,
            "plan_code": format!("plan-{}", &suffix[..8])
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let plan_code_str = created["codes"][0].as_str().unwrap().to_owned();

    let before = env.ledger.balance(env.user_id).await.unwrap().as_micros();
    let redeemed: Value = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": plan_code_str}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        redeemed["amount_micro"], 5_000_000,
        "套餐 grant 应覆盖面值：{redeemed}"
    );
    assert_eq!(redeemed["granted_group"].as_str().unwrap(), group);
    let after = env.ledger.balance(env.user_id).await.unwrap().as_micros();
    assert_eq!(after - before, 5_000_000);
    // 加组 + 有效期落库
    let in_group = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM user_groups WHERE user_id = $1 AND group_code = $2) AS "e!""#,
        env.user_id,
        group
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(in_group, "套餐应把用户加入分组");
    let expires = sqlx::query_scalar!(
        r#"SELECT balance_expires_at FROM users WHERE id = $1"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(expires.is_some(), "套餐应设置余额有效期");
    // 清理有效期，防 worker 清零本测试用户
    sqlx::query!(
        r#"UPDATE users SET balance_expires_at = NULL WHERE id = $1"#,
        env.user_id
    )
    .execute(&env.pg)
    .await
    .unwrap();

    // bind_user 限定：绑给 admin，普通用户核销 → 404；admin 核销 → 成功
    let bound: Value = client
        .post(format!("http://{}/admin/redemptions", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({"count": 1, "amount_micro": 300_000,
                      "bind_user_id": admin_id_of(&env).await}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bound_code = bound["codes"][0].as_str().unwrap().to_owned();
    let denied = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": bound_code}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 404, "非绑定用户核销应 404");
    let allowed = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.admin_token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": bound_code}))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200, "绑定用户核销应成功");

    // per-IP 上限：同批 2 张、每 IP 限 1 → 第二张同 IP 拒 429
    let capped: Value = client
        .post(format!("http://{}/admin/redemptions", env.addr))
        .bearer_auth(&env.admin_token)
        .json(&json!({"count": 2, "amount_micro": 100_000, "max_per_ip": 1}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ip = format!("203.0.113.{}", u32::from(rand_octet()));
    let c1 = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", &ip)
        .json(&json!({"code": capped["codes"][0].as_str().unwrap()}))
        .send()
        .await
        .unwrap();
    assert_eq!(c1.status(), 200, "首张核销应成功");
    let c2 = client
        .post(format!("http://{}/api/me/redeem", env.addr))
        .bearer_auth(&env.user_token)
        .header("x-real-ip", &ip)
        .json(&json!({"code": capped["codes"][1].as_str().unwrap()}))
        .send()
        .await
        .unwrap();
    assert_eq!(c2.status(), 429, "同 IP 超限应 429");
}

async fn admin_id_of(env: &TestEnv) -> i64 {
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(env.admin_token.as_bytes()));
    sqlx::query_scalar!(r#"SELECT user_id FROM api_keys WHERE key_hash = $1"#, hash)
        .fetch_one(&env.pg)
        .await
        .unwrap()
}

fn rand_octet() -> u8 {
    use rand::RngExt;
    rand::rng().random_range(1..=254)
}
