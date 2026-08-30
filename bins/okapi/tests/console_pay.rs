//! 支付闭环验收（§11.2 / §13-M4）：epay 下单签名→回调验签核销→重放幂等；
//! Stripe Checkout（mock API）→ webhook HMAC 验签→核销幂等；金额换算精确；
//! 错签名拒绝。依赖 .env（scripts/dev-deps.sh up）。

use axum::response::IntoResponse;
use axum::routing::post;
use hmac::{Hmac, KeyInit as _, Mac};
use md5::{Digest as Md5Digest, Md5};
use okapi::{console, gateway};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use uuid::Uuid;

struct TestEnv {
    pg: PgPool,
    ledger: okapi_ledger::BalanceLedger,
    addr: SocketAddr,
    user_token: String,
    user_id: i64,
}

const EPAY_KEY: &str = "epay-merchant-secret";
const STRIPE_WH: &str = "whsec_test_secret";

/// mock Stripe API：POST /v1/checkout/sessions → 固定 session。
async fn mock_stripe(headers: axum::http::HeaderMap, body: String) -> axum::response::Response {
    assert!(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer sk_test_")),
        "必须带商户密钥"
    );
    assert!(body.contains("unit_amount]=500"), "分为最小单位：{body}");
    axum::Json(json!({"id": "cs_test_1", "url": "https://checkout.stripe.test/pay/cs_test_1"}))
        .into_response()
}

async fn setup() -> TestEnv {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let stripe_app = axum::Router::new().route("/v1/checkout/sessions", post(mock_stripe));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stripe_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, stripe_app).await.unwrap();
    });

    for (key, value) in [
        (
            "payment_epay",
            json!({"gateway_url": "https://epay.test/submit.php", "pid": "1001",
                   "key": EPAY_KEY, "usd_to_cny_milli": 7000}),
        ),
        (
            "payment_stripe",
            json!({"secret_key": "sk_test_abc", "webhook_secret": STRIPE_WH,
                   "api_base": format!("http://{stripe_addr}")}),
        ),
    ] {
        sqlx::query!(
            r#"INSERT INTO settings (key, value) VALUES ($1, $2)
               ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
            key,
            value
        )
        .execute(&pg)
        .await
        .unwrap();
    }

    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = okapi_store::provision::create_user(&pg, &format!("pay-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-pay-{suffix}");
    let hash = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(user_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, user_id, &hash, "sk-okapi-pay")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let ledger = state.ledger.clone();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    TestEnv {
        pg,
        ledger,
        addr,
        user_token,
        user_id,
    }
}

fn epay_sign(params: &BTreeMap<&str, String>) -> String {
    let mut buf = String::new();
    for (k, v) in params {
        if v.is_empty() || *k == "sign" || *k == "sign_type" {
            continue;
        }
        if !buf.is_empty() {
            buf.push('&');
        }
        buf.push_str(k);
        buf.push('=');
        buf.push_str(v);
    }
    buf.push_str(EPAY_KEY);
    hex::encode(Md5::digest(buf.as_bytes()))
}

