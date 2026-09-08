//! 通知多路验收（#1790-8，M4）：webhook 分发 / 事件订阅过滤 / 频率闸 / 余额低扫描。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Json;
use axum::routing::post;
use okapi::worker::notify;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

async fn spawn_sink() -> (
    SocketAddr,
    Arc<AtomicUsize>,
    Arc<std::sync::Mutex<Vec<Value>>>,
) {
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (h2, b2) = (Arc::clone(&hits), Arc::clone(&bodies));
    let app = axum::Router::new().route(
        "/hook",
        post(move |Json(v): Json<Value>| {
            let (h, b) = (Arc::clone(&h2), Arc::clone(&b2));
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                b.lock().unwrap().push(v);
                Json(json!({"ok": true}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, hits, bodies)
}

/// 用完即删的临时库：`notify_channels`、`balance_low_threshold_micro` 都是**全局**设置键，
/// 写在共享开发库上会与同二进制并行跑的 `notify_dispatch_and_mute` /
/// `balance_low_scan_respects_threshold` 互相覆盖。扫描与 Notifier 都指这一个库，
/// 于是低余额用户、冷却 key 也只有本用例造的那些，断言能钉死数量而不是"至少有一个"。
struct TempDb {
    pool: PgPool,
    admin: PgPool,
    name: String,
}

impl TempDb {
    async fn teardown(self) {
        self.pool.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#,
            self.name
        )))
        .execute(&self.admin)
        .await;
    }
}

/// 建临时库 + 一个订阅三类事件的 webhook 通道。静默键在 Redis 里按 `<idx>:<event>` 固定，
/// 先 DEL 掉——上一次跑留下的键会把本轮第一条吞掉，现象是"通知莫名其妙丢了"。
async fn temp_db_notifier(sink: &SocketAddr) -> (notify::Notifier, fred::clients::Client, TempDb) {
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let admin = okapi_store::connect_pg(&database_url).await.unwrap();
    let name = format!("okapi_ntf_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!(r#"CREATE DATABASE "{name}""#)))
        .execute(&admin)
        .await
        .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let pool = okapi_store::connect_pg(&format!("{base}/{name}"))
        .await
        .unwrap();
    okapi_store::run_migrations(&pool).await.unwrap();
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('notify_channels', $1)"#,
        json!([{ "type": "webhook", "url": format!("http://{sink}/hook"),
                 "events": ["drift", "channel_cooldown", "balance_low"],
                 "min_interval_secs": 1 }])
    )
    .execute(&pool)
    .await
    .unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    for event in ["drift", "channel_cooldown", "balance_low"] {
        let _: Option<i64> =
            fred::interfaces::KeysInterface::del(&redis, format!("notify:mute:0:{event}"))
                .await
                .unwrap();
    }
    let notifier = notify::Notifier::new(pool.clone(), redis.clone());
    (notifier, redis, TempDb { pool, admin, name })
}

/// worker 主循环那三条告警的**真实载荷**此前一条都没被核对过。
///
/// `notify_dispatch_and_mute` 验的是包络与频率闸，用的是一个合成事件名 `drift_<uuid>`；
/// `balance_low_scan_respects_threshold` 验的是扫描函数的返回值。中间那一段——扫出来的东西
/// 怎么拼成告警体——原先写在 `select!` 臂里，测试够不着，只能照抄一份 `json!` 自娱自乐。
/// 本轮把三处收进 `reconcile_and_notify` / `channel_cooldown_and_notify` /
/// `balance_low_and_notify`（`margin_breaker::evaluate_and_notify` 同一形状），这里在 mock
/// webhook 上逐字段核对：收告警的人得能直接看出"谁、多少、差在哪"，只发一个 count 的话
/// 还得自己回站上查。同时钉住"没事不吵"——阈值关着 / 零冷却 / 零差异都一条都不该发。
// 三类告警共用同一个临时库与同一个 sink，拆开就得建三份、且验不了"互不串台"
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn worker_alerts_carry_actionable_payloads() {
    dotenvy::dotenv().ok();
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let (sink, hits, bodies) = spawn_sink().await;
    let (notifier, redis, tmp) = temp_db_notifier(&sink).await;
    let ledger =
        okapi_ledger::BalanceLedger::new(okapi_store::connect_redis(&redis_url).await.unwrap());
    let take = |bodies: &Arc<std::sync::Mutex<Vec<Value>>>| -> Vec<Value> {
        std::mem::take(&mut *bodies.lock().unwrap())
    };

    // —— 三件事都没发生：一条都不该发 ——
    // 空库：没有用户、没有渠道 key、阈值键不存在
    assert_eq!(
        notify::balance_low_and_notify(&tmp.pool, &notifier)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        notify::channel_cooldown_and_notify(&tmp.pool, &notifier)
            .await
            .unwrap(),
        0
    );
    assert!(
        okapi::worker::reconcile_and_notify(&tmp.pool, &ledger, 1000, &notifier)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "无事发生时不得打扰运维");

    // —— drift：PG 快照与账本对不上（造一个只有快照、没有 billing_events 的用户）——
    let suffix = Uuid::new_v4().simple().to_string();
    let drifted = okapi_store::provision::create_user(&tmp.pool, &format!("dr-{suffix}"))
        .await
        .unwrap();
    sqlx::query!(
        r#"UPDATE users SET balance_micro = 12_345 WHERE id = $1"#,
        drifted
    )
    .execute(&tmp.pool)
    .await
    .unwrap();
    let drifts = okapi::worker::reconcile_and_notify(&tmp.pool, &ledger, 1000, &notifier)
        .await
        .unwrap();
    assert_eq!(drifts.len(), 1, "只该有本用例造的这一个：{drifts:?}");
    let sent = take(&bodies);
    assert_eq!(sent.len(), 1, "对账差异应告警一次：{sent:?}");
    assert_eq!(sent[0]["event"], "drift");
    assert!(sent[0]["at"].is_string());
    assert_eq!(sent[0]["payload"]["count"], 1);
    assert_eq!(
        sent[0]["payload"]["user_ids"],
        json!([drifted]),
        "得带上是谁——不然运维拿到 count=1 还是不知道去查哪个用户"
    );

    // —— channel_cooldown：status 2/3/4 都算冷却中 ——
    let (channel_id, _) = okapi_store::provision::create_channel(
        &tmp.pool,
        &format!("ch-{suffix}"),
        "openai",
        "http://127.0.0.1:1",
        "sk-x",
        &["m-x"],
        false,
        None,
    )
    .await
    .unwrap();
    sqlx::query!(
        r#"UPDATE channel_keys SET status = 3 WHERE channel_id = $1"#,
        channel_id
    )
    .execute(&tmp.pool)
    .await
    .unwrap();
    let cooling = notify::channel_cooldown_and_notify(&tmp.pool, &notifier)
        .await
        .unwrap();
    assert_eq!(cooling, 1);
    let sent = take(&bodies);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["event"], "channel_cooldown");
    assert_eq!(sent[0]["payload"]["count"], 1);

    // —— balance_low：阈值关着时连扫都不扫 ——
    sqlx::query!(
        r#"UPDATE users SET balance_micro = 100 WHERE id = $1"#,
        drifted
    )
    .execute(&tmp.pool)
    .await
    .unwrap();
    assert_eq!(
        notify::balance_low_and_notify(&tmp.pool, &notifier)
            .await
            .unwrap(),
        0,
        "阈值未配置时不告警"
    );
    assert!(take(&bodies).is_empty());

    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('balance_low_threshold_micro', '101'::jsonb)"#
    )
    .execute(&tmp.pool)
    .await
    .unwrap();
    assert_eq!(
        notify::balance_low_and_notify(&tmp.pool, &notifier)
            .await
            .unwrap(),
        1
    );
    let sent = take(&bodies);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["event"], "balance_low");
    assert_eq!(
        sent[0]["payload"]["users"],
        json!([{ "user_id": drifted, "balance_micro": 100 }]),
        "余额要跟着用户一起发：运维据此判断先给谁打电话"
    );

    // —— 频率闸对这三个事件同样生效（min_interval_secs=1，紧接着重发应被吞）——
    let before = hits.load(Ordering::SeqCst);
    notify::balance_low_and_notify(&tmp.pool, &notifier)
        .await
        .unwrap();
    assert_eq!(
        hits.load(Ordering::SeqCst),
        before,
        "静默期内同事件不得重发"
    );

    drop(redis);
    tmp.teardown().await;
}

