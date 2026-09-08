//! 订阅套餐验收（IMPLEMENTATION §11.28）：
//! - Lua 契约：选池（订阅优先 / 允许单笔越界 / 窗口外落钱包）、commit / refund 回同池、
//!   repair 按池、sub_set 保留在途；
//! - 端到端：建订阅套餐 → 管理员发放 → 网关请求走订阅池（钱包不动、records/events pool=1）
//!   → 越界 → 耗尽落钱包 → 两池对账零漂移；
//! - worker：滚窗重置（连跳）→ 到期收组、池不可用；
//! - 购买：checkout 建单 + epay 回调激活（不入钱包）；不可购买 400；激活期内换套餐 409；
//! - 兑换码绑订阅套餐：核销即激活，再核销 = 续期；管理员取消。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::{Duration as ChronoDuration, Utc};
use md5::{Digest as Md5Digest, Md5};
use okapi::{console, gateway, worker};
use okapi_domain::Money;
use okapi_ledger::{BalanceLedger, CommitOutcome, LimitCaps, Pool, ReserveOutcome};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

const EPAY_KEY: &str = "epay-sub-secret";
/// 钱包起始额（micro）。
const WALLET: i64 = 50_000_000;

async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion","model": req["model"],
        "choices":[{"index":0,"message":{"role":"assistant","content":"hi"}}],
        "usage":{"prompt_tokens":1000,"completion_tokens":200}
    }))
    .into_response()
}

