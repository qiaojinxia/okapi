//! Profile writes require a matching web session and API key; only basic fields are mutable.
use okapi::{console, gateway};
use serde_json::{Value, json};
use std::future::IntoFuture;
use uuid::Uuid;

fn hash(value: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[tokio::test]
async fn profile_editing_is_owned_validated_and_persisted() {
    okapi_store::test_support::assert_isolated();
    let source = std::env::var("DATABASE_URL").unwrap();
    let admin = okapi_store::connect_pg(&source).await.unwrap();
    let database = format!("profile_test_{}", Uuid::new_v4().simple());
    // The identifier is a fixed prefix plus a hexadecimal UUID, with no user input.
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    let mut url = reqwest::Url::parse(&source).unwrap();
    url.set_path(&database);
    let pg = okapi_store::connect_pg(url.as_str()).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let state = gateway::build_state(
        url.as_str(),
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "profile-test",
        None,
        None,
    )
    .await
    .unwrap();
    let mut owners = Vec::new();
    for name in ["alice", "bob"] {
        let id = okapi_store::provision::create_user(&pg, name)
            .await
            .unwrap();
        let token = format!("sk-profile-{}", Uuid::new_v4().simple());
        okapi_store::provision::create_api_key(&pg, id, &hash(&token), "sk-profile")
            .await
            .unwrap();
        let sid = Uuid::new_v4().to_string();
        state.sched.web_session_set(&sid, id, None, None).await;
        owners.push((id, token, format!("okapi_session={sid}")));
    }
    // Clean up isolated data even when an HTTP assertion fails.
    let result = tokio::spawn(verify_profile(state.clone(), pg.clone(), owners.clone())).await;
    for (_, token, cookie) in &owners {
        state.sched.auth_del(&hash(token)).await;
        state
            .sched
            .web_session_del(cookie.strip_prefix("okapi_session=").unwrap())
            .await;
    }
    state.pg.close().await;
    pg.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    result.unwrap();
}

async fn verify_profile(
    state: gateway::state::AppState,
    pg: sqlx::PgPool,
    owners: Vec<(i64, String, String)>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(axum::serve(listener, console::router(state.clone())).into_future());
    let endpoint = format!("http://{address}/api/me/profile");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let (_, token, cookie) = &owners[0];
    let identity_endpoint = format!("http://{address}/api/me");
    assert_identity(&client, &identity_endpoint, token, owners[0].0, "alice").await;
    let body = json!({"username":" 新名字 ","language":"en"});
    let denied = client
        .patch(&endpoint)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401, "{denied:?}");
    let mismatch = client
        .patch(&endpoint)
        .bearer_auth(token)
        .header("cookie", &owners[1].2)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(mismatch.status(), 401);
    for invalid in [
        json!({"username":" ","language":"en"}),
        json!({"username":"alice","language":"bad"}),
        json!({"username":"alice","language":"en","role":100}),
    ] {
        let response = client
            .patch(&endpoint)
            .bearer_auth(token)
            .header("cookie", cookie)
            .json(&invalid)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let conflict = client
        .patch(&endpoint)
        .bearer_auth(token)
        .header("cookie", cookie)
        .json(&json!({"username":"bob","language":"en"}))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), 409);
    assert_eq!(
        conflict.json::<Value>().await.unwrap()["error"]["code"],
        "profile_username_taken"
    );
    let saved = client
        .patch(&endpoint)
        .bearer_auth(token)
        .header("cookie", cookie)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    assert_eq!(saved.json::<Value>().await.unwrap()["username"], "新名字");
    let read = client
        .get(&endpoint)
        .bearer_auth(token)
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(read["username"], "新名字");
    assert_eq!(read["language"], "en");
    assert_identity(&client, &identity_endpoint, token, owners[0].0, "新名字").await;
    let bob: (String, i16, i64) =
        sqlx::query_as("SELECT username,role,balance_micro FROM users WHERE id=$1")
            .bind(owners[1].0)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(bob, ("bob".into(), 1, 0));
    server.abort();
    let _ = server.await;
}

async fn assert_identity(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    user_id: i64,
    username: &str,
) {
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let identity = response.json::<Value>().await.unwrap();
    assert_eq!(identity["username"], username);
    assert_eq!(identity["user_id"], user_id);
}