/// 订阅命中才发、频率闸生效、事件包络字段完整。
#[tokio::test]
async fn notify_dispatch_and_mute() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();

    let (sink, hits, bodies) = spawn_sink().await;
    // 每次用独立事件名，避免与并行跑的其他测试互踩频率闸
    let event = format!("drift_{}", &Uuid::new_v4().simple().to_string()[..8]);

    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('notify_channels', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!([{
            "type": "webhook",
            "url": format!("http://{sink}/hook"),
            "events": [event],
            "min_interval_secs": 60
        }])
    )
    .execute(&pg)
    .await
    .unwrap();

    let notifier = notify::Notifier::new(pg.clone(), redis);

    // 命中订阅 → 发送
    notifier.dispatch(&event, &json!({"count": 2})).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "订阅事件应送达");
    {
        let got = bodies.lock().unwrap();
        assert_eq!(got[0]["event"], Value::String(event.clone()));
        assert_eq!(got[0]["payload"]["count"], 2);
        assert!(got[0]["at"].is_string(), "应带时间戳");
    }

    // 频率闸：静默期内重发被吞
    notifier.dispatch(&event, &json!({"count": 3})).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "静默期内不得重发");

    // 未订阅事件不发
    notifier
        .dispatch("some_other_event", &json!({"x": 1}))
        .await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "未订阅事件不得发送");
}

