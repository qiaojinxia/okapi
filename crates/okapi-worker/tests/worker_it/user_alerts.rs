//! 用户级告警评估与投递验收：低余额 + 即将到期密钥 Webhook（含 HMAC 签名与 Redis 静默窗）。
use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::routing::post;
use fred::interfaces::KeysInterface;
use hmac::{Hmac, KeyInit, Mac};
use okapi_worker::user_alerts;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
struct Captured {
    signature: Option<String>,
    timestamp: Option<String>,
    raw_body: String,
    json_body: Value,
}

async fn spawn_sink() -> (SocketAddr, Arc<Mutex<Vec<Captured>>>) {
    let captured: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let cap_clone = Arc::clone(&captured);
    let app = axum::Router::new().route(
        "/user-hook",
        post(move |headers: HeaderMap, body: Bytes| {
            let cap = Arc::clone(&cap_clone);
            async move {
                let raw_body = String::from_utf8_lossy(&body).into_owned();
                let json_body: Value = serde_json::from_str(&raw_body).unwrap_or(Value::Null);
                cap.lock().unwrap().push(Captured {
                    signature: headers
                        .get("x-okapi-signature")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned),
                    timestamp: headers
                        .get("x-okapi-timestamp")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned),
                    raw_body,
                    json_body,
                });
                axum::Json(json!({ "ok": true }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (sink, captured)
}

#[tokio::test]
async fn user_alerts_evaluate_low_balance_and_expiring_keys_with_mute_and_hmac() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let (sink, captured) = spawn_sink().await;

    let admin: PgPool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!("okapi_ua_{}", &Uuid::new_v4().simple().to_string()[..12]);
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin)
    .await
    .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let pg = okapi_store::connect_pg(&format!("{base}/{db_name}"))
        .await
        .unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('ssrf_policy', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(json!({ "allow_http": true, "allow_private": true }))
    .execute(&pg)
    .await
    .unwrap();

    let uid = okapi_store::provision::create_user(&pg, "alert-user")
        .await
        .unwrap();
    sqlx::query("UPDATE users SET balance_micro = 500000 WHERE id = $1")
        .bind(uid)
        .execute(&pg)
        .await
        .unwrap();
    let kid = okapi_store::provision::create_api_key(&pg, uid, "hash-expiring-key-01", "sk-exp")
        .await
        .unwrap();
    sqlx::query("UPDATE api_keys SET expires_at = now() + interval '2 days' WHERE id = $1")
        .bind(kid)
        .execute(&pg)
        .await
        .unwrap();
    let secret = "user-webhook-secret";
    sqlx::query(
        "INSERT INTO user_alert_prefs \
         (user_id, low_balance_threshold_micro, key_expiry_warn_days, email_enabled, webhook_url, webhook_secret, min_interval_secs) \
         VALUES ($1, 1000000, 7, false, $2, $3, 3600)",
    )
    .bind(uid)
    .bind(format!("http://{sink}/user-hook"))
    .bind(secret)
    .execute(&pg)
    .await
    .unwrap();

    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let _: Result<i64, _> = redis
        .del(vec![
            format!("notify:user:{uid}:user_balance_low"),
            format!("notify:user:{uid}:user_key_expiring"),
        ])
        .await;

    let first = user_alerts::evaluate_and_notify(&pg, &redis).await.unwrap();
    assert_eq!(first.low_balance_sent, 1);
    assert_eq!(first.key_expiry_sent, 1);

    let second = user_alerts::evaluate_and_notify(&pg, &redis).await.unwrap();
    assert_eq!(second.low_balance_sent, 0);
    assert_eq!(second.key_expiry_sent, 0);

    let received = captured.lock().unwrap().clone();
    assert_eq!(received.len(), 2);
    for item in &received {
        let ts = item.timestamp.as_deref().expect("timestamp header");
        let sig = item.signature.as_deref().expect("signature header");
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(ts.as_bytes());
        mac.update(b".");
        mac.update(item.raw_body.as_bytes());
        assert_eq!(
            sig,
            format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
        );
    }
    let events: Vec<&str> = received
        .iter()
        .filter_map(|i| i.json_body["event"].as_str())
        .collect();
    assert!(events.contains(&"user_balance_low"), "{events:?}");
    assert!(events.contains(&"user_key_expiring"), "{events:?}");

    pg.close().await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"DROP DATABASE IF EXISTS "{db_name}" WITH (FORCE)"#
    )))
    .execute(&admin)
    .await;
}