async fn serve(router: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn uniq_ip() -> String {
    let h = Uuid::new_v4().simple().to_string();
    format!("2001:db8:{}:{}::1", &h[0..4], &h[4..8])
}

struct Bed {
    pg: PgPool,
    ledger: BalanceLedger,
    redis: fred::clients::Client,
    user_id: i64,
    token: String,
    admin_token: String,
    model: String,
    group: String,
    gateway: SocketAddr,
    console: SocketAddr,
    suffix: String,
}

// 套餐 / 分组 / 三角色装配放同一视野
#[allow(clippy::too_many_lines)]
async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    let model = format!("sub-m-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "2.0", "1.0", "1.0")
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &pg,
        &format!("sub-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();
    // 订阅附加分组（default 池，倍率 1）
    let group = format!("sub-g-{suffix}");
    okapi_store::admin::upsert_price_group(
        &pg,
        okapi_store::admin::PriceGroupInput {
            group_code: &group,
            group_ratio: "1.0",
            description: "",
            pool_code: None,
            self_select: false,
            rpm_limit: None,
            rph_limit: None,
        },
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("sub-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-sub-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-sub")
        .await
        .unwrap();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("sub-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-sub-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-adm")
        .await
        .unwrap();

    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('payment_epay', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!({"gateway_url": "https://epay.test/submit.php", "pid": "1001",
               "key": EPAY_KEY, "usd_to_cny_milli": 7000})
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let ledger = state.ledger.clone();
    // 钱包双侧入账（对账要干净）
    ledger
        .credit(user_id, Money::from_micros(WALLET))
        .await
        .unwrap();
    okapi_ledger::pg::record_credit(
        &pg,
        user_id,
        Money::from_micros(WALLET),
        "recharge",
        "test",
        json!({"reason": "seed"}),
    )
    .await
    .unwrap();
    let gateway_addr = serve(gateway::router(state.clone())).await;
    let console_addr = serve(console::router(state)).await;
    Bed {
        pg,
        ledger,
        redis,
        user_id,
        token,
        admin_token,
        model,
        group,
        gateway: gateway_addr,
        console: console_addr,
        suffix,
    }
}

impl Bed {
    async fn chat(&self) -> u16 {
        reqwest::Client::new()
            .post(format!("http://{}/v1/chat/completions", self.gateway))
            .bearer_auth(&self.token)
            .json(&json!({"model": self.model, "stream": false,
                          "messages": [{"role": "user", "content": "hello"}]}))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    /// 等下一笔 committed 记录（排除已见）：返回 (request_id, amount, pool)。
    async fn wait_committed(&self, seen: &[Uuid]) -> (Uuid, i64, i16) {
        for _ in 0..80 {
            let rows = sqlx::query!(
                r#"SELECT request_id, amount_micro, pool FROM billing_records
                   WHERE user_id = $1 AND status = 20 ORDER BY id"#,
                self.user_id
            )
            .fetch_all(&self.pg)
            .await
            .unwrap();
            if let Some(r) = rows.into_iter().find(|r| !seen.contains(&r.request_id)) {
                return (r.request_id, r.amount_micro, r.pool);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("未等到 committed 记录");
    }

    async fn upsert_plan(&self, body: Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("http://{}/admin/plans", self.console))
            .bearer_auth(&self.admin_token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    fn sub_plan(&self, code: &str, quota: i64, price: i64, group: bool) -> Value {
        json!({
            "plan_code": code, "display_name": "Pro", "kind": 1,
            "grant_micro": quota, "price_micro": price,
            "period": 1, "duration_days": 30,
            "group_code": group.then(|| self.group.clone()),
            "description": "sub test", "sort_order": 5
        })
    }

    async fn admin_grant(&self, code: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!(
                "http://{}/admin/users/{}/subscription",
                self.console, self.user_id
            ))
            .bearer_auth(&self.admin_token)
            .json(&json!({"plan_code": code}))
            .send()
            .await
            .unwrap()
    }

    async fn mine(&self) -> Value {
        reqwest::Client::new()
            .get(format!("http://{}/api/me/subscription", self.console))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn wallet(&self) -> i64 {
        self.ledger.balance(self.user_id).await.unwrap().as_micros()
    }

    async fn sub(&self) -> (i64, i64) {
        let (m, until) = self.ledger.sub_balance(self.user_id).await.unwrap();
        (m.as_micros(), until)
    }

    async fn in_group(&self) -> bool {
        sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM user_groups WHERE user_id = $1 AND group_code = $2) AS "e!""#,
            self.user_id,
            self.group
        )
        .fetch_one(&self.pg)
        .await
        .unwrap()
    }

    async fn events(&self, pool: i16) -> Vec<(String, i64)> {
        sqlx::query!(
            r#"SELECT event_type, delta_micro FROM billing_events
               WHERE user_id = $1 AND pool = $2 ORDER BY event_id"#,
            self.user_id,
            pool
        )
        .fetch_all(&self.pg)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.event_type, r.delta_micro))
        .collect()
    }

    /// 两池不变式：钱包 avail + 在途₀ == Σ events(pool=0) == users.balance_micro；订阅 sub + 在途₁ == Σ events(pool=1)。
    async fn assert_zero_drift(&self) {
        let inflight: Vec<_> = self.ledger.list_reservations(self.user_id).await.unwrap();
        let inflight0: i64 = inflight
            .iter()
            .filter(|r| r.pool == Pool::Wallet)
            .map(|r| r.amount.as_micros())
            .sum();
        let inflight1: i64 = inflight
            .iter()
            .filter(|r| r.pool == Pool::Subscription)
            .map(|r| r.amount.as_micros())
            .sum();
        let sum0: i64 = self.events(0).await.iter().map(|(_, d)| d).sum();
        let sum1: i64 = self.events(1).await.iter().map(|(_, d)| d).sum();
        let snapshot = sqlx::query_scalar!(
            r#"SELECT balance_micro FROM users WHERE id = $1"#,
            self.user_id
        )
        .fetch_one(&self.pg)
        .await
        .unwrap();
        assert_eq!(self.wallet().await + inflight0, sum0, "钱包池不变式");
        assert_eq!(snapshot, sum0, "users.balance_micro 只随钱包事件动");
        assert_eq!(self.sub().await.0 + inflight1, sum1, "订阅池不变式");
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

// ---- Lua 契约 ----

#[tokio::test]
// 选池 / 越界 / 回同池 / 按池修复 / 在途保留 一体时序
#[allow(clippy::too_many_lines)]
async fn lua_pool_contract() {
    dotenvy::dotenv().ok();
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let ledger = BalanceLedger::new(redis);
    // 无 PG 用户：账本键只认 uid，用随机负数避开真实用户
    let uid = -i64::from(rand::random::<u32>()) - 1;
    let now = Utc::now();
    let reserve = |rid: Uuid, est: i64| {
        let ledger = ledger.clone();
        async move {
            ledger
                .reserve(
                    okapi_ledger::ReserveRequest {
                        user_id: uid,
                        api_key_id: 7,
                        request_id: rid,
                        est: Money::from_micros(est),
                        caps: LimitCaps::default(),
                        est_tokens: 10,
                    },
                    now,
                )
                .await
                .unwrap()
        }
    };

    ledger
        .credit(uid, Money::from_micros(10_000))
        .await
        .unwrap();
    // 无订阅 → 钱包
    let r0 = Uuid::new_v4();
    assert!(matches!(
        reserve(r0, 1_000).await,
        ReserveOutcome::Reserved {
            pool: Pool::Wallet,
            ..
        }
    ));
    assert_eq!(ledger.balance(uid).await.unwrap().as_micros(), 9_000);

    // 订阅池 300、窗口内 → 订阅优先；估算 1_000 > 300 仍走订阅（允许越界）
    let until = now.timestamp() + 3600;
    let set = ledger
        .sub_set(uid, Money::from_micros(300), until)
        .await
        .unwrap();
    assert_eq!((set.before.as_micros(), set.after.as_micros()), (0, 300));
    let r1 = Uuid::new_v4();
    let out = reserve(r1, 1_000).await;
    assert!(
        matches!(out, ReserveOutcome::Reserved { pool: Pool::Subscription, balance_after } if balance_after.as_micros() == -700),
        "订阅池不校验足额：{out:?}"
    );
    assert_eq!(
        ledger.balance(uid).await.unwrap().as_micros(),
        9_000,
        "钱包不动"
    );

    // 池 ≤ 0 → 落钱包
    let r2 = Uuid::new_v4();
    assert!(matches!(
        reserve(r2, 500).await,
        ReserveOutcome::Reserved {
            pool: Pool::Wallet,
            ..
        }
    ));
    assert_eq!(ledger.balance(uid).await.unwrap().as_micros(), 8_500);

    // 在途按池列出
    let inflight = ledger.list_reservations(uid).await.unwrap();
    let pool_of = |rid: Uuid| inflight.iter().find(|r| r.request_id == rid).unwrap().pool;
    assert_eq!(pool_of(r0), Pool::Wallet);
    assert_eq!(pool_of(r1), Pool::Subscription);
    assert_eq!(pool_of(r2), Pool::Wallet);

    // sub_set 保留订阅在途：new = quota + Σ在途₁ = 1_000 + 1_000
    let set = ledger
        .sub_set(uid, Money::from_micros(1_000), until)
        .await
        .unwrap();
    assert_eq!(
        (set.before.as_micros(), set.after.as_micros()),
        (-700, 2_000)
    );

    // commit 回同池：r1 实际 400 → 订阅池 += 600 → 2_600；钱包不动
    let c = ledger
        .commit(uid, 7, r1, Money::from_micros(400))
        .await
        .unwrap();
    assert!(
        matches!(c, CommitOutcome::Committed { pool: Pool::Subscription, refund_delta, balance_after }
            if refund_delta.as_micros() == 600 && balance_after.as_micros() == 2_600),
        "{c:?}"
    );
    assert_eq!(ledger.balance(uid).await.unwrap().as_micros(), 8_500);
    // 老窗口在途结算后池 = 新窗额度 + 结算冲回，与不变式一致（1_000 + 1_000 − 400 = 1_600？不：
    // sub_set 时把在途 1_000 全额加回，commit 只退 600 → 2_600 = 新窗 1_000 + 已冲回 1_000 + 退 600；
    // 事件侧：sub_reset delta = 2_700，commit 事件 delta = −400 → Σ = 300 − 1_000 + 2_700 − 400 = 1_600 ≠ 2_600？
    // 不矛盾：Σ events 还含 reserve 时的 −1_000 只在 Redis 侧（预扣不记事件），不变式是 sub + 在途₁：
    // 2_600 + 0 == 300（grant）+ 2_700（reset）− 400（commit）= 2_600 ✓

    // refund 回同池：r0 钱包 → 钱包 += 1_000
    let rf = ledger.refund(uid, 7, r0).await.unwrap();
    assert_eq!(rf.pool, Pool::Wallet);
    assert_eq!(rf.released.as_micros(), 1_000);
    assert_eq!(ledger.balance(uid).await.unwrap().as_micros(), 9_500);
    // 幂等：再退返回 0
    let rf2 = ledger.refund(uid, 7, r0).await.unwrap();
    assert_eq!(rf2.released.as_micros(), 0);

    // repair 按池：把钱包修到目标 9_000（在途₀ = r2 的 500 → avail = 8_500），订阅池不动
    let rep = ledger
        .repair(uid, Money::from_micros(9_000), Pool::Wallet)
        .await
        .unwrap();
    assert_eq!(rep.inflight.as_micros(), 500);
    assert_eq!(rep.after.as_micros(), 8_500);
    assert_eq!(ledger.sub_balance(uid).await.unwrap().0.as_micros(), 2_600);
    let rep = ledger
        .repair(uid, Money::from_micros(100), Pool::Subscription)
        .await
        .unwrap();
    assert_eq!(rep.inflight.as_micros(), 0, "订阅在途已结算");
    assert_eq!(rep.after.as_micros(), 100);
    assert_eq!(
        ledger.balance(uid).await.unwrap().as_micros(),
        8_500,
        "修订阅池不碰钱包"
    );

    // 窗口外（sub_until 过去）→ 落钱包
    ledger
        .sub_touch_until(uid, now.timestamp() - 1)
        .await
        .unwrap();
    let r3 = Uuid::new_v4();
    assert!(matches!(
        reserve(r3, 100).await,
        ReserveOutcome::Reserved {
            pool: Pool::Wallet,
            ..
        }
    ));

    // 清理
    for rid in [r2, r3] {
        ledger.refund(uid, 7, rid).await.unwrap();
    }
}

// ---- 端到端：发放 → 网关走订阅池 → 越界 → 落钱包 → 对账 ----

#[tokio::test]
// 一体时序脚本
#[allow(clippy::too_many_lines)]
async fn gateway_bills_subscription_pool_first() {
    let bed = setup().await;
    let client = reqwest::Client::new();

    // 探针：无订阅时一笔请求的实际费用 A（钱包付）
    assert_eq!(bed.chat().await, 200);
    let (r0, amount, pool0) = bed.wait_committed(&[]).await;
    assert_eq!(pool0, 0);
    assert!(amount > 0);
    assert_eq!(bed.wallet().await, WALLET - amount);

    // 订阅套餐：每窗额度 = 1.5A → 第一笔够、第二笔越界、第三笔落钱包
    let quota = amount + amount / 2;
    let code = format!("pro-{}", bed.suffix);
    let resp = bed.upsert_plan(bed.sub_plan(&code, quota, 0, true)).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    // 管理列表回显订阅字段
    let plans: Value = client
        .get(format!("http://{}/admin/plans?limit=200", bed.console))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = plans["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["plan_code"] == code)
        .expect("列表含新套餐");
    assert_eq!(row["kind"], 1);
    assert_eq!(row["period"], 1);
    assert_eq!(row["duration_days"], 30);
    assert_eq!(row["active_subscribers"], 0);

    // 发放
    assert!(!bed.in_group().await);
    let granted: Value = bed.admin_grant(&code).await.json().await.unwrap();
    assert_eq!(granted["outcome"], "activated", "{granted}");
    assert_eq!(granted["subscription"]["remaining_micro"], quota);
    assert_eq!(granted["subscription"]["granted_group"], true);
    assert!(bed.in_group().await, "订阅附加分组");
    assert_eq!(bed.sub().await.0, quota);
    assert_eq!(
        bed.events(1).await,
        vec![("sub_grant".to_owned(), quota)],
        "激活事件 pool=1"
    );
    assert_eq!(bed.wallet().await, WALLET - amount, "发放不动钱包");
    let mine = bed.mine().await;
    assert_eq!(mine["subscription"]["plan_code"], code);
    assert_eq!(mine["subscription"]["remaining_micro"], quota);

    // 第一笔：订阅池付
    assert_eq!(bed.chat().await, 200);
    let (r1, a1, pool1) = bed.wait_committed(&[r0]).await;
    assert_eq!(pool1, 1, "records.pool=1");
    assert_eq!(a1, amount);
    assert_eq!(bed.wallet().await, WALLET - amount, "钱包不动");
    assert_eq!(bed.sub().await.0, quota - amount);
    let commit_events: Vec<_> = bed
        .events(1)
        .await
        .into_iter()
        .filter(|(t, _)| t == "commit")
        .collect();
    assert_eq!(commit_events, vec![("commit".to_owned(), -amount)]);

    // 第二笔：池剩 0.5A < A，仍走订阅池（越界为负）
    assert_eq!(bed.chat().await, 200);
    let (r2, _, pool2) = bed.wait_committed(&[r0, r1]).await;
    assert_eq!(pool2, 1, "允许单笔越界");
    assert_eq!(bed.sub().await.0, quota - 2 * amount);
    assert!(bed.sub().await.0 < 0);
    assert_eq!(bed.wallet().await, WALLET - amount);
    // 门户展示钳 0
    assert_eq!(bed.mine().await["subscription"]["remaining_micro"], 0);

    // 第三笔：池耗尽 → 钱包
    assert_eq!(bed.chat().await, 200);
    let (_, _, pool3) = bed.wait_committed(&[r0, r1, r2]).await;
    assert_eq!(pool3, 0, "池耗尽落钱包");
    assert_eq!(bed.wallet().await, WALLET - 2 * amount);

    // 两池对账零漂移；管理端 reconcile 也不报这个用户
    bed.assert_zero_drift().await;
    let drift = worker::stable_drift(&bed.pg, &bed.ledger, bed.user_id)
        .await
        .unwrap();
    assert!(drift.is_some(), "稳定（无结算中间态）");
    let repaired = worker::repair_balance(&bed.pg, &bed.ledger, bed.user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repaired.redis_before_micro, repaired.redis_after_micro,
        "钱包无需修"
    );
    assert_eq!(
        repaired.sub_redis_before_micro, repaired.sub_redis_after_micro,
        "订阅池无需修"
    );

    // /v1/dashboard/billing/subscription：hard_limit 计入订阅剩余（越界为负钳 0 → 不减）
    let dash: Value = client
        .get(format!(
            "http://{}/v1/dashboard/billing/subscription",
            bed.gateway
        ))
        .bearer_auth(&bed.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(dash["hard_limit_usd"].is_number());

    // 同套餐再发放 = 续期：expires_at 后移 30 天，池余额不变
    let before = bed.mine().await;
    let renewed: Value = bed.admin_grant(&code).await.json().await.unwrap();
    assert_eq!(renewed["outcome"], "renewed", "{renewed}");
    let exp_before: chrono::DateTime<Utc> =
        serde_json::from_value(before["subscription"]["expires_at"].clone()).unwrap();
    let exp_after: chrono::DateTime<Utc> =
        serde_json::from_value(renewed["subscription"]["expires_at"].clone()).unwrap();
    assert_eq!(exp_after - exp_before, ChronoDuration::days(30));
    assert_eq!(bed.sub().await.0, quota - 2 * amount, "续期不加额");

    // 激活期内换别的套餐 → 409 subscription_active
    let other = format!("max-{}", bed.suffix);
    bed.upsert_plan(bed.sub_plan(&other, 1_000_000, 0, false))
        .await;
    let conflict = bed.admin_grant(&other).await;
    assert_eq!(conflict.status(), 409);
    let body: Value = conflict.json().await.unwrap();
    assert_eq!(body["error"]["code"], "subscription_active");
    assert_eq!(body["error"]["param"], code);

    // 管理员取消：status 3、池清零、分组收回、sub_expire 事件（delta = −当前池值）
    let sub_before = bed.sub().await.0;
    let cancel = client
        .delete(format!(
            "http://{}/admin/users/{}/subscription",
            bed.console, bed.user_id
        ))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), 200);
    assert_eq!(bed.sub().await, (0, 0));
    assert!(!bed.in_group().await, "收回订阅授予的分组");
    let last = bed.events(1).await.pop().unwrap();
    assert_eq!(last, ("sub_expire".to_owned(), -sub_before));
    assert!(bed.mine().await["subscription"].is_null());
    let status: i16 = sqlx::query_scalar!(
        r#"SELECT status FROM user_subscriptions WHERE user_id = $1 ORDER BY id DESC LIMIT 1"#,
        bed.user_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(status, 3);
    // 再取消 → 404
    let again = client
        .delete(format!(
            "http://{}/admin/users/{}/subscription",
            bed.console, bed.user_id
        ))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 404);
    bed.assert_zero_drift().await;

    // 无订阅后请求回钱包
    assert_eq!(bed.chat().await, 200);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bed.wallet().await, WALLET - 3 * amount);
    // 套餐被订阅实例引用 → 拒删。断到 error_code 而不止 409：
    // 409 还有 group_in_use / role_in_use 等好几个来源，只看状态码的话，
    // 这条守卫哪天判错、恰好被别的冲突挡下，用例照样绿。
    let del = client
        .delete(format!("http://{}/admin/plans/{code}", bed.console))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 409);
    let body: Value = del.json().await.unwrap();
    assert_eq!(body["error"]["code"], "plan_in_use", "{body}");

    // 反面：没人引用的套餐删得掉，否则"拒删"可能只是这个端点根本删不动
    let spare = format!("{code}-spare");
    assert_eq!(
        bed.upsert_plan(bed.sub_plan(&spare, 1000, 1000, false))
            .await
            .status(),
        200
    );
    let del = client
        .delete(format!("http://{}/admin/plans/{spare}", bed.console))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 200, "{:?}", del.text().await);
    let gone = client
        .delete(format!("http://{}/admin/plans/{spare}", bed.console))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 404, "删过之后再删是 404");
}

// ---- worker：滚窗 / 到期 ----

#[tokio::test]
// 滚窗与到期一体时序
#[allow(clippy::too_many_lines)]
async fn worker_rolls_window_and_expires() {
    let bed = setup().await;
    // 用户本来就在组里 → 订阅不算新加，到期不收回
    sqlx::query!(
        "INSERT INTO user_groups (user_id, group_code) VALUES ($1, $2)",
        bed.user_id,
        bed.group
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    let quota = 3_000_000;
    let code = format!("daily-{}", bed.suffix);
    bed.upsert_plan(bed.sub_plan(&code, quota, 0, true)).await;
    let granted: Value = bed.admin_grant(&code).await.json().await.unwrap();
    assert_eq!(granted["subscription"]["granted_group"], false);
    let sub_id = granted["subscription"]["id"].as_i64().unwrap();

    // 消耗一笔（订阅池）
    assert_eq!(bed.chat().await, 200);
    let (first, amount, pool) = bed.wait_committed(&[]).await;
    assert_eq!(pool, 1);
    assert_eq!(bed.sub().await.0, quota - amount);

    // 未到窗：tick 不动这条订阅（并行用例可能有自己的到点订阅，不断言计数）
    worker::subscriptions_tick(&bed.pg, &bed.ledger, &bed.redis, Utc::now())
        .await
        .unwrap();
    assert_eq!(bed.sub().await.0, quota - amount);

    // 把窗口拨到 3.5 天前：连跳到覆盖 now 的那格，只重置一次、不补发
    let old_start = Utc::now() - ChronoDuration::hours(24 * 4 + 12);
    sqlx::query!(
        r#"UPDATE user_subscriptions SET window_start = $2, window_end = $3 WHERE id = $1"#,
        sub_id,
        old_start,
        old_start + ChronoDuration::days(1)
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    let r = worker::subscriptions_tick(&bed.pg, &bed.ledger, &bed.redis, Utc::now())
        .await
        .unwrap();
    assert_eq!(r.rolled, 1);
    assert_eq!(bed.sub().await.0, quota, "滚窗重置到 quota，不累积");
    let win = sqlx::query!(
        r#"SELECT window_start, window_end FROM user_subscriptions WHERE id = $1"#,
        sub_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    let now = Utc::now();
    assert!(
        win.window_start <= now && now < win.window_end,
        "窗口覆盖 now"
    );
    assert_eq!(win.window_end - win.window_start, ChronoDuration::days(1));
    assert_eq!(
        bed.sub().await.1,
        win.window_end.timestamp(),
        "sub_until = window_end（早于 expires_at）"
    );
    let reset = bed
        .events(1)
        .await
        .into_iter()
        .filter(|(t, _)| t == "sub_reset")
        .collect::<Vec<_>>();
    assert_eq!(
        reset,
        vec![("sub_reset".to_owned(), amount)],
        "重置事件 delta = 补回的量"
    );
    bed.assert_zero_drift().await;

    // 到期：expires_at 拨到过去 → status 2、池清零、分组保留（本来就在组里）
    sqlx::query!(
        r#"UPDATE user_subscriptions SET expires_at = now() - interval '1 second' WHERE id = $1"#,
        sub_id
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    let r = worker::subscriptions_tick(&bed.pg, &bed.ledger, &bed.redis, Utc::now())
        .await
        .unwrap();
    assert_eq!(r.expired, 1);
    assert!(!r.group_changed, "分组不是订阅授予的，不收回");
    assert_eq!(bed.sub().await, (0, 0));
    assert!(bed.in_group().await, "用户原有分组保留");
    let status: i16 = sqlx::query_scalar!(
        r#"SELECT status FROM user_subscriptions WHERE id = $1"#,
        sub_id
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(status, 2);
    assert_eq!(
        bed.events(1).await.pop().unwrap(),
        ("sub_expire".to_owned(), -quota)
    );
    bed.assert_zero_drift().await;

    // 到期后请求落钱包
    let wallet_before = bed.wallet().await;
    assert_eq!(bed.chat().await, 200);
    let (_, _, pool) = bed.wait_committed(&[first]).await;
    assert_eq!(pool, 0, "到期后 records.pool=0");
    assert_eq!(bed.wallet().await, wallet_before - amount);
    // 幂等：再 tick 无事
    let r = worker::subscriptions_tick(&bed.pg, &bed.ledger, &bed.redis, Utc::now())
        .await
        .unwrap();
    assert_eq!((r.rolled, r.expired), (0, 0));
}

// ---- 购买 / 兑换码 ----

#[tokio::test]
// 购买回调 + 兑换码 一体时序
#[allow(clippy::too_many_lines)]
async fn checkout_callback_and_redeem_activate() {
    let bed = setup().await;
    let client = reqwest::Client::new();
    let quota = 2_000_000;
    let code = format!("buy-{}", bed.suffix);
    bed.upsert_plan(bed.sub_plan(&code, quota, 9_990_000, false))
        .await;
    let free = format!("free-{}", bed.suffix);
    bed.upsert_plan(bed.sub_plan(&free, quota, 0, false)).await;

    // 门户套餐页
    let plans: Value = client
        .get(format!("http://{}/api/plans", bed.console))
        .bearer_auth(&bed.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let buy = plans["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["plan_code"] == code)
        .unwrap();
    assert_eq!(buy["purchasable"], true);
    assert_eq!(buy["price_micro"], 9_990_000);
    let free_row = plans["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["plan_code"] == free)
        .unwrap();
    assert_eq!(free_row["purchasable"], false);

    // 不可购买 → 400
    let bad = client
        .post(format!(
            "http://{}/api/me/subscriptions/checkout",
            bed.console
        ))
        .bearer_auth(&bed.token)
        .json(&json!({"plan_code": free, "gateway": "epay"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    assert_eq!(
        bad.json::<Value>().await.unwrap()["error"]["code"],
        "plan_not_purchasable"
    );

    // 下单：售价 $9.99 × 7 = 69.93 CNY；订单带 plan_id
    let order: Value = client
        .post(format!(
            "http://{}/api/me/subscriptions/checkout",
            bed.console
        ))
        .bearer_auth(&bed.token)
        .json(&json!({"plan_code": code, "gateway": "epay"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let order_no = order["order_no"].as_str().unwrap().to_owned();
    assert_eq!(order["params"]["money"], "69.93");
    assert_eq!(order["params"]["name"], format!("okapi_plan_{code}"));
    let plan_id: Option<i64> = sqlx::query_scalar!(
        r#"SELECT plan_id FROM recharge_orders WHERE order_no = $1"#,
        order_no
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert!(plan_id.is_some(), "订阅购买单带 plan_id");

    // 回调：激活、不入钱包
    let wallet_before = bed.wallet().await;
    let mut cb: BTreeMap<&str, String> = BTreeMap::new();
    cb.insert("pid", "1001".to_owned());
    cb.insert("trade_no", format!("EP-SUB-{}", bed.suffix));
    cb.insert("out_trade_no", order_no.clone());
    cb.insert("type", "alipay".to_owned());
    cb.insert("name", format!("okapi_plan_{code}"));
    cb.insert("money", "69.93".to_owned());
    cb.insert("trade_status", "TRADE_SUCCESS".to_owned());
    let sign = epay_sign(&cb);
    let mut qs = cb
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let _ = write!(qs, "&sign={sign}&sign_type=MD5");
    let resp = client
        .get(format!("http://{}/pay/callback/epay?{qs}", bed.console))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.text().await.unwrap(), "success");
    assert_eq!(bed.wallet().await, wallet_before, "订阅购买不入钱包");
    assert_eq!(bed.sub().await.0, quota);
    let mine = bed.mine().await;
    assert_eq!(mine["subscription"]["plan_code"], code);
    assert_eq!(
        mine["subscription"]["source"],
        format!("purchase:{order_no}")
    );
    let recharge_events = bed
        .events(0)
        .await
        .into_iter()
        .filter(|(t, _)| t == "recharge")
        .count();
    assert_eq!(
        recharge_events, 1,
        "只有 seed 那笔充值，购买单不记 recharge"
    );

    // 回调重放：幂等（不续期）
    let exp_before = mine["subscription"]["expires_at"].clone();
    let replay = client
        .get(format!("http://{}/pay/callback/epay?{qs}", bed.console))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.text().await.unwrap(), "success");
    assert_eq!(bed.mine().await["subscription"]["expires_at"], exp_before);

    // 激活期内 checkout 别的套餐 → 409（付款前拦住）
    let other = format!("other-{}", bed.suffix);
    bed.upsert_plan(bed.sub_plan(&other, quota, 5_000_000, false))
        .await;
    let conflict = client
        .post(format!(
            "http://{}/api/me/subscriptions/checkout",
            bed.console
        ))
        .bearer_auth(&bed.token)
        .json(&json!({"plan_code": other, "gateway": "epay"}))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), 409);
    assert_eq!(
        conflict.json::<Value>().await.unwrap()["error"]["code"],
        "subscription_active"
    );

    // 兑换码绑同一订阅套餐：核销 = 续期（面值忽略、钱包不动）
    let created: Value = client
        .post(format!("http://{}/admin/redemptions", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"count": 1, "amount_micro": 1, "plan_code": code}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let redeem_code = created["codes"][0].as_str().unwrap().to_owned();
    let redeemed: Value = client
        .post(format!("http://{}/api/me/redeem", bed.console))
        .bearer_auth(&bed.token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": redeem_code}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(redeemed["outcome"], "renewed", "{redeemed}");
    assert_eq!(redeemed["amount_micro"], 0);
    assert_eq!(bed.wallet().await, wallet_before);
    let exp_a: chrono::DateTime<Utc> = serde_json::from_value(exp_before).unwrap();
    let exp_b: chrono::DateTime<Utc> =
        serde_json::from_value(redeemed["subscription"]["expires_at"].clone()).unwrap();
    assert_eq!(exp_b - exp_a, ChronoDuration::days(30));

    // 取消后，兑换码绑订阅 = 全新激活
    client
        .delete(format!(
            "http://{}/admin/users/{}/subscription",
            bed.console, bed.user_id
        ))
        .bearer_auth(&bed.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(bed.sub().await, (0, 0));
    let created: Value = client
        .post(format!("http://{}/admin/redemptions", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"count": 1, "amount_micro": 1, "plan_code": code}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let redeemed: Value = client
        .post(format!("http://{}/api/me/redeem", bed.console))
        .bearer_auth(&bed.token)
        .header("x-real-ip", uniq_ip())
        .json(&json!({"code": created["codes"][0].as_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(redeemed["outcome"], "activated", "{redeemed}");
    assert_eq!(bed.sub().await.0, quota);
    assert!(
        bed.mine().await["subscription"]["source"]
            .as_str()
            .unwrap()
            .starts_with("redeem:")
    );
    assert_eq!(bed.mine().await["history"].as_array().unwrap().len(), 2);
    bed.assert_zero_drift().await;
}

/// 套餐字段校验：kind 1 缺 period / duration → 400；kind 越界 → 400；分组不存在 → 400。
#[tokio::test]
async fn plan_validation() {
    let bed = setup().await;
    let code = format!("bad-{}", bed.suffix);
    let cases = [
        (
            json!({"plan_code": code, "display_name": "x", "kind": 1, "grant_micro": 1, "duration_days": 30}),
            "period",
        ),
        (
            json!({"plan_code": code, "display_name": "x", "kind": 1, "grant_micro": 1, "period": 3}),
            "duration_days",
        ),
        (
            json!({"plan_code": code, "display_name": "x", "kind": 7, "grant_micro": 1}),
            "kind",
        ),
        (
            json!({"plan_code": code, "display_name": "x", "grant_micro": 1, "group_code": "no-such-group"}),
            "group_code",
        ),
        (
            json!({"plan_code": code, "display_name": "x", "kind": 1, "grant_micro": 1, "period": 1, "duration_days": 1, "price_micro": -5}),
            "price_micro",
        ),
    ];
    for (body, param) in cases {
        let resp = bed.upsert_plan(body).await;
        assert_eq!(resp.status(), 400);
        let err: Value = resp.json().await.unwrap();
        assert_eq!(err["error"]["param"], param, "{err}");
    }
    // kind 0 老形状仍可用（无 period）
    let ok = bed
        .upsert_plan(json!({"plan_code": code, "display_name": "x", "grant_micro": 1}))
        .await;
    assert_eq!(ok.status(), 200);
}