/// 余额低扫描：阈值关闭返回空；开启后返回低于阈值的用户。
#[tokio::test]
async fn balance_low_scan_respects_threshold() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 关闭（缺省/0）：空
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'balance_low_threshold_micro'"#)
        .execute(&pg)
        .await
        .unwrap();
    assert!(notify::scan_balance_low(&pg).await.unwrap().is_empty());

    // 造一个低余额用户并开启阈值
    let suffix = Uuid::new_v4().simple().to_string();
    let user = okapi_store::provision::create_user(&pg, &format!("low-{suffix}"))
        .await
        .unwrap();
    sqlx::query!(
        r#"UPDATE users SET balance_micro = 100 WHERE id = $1"#,
        user
    )
    .execute(&pg)
    .await
    .unwrap();
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('balance_low_threshold_micro', '101'::jsonb)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#
    )
    .execute(&pg)
    .await
    .unwrap();

    let low = notify::scan_balance_low(&pg).await.unwrap();
    assert!(
        low.iter().any(|(id, bal)| *id == user && *bal == 100),
        "低余额用户应被扫出（100 < 阈值 101 且降序排最前）：{low:?}"
    );

    // 清理阈值，避免影响并行测试的 worker 逻辑
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'balance_low_threshold_micro'"#)
        .execute(&pg)
        .await
        .unwrap();
    // 也清掉自己造的低余额用户：不清就会在开发库里越积越多，同额用户超过 LIMIT 20
    // 之后本用例必然挂——测试自污染，且现象是"扫不出刚建的用户"这种极难读的失败
    sqlx::query!(r#"UPDATE users SET deleted_at = now() WHERE id = $1"#, user)
        .execute(&pg)
        .await
        .unwrap();
}