#[tokio::test]
// 双网关端到端场景脚本
#[allow(clippy::too_many_lines)]
async fn payment_full_cycle_epay_and_stripe() {
    let env = setup().await;
    let client = reqwest::Client::new();

    // —— epay：下单（$12.3456 → CNY 7 倍率 = 86.42 向上取整分） ——
    let order: Value = client
        .post(format!("http://{}/api/me/topup", env.addr))
        .bearer_auth(&env.user_token)
        .json(&json!({"amount_micro": 12_345_600, "gateway": "epay"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let order_no = order["order_no"].as_str().unwrap().to_owned();
    assert_eq!(
        order["params"]["money"], "86.42",
        "12.3456 USD × 7.0 = 86.4192 → 分向上取整"
    );
    // 服务器签名可用商户密钥复验
    let mut p: BTreeMap<&str, String> = BTreeMap::new();
    for k in ["pid", "type", "out_trade_no", "name", "money"] {
        p.insert(
            match k {
                "pid" => "pid",
                "type" => "type",
                "out_trade_no" => "out_trade_no",
                "name" => "name",
                _ => "money",
            },
            order["params"][k].as_str().unwrap().to_owned(),
        );
    }
    assert_eq!(order["params"]["sign"].as_str().unwrap(), epay_sign(&p));

    // —— epay 回调：正确签名核销 ——
    let mut cb: BTreeMap<&str, String> = BTreeMap::new();
    cb.insert("pid", "1001".to_owned());
    cb.insert("trade_no", "EP123".to_owned());
    cb.insert("out_trade_no", order_no.clone());
    cb.insert("type", "alipay".to_owned());
    cb.insert("name", "okapi_topup".to_owned());
    cb.insert("money", "86.42".to_owned());
    cb.insert("trade_status", "TRADE_SUCCESS".to_owned());
    let sign = epay_sign(&cb);
    let qs = |cb: &BTreeMap<&str, String>, sign: &str| {
        let mut out = cb
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let _ = write!(out, "&sign={sign}&sign_type=MD5");
        out
    };
    let resp = client
        .get(format!(
            "http://{}/pay/callback/epay?{}",
            env.addr,
            qs(&cb, &sign)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.text().await.unwrap(),
        "success",
        "epay 协议要求纯文本应答"
    );
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        12_345_600,
        "按订单额度入账（非支付币种）"
    );
    let event = sqlx::query!(
        r#"SELECT actor, event_type FROM billing_events WHERE user_id = $1"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(event.actor, "system:payment");
    assert_eq!(event.event_type, "recharge");

    // 重放：仍 success（回调方停止重试）但不重复入账
    let replay = client
        .get(format!(
            "http://{}/pay/callback/epay?{}",
            env.addr,
            qs(&cb, &sign)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.text().await.unwrap(), "success");
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        12_345_600,
        "重放幂等"
    );

    // 错签名拒绝
    let bad = client
        .get(format!(
            "http://{}/pay/callback/epay?{}",
            env.addr,
            qs(&cb, "deadbeef")
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    // —— Stripe：下单（mock API 出 session） ——
    let order: Value = client
        .post(format!("http://{}/api/me/topup", env.addr))
        .bearer_auth(&env.user_token)
        .json(&json!({"amount_micro": 5_000_000, "gateway": "stripe"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let stripe_order_no = order["order_no"].as_str().unwrap().to_owned();
    assert_eq!(
        order["pay_url"],
        "https://checkout.stripe.test/pay/cs_test_1"
    );

    // webhook：HMAC 验签核销
    let payload = json!({
        "type": "checkout.session.completed",
        "data": {"object": {"id": "cs_test_1", "metadata": {"order_no": stripe_order_no}}}
    })
    .to_string();
    let ts = "1700000000";
    let mut mac = <Hmac<Sha256>>::new_from_slice(STRIPE_WH.as_bytes()).unwrap();
    mac.update(ts.as_bytes());
    mac.update(b".");
    mac.update(payload.as_bytes());
    let v1 = hex::encode(mac.finalize().into_bytes());
    let resp = client
        .post(format!("http://{}/pay/callback/stripe", env.addr))
        .header("stripe-signature", format!("t={ts},v1={v1}"))
        .header("content-type", "application/json")
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        12_345_600 + 5_000_000
    );

    // webhook 重放幂等 + 错签名拒绝
    let _ = client
        .post(format!("http://{}/pay/callback/stripe", env.addr))
        .header("stripe-signature", format!("t={ts},v1={v1}"))
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        12_345_600 + 5_000_000,
        "webhook 重放幂等"
    );
    let bad = client
        .post(format!("http://{}/pay/callback/stripe", env.addr))
        .header("stripe-signature", format!("t={ts},v1=deadbeef"))
        .body(payload)
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);

    // 订单状态：两单均 paid
    let paid = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM recharge_orders
           WHERE user_id = $1 AND status = 1"#,
        env.user_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(paid, 2);
}

/// 邀请返利（M4 aff）：邀请码生成 → 注册绑定 → 充值核销返利 → 门户统计。
#[tokio::test]
// 单场景端到端脚本，拆分损害时序可读性
#[allow(clippy::too_many_lines)]
async fn aff_reward_on_recharge() {
    let env = setup().await;
    let client = reqwest::Client::new();

    // 返利开关：10%（基点）
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('aff_percent_bp', '1000'::jsonb)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#
    )
    .execute(&env.pg)
    .await
    .unwrap();

    // 邀请人（env 用户）取邀请码（惰性生成 + 幂等）
    let aff: Value = client
        .get(format!("http://{}/api/me/aff", env.addr))
        .bearer_auth(&env.user_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let code = aff["aff_code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 8);
    let again: Value = client
        .get(format!("http://{}/api/me/aff", env.addr))
        .bearer_auth(&env.user_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["aff_code"].as_str().unwrap(), code, "邀请码应稳定");

    // 被邀请人注册（带邀请码）
    let suffix = Uuid::new_v4().simple().to_string();
    let email = format!("aff-{suffix}@test.local");
    let reg: Value = client
        .post(format!("http://{}/auth/register", env.addr))
        .json(&json!({
            "email": email, "username": format!("aff-{suffix}"),
            "password": "password123", "aff_code": code
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let invitee_id = reg["user_id"].as_i64().unwrap();

    // 被邀请人发 key 并充值 $10 → epay 回调核销
    let invitee_token = format!("sk-okapi-aff-{suffix}");
    let hash = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(invitee_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&env.pg, invitee_id, &hash, "sk-okapi-aff")
        .await
        .unwrap();
    let order: Value = client
        .post(format!("http://{}/api/me/topup", env.addr))
        .bearer_auth(&invitee_token)
        .json(&json!({"amount_micro": 10_000_000, "gateway": "epay"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let order_no = order["order_no"].as_str().unwrap().to_owned();

    let inviter_before = env.ledger.balance(env.user_id).await.unwrap().as_micros();

    let mut cb: BTreeMap<&str, String> = BTreeMap::new();
    cb.insert("pid", "1001".to_owned());
    cb.insert("trade_no", format!("EP-AFF-{suffix}"));
    cb.insert("out_trade_no", order_no);
    cb.insert("type", "alipay".to_owned());
    cb.insert("name", "okapi_topup".to_owned());
    cb.insert("money", "70.00".to_owned());
    cb.insert("trade_status", "TRADE_SUCCESS".to_owned());
    let sign = epay_sign(&cb);
    let mut qs = cb
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let _ = write!(qs, "&sign={sign}&sign_type=MD5");
    let resp = client
        .get(format!("http://{}/pay/callback/epay?{qs}", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "success");

    // 返利到账：$10 × 10% = 1_000_000 micro
    let inviter_after = env.ledger.balance(env.user_id).await.unwrap().as_micros();
    assert_eq!(
        inviter_after - inviter_before,
        1_000_000,
        "邀请人应得 10% 返利"
    );

    // 门户统计：邀请 1 人、累计返利
    let stat: Value = client
        .get(format!("http://{}/api/me/aff", env.addr))
        .bearer_auth(&env.user_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stat["invitees"], 1);
    assert_eq!(stat["reward_sum_micro"], 1_000_000);
}
