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
    sqlx::query("INSERT INTO settings(key,value) VALUES ('ssrf_policy',$1) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").bind(json!({"allow_http":true,"allow_private":true})).execute(&pool).await.unwrap();
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
    okapi_store::test_support::assert_isolated();
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
        okapi::worker::reconcile_and_notify(&tmp.pool, &ledger, 1000, None, &notifier)
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
    let drifts = okapi::worker::reconcile_and_notify(&tmp.pool, &ledger, 1000, None, &notifier)
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

    // —— channel_cooldown：invalid 必须告警并带可定位的 key id ——
    let (channel_id, channel_key_id) = okapi_store::provision::create_channel(
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
        r#"UPDATE channel_keys SET status = 6 WHERE channel_id = $1"#,
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

    assert_eq!(
        sent[0]["payload"]["channel_key_ids"],
        json!([channel_key_id])
    );
    sqlx::query("UPDATE channel_keys SET status=5 WHERE channel_id=$1")
        .bind(channel_id)
        .execute(&tmp.pool)
        .await
        .unwrap();
    assert_eq!(
        notify::channel_cooldown_and_notify(&tmp.pool, &notifier)
            .await
            .unwrap(),
        0,
        "manual bans are intentional, no alert"
    );

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
    assert!(take(&bodies).is_empty(), "take(&bodies) should be empty");

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
        json!([{ "user_id": drifted }]),
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

/// 对账恰好扫在结算窗口里（Redis 已扣、PG 事件还没落）看到的差异不是漂移：不记「不会自愈」的 error，
/// 也不发 `drift` 通知。线上 2026-10-29 为此记过一条，差额正好是同一秒结算的那笔请求（66,249µ），
/// 之后十几个小时零差异。真漂移（隔了窗口仍在）照报，并且复核会让这一轮多等一个窗口。
#[tokio::test]
async fn a_gap_that_closes_within_the_settle_window_is_not_alerted() {
    okapi_store::test_support::assert_isolated();
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let (sink, hits, bodies) = spawn_sink().await;
    let (notifier, redis, tmp) = temp_db_notifier(&sink).await;
    let ledger =
        okapi_ledger::BalanceLedger::new(okapi_store::connect_redis(&redis_url).await.unwrap());

    // 起手三处一致：Redis 入账 + PG 事件 + 展示快照
    let suffix = Uuid::new_v4().simple().to_string();
    let user = okapi_store::provision::create_user(&tmp.pool, &format!("st-{suffix}"))
        .await
        .unwrap();
    let amount = okapi_domain::Money::from_micros(9_000_000);
    ledger.credit(user, amount).await.unwrap();
    okapi_ledger::pg::record_credit(&tmp.pool, user, amount, "adjust", "test", json!({}))
        .await
        .unwrap();
    assert!(
        okapi::worker::reconcile_and_notify(&tmp.pool, &ledger, 1000, None, &notifier)
            .await
            .unwrap()
            .is_empty(),
        "起手应当一致"
    );

    // Redis 先动了，账本 0.7 秒后才追平：首轮能扫到，但窗口内就消失了
    ledger.drain(user).await.unwrap();
    let catch_up = {
        let ledger = ledger.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
            ledger.credit(user, amount).await.unwrap();
        })
    };
    let drifts = okapi::worker::reconcile_and_notify(&tmp.pool, &ledger, 1000, None, &notifier)
        .await
        .unwrap();
    catch_up.await.unwrap();
    assert!(drifts.is_empty(), "窗口里的瞬时态不是漂移：{drifts:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "瞬时态不得惊动运维");

    // 真漂移：窗口过了仍然对不上 → 照报，且是复核时的读数；这一轮确实多等了一个窗口
    ledger.drain(user).await.unwrap();
    let started = std::time::Instant::now();
    let drifts = okapi::worker::reconcile_and_notify(&tmp.pool, &ledger, 1000, None, &notifier)
        .await
        .unwrap();
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(1400),
        "复核要隔一个结算窗口：{:?}",
        started.elapsed()
    );
    assert_eq!(drifts.len(), 1, "{drifts:?}");
    assert_eq!(drifts[0].user_id, user);
    assert_eq!(drifts[0].events_sum_micro, 9_000_000);
    assert_eq!(drifts[0].redis_effective_micro, 0);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "真漂移要告警一次");
    let sent = std::mem::take(&mut *bodies.lock().unwrap());
    assert_eq!(sent[0]["event"], "drift");
    assert_eq!(sent[0]["payload"]["user_ids"], json!([user]));

    drop(redis);
    tmp.teardown().await;
}

