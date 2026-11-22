//! 后台「统计时区」（settings.reporting_timezone）：写入校验，以及不重启就切换 PG 会话时区与
//! ClickHouse 查询时区、切回跟随服务器。
//!
//! 统计时区是进程级全局状态，所以单独一个测试二进制、一个测试函数按顺序走完，不和别的用例并行。

use okapi_console as console;
use okapi_gateway as gateway;
use serde_json::{Value, json};
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(app: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

async fn put_zone(addr: SocketAddr, token: &str, value: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/admin/settings"))
        .bearer_auth(token)
        .json(&json!({ "key": "reporting_timezone", "value": value }))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn read_zone(addr: SocketAddr, token: &str) -> Value {
    reqwest::Client::new()
        .get(format!("http://{addr}/admin/settings/reporting_timezone"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn pg_zone(pg: &sqlx::PgPool) -> String {
    sqlx::query_scalar("SELECT current_setting('TimeZone')")
        .fetch_one(pg)
        .await
        .unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn reporting_timezone_switches_sessions_without_restart() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let ch_url = okapi_store::test_support::optional_service("OKAPI_CLICKHOUSE_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("tz-a-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = 100 WHERE id = $1")
        .bind(admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let token = format!("sk-okapi-tz-a-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&token), "sk-tz-a")
        .await
        .unwrap();
    let state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    if let Some(ch) = &state.ch {
        ch.ensure_schema().await.unwrap();
    }
    let ch = state.ch.clone();
    let app_pg = state.pg.clone();
    let console = serve(console::router(state)).await;

    let host = okapi_store::timezone::machine_timezone()
        .unwrap()
        .to_owned();
    let target = if host == "Asia/Shanghai" {
        "America/Los_Angeles"
    } else {
        "Asia/Shanghai"
    };
    // Connections opened before the switch: they must not keep the old session zone afterwards.
    assert_eq!(pg_zone(&pg).await, host);
    assert_eq!(pg_zone(&app_pg).await, host);

    // Rejected before anything is stored: unknown names, non-canonical spellings, quotes (the name is
    // spliced into ClickHouse SQL), and zones whose offset is not a whole hour.
    for (bad, param) in [
        (json!("Mars/Phobos"), "reporting_timezone"),
        (json!("Asia/Shanghai'"), "reporting_timezone"),
        (json!("/etc/localtime"), "reporting_timezone"),
        (json!("posix/Asia/Shanghai"), "reporting_timezone"),
        (json!(8), "reporting_timezone"),
        (json!("Asia/Kolkata"), "reporting_timezone_whole_hours"),
        (
            json!("Australia/Lord_Howe"),
            "reporting_timezone_whole_hours",
        ),
    ] {
        let (status, body) = put_zone(console, &token, bad.clone()).await;
        assert_eq!(status, 400, "{bad}: {body}");
        assert_eq!(body["error"]["param"], param, "{bad}: {body}");
    }
    let stored: Option<Value> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'reporting_timezone'")
            .fetch_optional(&pg)
            .await
            .unwrap();
    assert!(
        stored.is_none(),
        "rejected values are never stored: {stored:?}"
    );
    let read = read_zone(console, &token).await;
    assert_eq!(read["value"], Value::Null);
    assert_eq!(read["host_timezone"], host.as_str());
    assert_eq!(read["effective_timezone"], host.as_str());

    // Switch: the process applies it at once, both pools hand out connections in the new zone.
    let (status, body) = put_zone(console, &token, json!(target)).await;
    assert_eq!(status, 200, "{body}");
    let read = read_zone(console, &token).await;
    assert_eq!(read["value"], target);
    assert_eq!(read["effective_timezone"], target);
    assert_eq!(read["host_timezone"], host.as_str());
    assert_eq!(okapi_store::timezone::reporting_timezone().unwrap(), target);
    for _ in 0..3 {
        assert_eq!(pg_zone(&pg).await, target);
        assert_eq!(pg_zone(&app_pg).await, target);
    }
    if let Some(ch) = &ch {
        let rows = ch
            .query_json_each_row("SELECT timezone() AS tz, toString(today()) AS today")
            .await
            .unwrap();
        assert_eq!(rows[0]["tz"], target);
        let local_today: String = sqlx::query_scalar("SELECT current_date::text")
            .fetch_one(&pg)
            .await
            .unwrap();
        assert_eq!(
            rows[0]["today"],
            local_today.as_str(),
            "PG and ClickHouse agree on today"
        );
    }

    // Back to following the host (null), again without a restart.
    let (status, body) = put_zone(console, &token, Value::Null).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(okapi_store::timezone::reporting_timezone().unwrap(), host);
    assert_eq!(pg_zone(&pg).await, host);
    assert_eq!(pg_zone(&app_pg).await, host);
    if let Some(ch) = &ch {
        let rows = ch
            .query_json_each_row("SELECT timezone() AS tz")
            .await
            .unwrap();
        assert_eq!(rows[0]["tz"], host.as_str());
    }
    let read = read_zone(console, &token).await;
    assert_eq!(read["effective_timezone"], host.as_str());
}
