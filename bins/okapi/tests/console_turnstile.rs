//! Turnstile 注册风控验收（IMPLEMENTATION §6.4）：`settings.turnstile_secret` 配置后，
//! 注册必须带 token 并经 siteverify 放行；缺 token / 校验失败 / 端点不可达三种拒绝各有 param。
//! siteverify 指向本地 mock（`settings.turnstile_verify_url`），并断言送出的表单体形状。
//! 用独立临时库：turnstile_secret 是全局设置，开在共享库会让并行套件的注册全部失败。
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::State;
use axum::routing::post;
use okapi::{console, gateway};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<String>>>);

/// mock siteverify：记录表单体；token 含 "good" 放行，其余拒绝。
async fn siteverify(State(seen): State<Seen>, body: String) -> axum::Json<Value> {
    let ok = body.contains("response=good");
    seen.0.lock().unwrap().push(body);
    axum::Json(
        json!({ "success": ok, "error-codes": if ok { vec![] } else { vec!["invalid-input-response"] } }),
    )
}

struct Bed {
    console: SocketAddr,
    pg: sqlx::PgPool,
    seen: Seen,
    mock: SocketAddr,
}

async fn bed() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    let admin_pool = okapi_store::connect_pg(&database_url).await.unwrap();
    let db_name = format!(
        "okapi_turnstile_{}",
        &Uuid::new_v4().simple().to_string()[..12]
    );
    // 库名为本测试生成的随机标识符（无注入面）
    sqlx::query(sqlx::AssertSqlSafe(format!(
        r#"CREATE DATABASE "{db_name}""#
    )))
    .execute(&admin_pool)
    .await
    .unwrap();
    let base = database_url.rsplit_once('/').map(|(b, _)| b).unwrap();
    let fresh_url = format!("{base}/{db_name}");
    let pg = okapi_store::connect_pg(&fresh_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let seen = Seen::default();
    let router = Router::new()
        .route("/siteverify", post(siteverify))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    for (key, value) in [
        ("turnstile_secret", json!("sec-123")),
        (
            "turnstile_verify_url",
            json!(format!("http://{mock}/siteverify")),
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

    let state = gateway::build_state(&fresh_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Bed {
        console,
        pg,
        seen,
        mock,
    }
}

async fn register(bed: &Bed, token: Option<&str>) -> (u16, Value) {
    let suffix = Uuid::new_v4().simple().to_string();
    let mut body = json!({
        "email": format!("ts-{suffix}@ok.test"),
        "username": format!("ts-{suffix}"),
        "password": "hunter2-strong",
    });
    if let Some(t) = token {
        body["turnstile_token"] = json!(t);
    }
    let resp = reqwest::Client::new()
        .post(format!("http://{}/auth/register", bed.console))
        .header("x-real-ip", format!("203.0.113.{}", rand::random::<u8>()))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn turnstile_gates_registration_and_reports_each_failure_mode() {
    let bed = bed().await;

    let (status, body) = register(&bed, None).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "bad_request");
    assert_eq!(body["error"]["param"], "turnstile_token");
    assert!(bed.seen.0.lock().unwrap().is_empty(), "缺 token 不该外呼");

    let (status, body) = register(&bed, Some("bad-token")).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "turnstile_failed");

    let (status, body) = register(&bed, Some("good-token")).await;
    assert_eq!(status, 200, "{body}");
    let forms = bed.seen.0.lock().unwrap().clone();
    assert_eq!(forms.len(), 2, "两次带 token 的注册各外呼一次");
    assert_eq!(forms[0], "secret=sec-123&response=bad-token");
    assert_eq!(forms[1], "secret=sec-123&response=good-token");

    // 端点不可达（指向已关闭的端口）→ 明确的 unreachable，而不是把注册放过去
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    sqlx::query!(
        r#"UPDATE settings SET value = $1 WHERE key = 'turnstile_verify_url'"#,
        json!(format!("http://{dead}/siteverify"))
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    let (status, body) = register(&bed, Some("good-token")).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["param"], "turnstile_unreachable");

    // 秘钥撤掉即整体关闭：不带 token 也能注册，且不再外呼
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'turnstile_secret'"#)
        .execute(&bed.pg)
        .await
        .unwrap();
    let (status, body) = register(&bed, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(bed.seen.0.lock().unwrap().len(), 2);
    let _ = bed.mock;
}