/// 订阅命中才发、频率闸生效、事件包络字段完整。
#[tokio::test]
async fn notify_dispatch_and_mute() {
    okapi_store::test_support::assert_isolated();
    let (sink, hits, bodies) = spawn_sink().await;
    let (notifier, _redis, tmp) = temp_db_notifier(&sink).await;
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
    .execute(&tmp.pool)
    .await
    .unwrap();

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
    tmp.teardown().await;
}

/// 失败不能启动静默窗；签名必须覆盖实际收到的原始 body，余额默认脱敏。
#[tokio::test]
async fn webhook_failure_retries_and_signs_redacted_payload() {
    use axum::body::Bytes;
    use axum::http::{HeaderMap, StatusCode};
    use hmac::{Hmac, KeyInit, Mac};

    okapi_store::test_support::assert_isolated();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (counter, capture) = (Arc::clone(&hits), Arc::clone(&seen));
    let app = axum::Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: Bytes| {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            capture.lock().unwrap().push((headers, body));
            async move {
                if attempt == 0 {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::OK
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (notifier, _redis, tmp) = temp_db_notifier(&sink).await;
    sqlx::query("UPDATE settings SET value=$1 WHERE key='notify_channels'")
        .bind(
            json!([{"type":"webhook","url":format!("http://{sink}/hook"),
                     "events":["balance_low"],"min_interval_secs":300,"secret":"test-secret"}]),
        )
        .execute(&tmp.pool)
        .await
        .unwrap();
    let payload = json!({"users":[{"user_id":42,"balance_micro":1234}]});
    notifier.dispatch("balance_low", &payload).await;
    notifier.dispatch("balance_low", &payload).await;
    notifier.dispatch("balance_low", &payload).await;
    assert_eq!(hits.load(Ordering::SeqCst), 2, "失败立即重试，成功后静默");
    {
        let records = seen.lock().unwrap();
        for (headers, body) in records.iter() {
            let value: Value = serde_json::from_slice(body).unwrap();
            assert_eq!(value["payload"]["users"], json!([{"user_id":42}]));
            let at = headers["x-okapi-timestamp"].to_str().unwrap();
            assert_eq!(value["at"], at);
            let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"test-secret").unwrap();
            mac.update(at.as_bytes());
            mac.update(b".");
            mac.update(body);
            let signature = headers["x-okapi-signature"]
                .to_str()
                .unwrap()
                .strip_prefix("sha256=")
                .unwrap();
            mac.verify_slice(&hex::decode(signature).unwrap()).unwrap();
        }
    }
    tmp.teardown().await;
    server.abort();
}

/// 余额低扫描：阈值关闭返回空；开启后返回低于阈值的用户。
#[tokio::test]
async fn balance_low_scan_respects_threshold() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 关闭（缺省/0）：空
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'balance_low_threshold_micro'"#)
        .execute(&pg)
        .await
        .unwrap();
    assert!(
        notify::scan_balance_low(&pg).await.unwrap().is_empty(),
        "notify::scan_balance_low(&pg).await.unwrap() should be empty"
    );

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

#[tokio::test]
async fn rejected_private_webhook_is_visible_in_audit() {
    okapi_store::test_support::assert_isolated();
    let (sink, hits, _) = spawn_sink().await;
    let (notifier, redis, bed) = temp_db_notifier(&sink).await;
    sqlx::query("UPDATE settings SET value='{}'::jsonb WHERE key='ssrf_policy'")
        .execute(&bed.pool)
        .await
        .unwrap();
    let _: i64 = fred::interfaces::KeysInterface::del(&redis, "notify:mute:0:ssrf_rejected_audit")
        .await
        .unwrap();
    notifier.dispatch("drift", &json!({"count":1})).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let detail: Value = sqlx::query_scalar(
        "SELECT detail FROM audit_logs WHERE action='notification.url_rejected'",
    )
    .fetch_one(&bed.pool)
    .await
    .unwrap();
    assert_eq!(detail["channel_index"], 0);
    assert_eq!(detail["error_code"], "bad_request");
    assert!(
        detail.get("url").is_none(),
        "do not retain tokens in webhook URLs"
    );
    bed.teardown().await;
}

/// 人工队列巡检：统计死信与结算隔离区都不会自己消失，非零就各派发一次、载荷带上数量；
/// 清空后再扫一条都不发（没事不吵）。
#[tokio::test]
async fn manual_queues_wake_operators_only_when_non_empty() {
    use fred::interfaces::{HashesInterface, KeysInterface};
    okapi_store::test_support::assert_isolated();
    let (sink, _hits, bodies) = spawn_sink().await;
    let (notifier, redis, tmp) = temp_db_notifier(&sink).await;
    sqlx::query("UPDATE settings SET value = $1 WHERE key = 'notify_channels'")
        .bind(
            json!([{ "type": "webhook", "url": format!("http://{sink}/hook"),
                       "events": ["dlq_backlog", "settlement_quarantine"],
                       "min_interval_secs": 1 }]),
        )
        .execute(&tmp.pool)
        .await
        .unwrap();
    let quarantine = "settlement:{retry}:quarantine";
    for key in [
        "notify:mute:0:dlq_backlog",
        "notify:mute:0:settlement_quarantine",
        quarantine,
    ] {
        let _: i64 = redis.del(key).await.unwrap();
    }
    let take = || std::mem::take(&mut *bodies.lock().unwrap());

    // 两条队列都空：不发
    assert_eq!(
        notify::manual_queues_and_notify(&tmp.pool, &redis, &notifier)
            .await
            .unwrap(),
        (0, 0)
    );
    assert!(take().is_empty(), "take() should be empty");

    // 死信两条（一条已处理不算）+ 隔离一条：各派发一次，数量对得上
    for status in [0_i16, 0, 2] {
        sqlx::query("INSERT INTO billing_dlq (source, payload, status) VALUES ('ch_batch', '{}'::jsonb, $1)")
            .bind(status)
            .execute(&tmp.pool)
            .await
            .unwrap();
    }
    let _: i64 = redis
        .hset(quarantine, ("00000000-0000-0000-0000-000000000001", "{}"))
        .await
        .unwrap();
    assert_eq!(
        notify::manual_queues_and_notify(&tmp.pool, &redis, &notifier)
            .await
            .unwrap(),
        (2, 1)
    );
    let sent = take();
    assert_eq!(sent.len(), 2, "{sent:?}");
    let event = |name: &str| {
        sent.iter()
            .find(|b| b["event"] == name)
            .unwrap_or_else(|| panic!("missing {name}: {sent:?}"))
    };
    assert_eq!(event("dlq_backlog")["payload"]["pending"], 2);
    assert_eq!(event("settlement_quarantine")["payload"]["quarantined"], 1);

    // 处理完：再扫不发
    sqlx::query("UPDATE billing_dlq SET status = 2")
        .execute(&tmp.pool)
        .await
        .unwrap();
    let _: i64 = redis.del(quarantine).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        notify::manual_queues_and_notify(&tmp.pool, &redis, &notifier)
            .await
            .unwrap(),
        (0, 0)
    );
    assert!(take().is_empty(), "take() should be empty");
    tmp.teardown().await;
}
