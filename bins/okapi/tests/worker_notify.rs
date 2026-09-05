//! 通知多路验收（#1790-8，M4）：webhook 分发 / 事件订阅过滤 / 频率闸 / 余额低扫描。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Json;
use axum::routing::post;
use okapi::worker::notify;
use serde_json::{Value, json};
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
