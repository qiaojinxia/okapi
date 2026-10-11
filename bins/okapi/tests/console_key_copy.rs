//! Owner-session key copying; run against an isolated test database and Redis DB.
use okapi::{console, gateway};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

struct Env {
    state: gateway::state::AppState,
    base: String,
    user: i64,
    auth_key: String,
    cookie: String,
    sid: String,
    legacy_id: i64,
}

async fn serve(state: gateway::state::AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, console::router(state)).await.unwrap();
    });
    format!("http://{addr}")
}

async fn setup(master: bool) -> Env {
    okapi_store::test_support::assert_isolated();
    let mut state = gateway::build_state(
        &std::env::var("DATABASE_URL").unwrap(),
        &std::env::var("OKAPI_REDIS_URL").unwrap(),
        "key-copy-test",
        None,
        None,
    )
    .await
    .unwrap();
    state.master_key = master.then(|| std::sync::Arc::from(hex::encode([11u8; 32]).as_str()));
    let user = okapi_store::provision::create_user(&state.pg, &format!("copy-{}", Uuid::new_v4()))
        .await
        .unwrap();
    let auth_key = format!("sk-okapi-fixture-{}", Uuid::new_v4());
    let hash = hex::encode(Sha256::digest(auth_key.as_bytes()));
    let legacy_id =
        okapi_store::provision::create_api_key(&state.pg, user, &hash, "sk-copy-fixture")
            .await
            .unwrap();
    let sid = okapi::gateway::sched_redis::SchedulerRedis::web_session_sid(
        user,
        &Uuid::new_v4().simple().to_string(),
    );
    state.sched.web_session_set(&sid, user, None, None).await;
    let base = serve(state.clone()).await;
    Env {
        state,
        base,
        user,
        auth_key,
        cookie: format!("okapi_session={sid}"),
        sid,
        legacy_id,
    }
}

async fn create(env: &Env) -> Value {
    reqwest::Client::new()
        .post(format!("{}/auth/keys", env.base))
        .header("cookie", &env.cookie)
        .json(&json!({"name":"copy-test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn copy(env: &Env, id: i64) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .post(format!("{}/auth/keys/{id}/copy", env.base))
        .header("cookie", &env.cookie)
        .bearer_auth(&env.auth_key)
        .json(&json!({}))
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn encrypted_key_copy_requires_matching_owner_session_and_hides_secrets_in_lists() {
    let env = setup(true).await;
    let created = create(&env).await;
    let id = created["key_id"].as_i64().unwrap();
    let token = created["api_key"].as_str().unwrap();
    assert_eq!(created["copy_available"], true);
    let (hash, stored): (String, Vec<u8>) =
        sqlx::query_as("SELECT key_hash,key_ciphertext FROM api_keys WHERE id=$1")
            .bind(id)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert_eq!(hash, hex::encode(Sha256::digest(token.as_bytes())));
    assert!(stored.starts_with(b"okk1"));
    assert!(!stored.windows(token.len()).any(|w| w == token.as_bytes()));
    for _ in 0..2 {
        let response = copy(&env, id).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store, private");
        assert_eq!(response.json::<Value>().await.unwrap()["api_key"], token);
    }
    let list: Value = reqwest::Client::new()
        .get(format!("{}/api/me/keys", env.base))
        .bearer_auth(&env.auth_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for row in list["data"].as_array().unwrap() {
        assert!(row.get("api_key").is_none());
        assert!(row.get("key_ciphertext").is_none());
        assert!(row.get("key_hash").is_none());
        assert_eq!(
            row["copy_status"],
            if row["id"] == id {
                "available"
            } else {
                "not_saved"
            }
        );
    }
    let client = reqwest::Client::new();
    let url = format!("{}/auth/keys/{id}/copy", env.base);
    for request in [
        client.post(&url).json(&json!({})),
        client
            .post(&url)
            .bearer_auth(&env.auth_key)
            .json(&json!({})),
    ] {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["cache-control"], "no-store, private");
    }
    assert_eq!(client.get(&url).send().await.unwrap().status(), 405);
    assert_eq!(
        client
            .post(&url)
            .header("cookie", &env.cookie)
            .bearer_auth(&env.auth_key)
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        415
    );
    let other = setup(true).await;
    assert_eq!(copy(&other, id).send().await.unwrap().status(), 404);
    assert_eq!(
        client
            .post(&url)
            .header("cookie", &other.cookie)
            .bearer_auth(&env.auth_key)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let legacy = copy(&env, env.legacy_id).send().await.unwrap();
    assert_eq!(legacy.status(), 409);
    assert_eq!(
        legacy.json::<Value>().await.unwrap()["error"]["code"],
        "key_copy_not_saved"
    );

    // Disabling a key does not lose its copy; soft deletion immediately prevents retrieval.
    sqlx::query("UPDATE api_keys SET status=2, expires_at=now()-interval '1 day' WHERE id=$1")
        .bind(id)
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(copy(&env, id).send().await.unwrap().status(), 200);
    sqlx::query("UPDATE api_keys SET deleted_at=now() WHERE id=$1")
        .bind(id)
        .execute(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(copy(&env, id).send().await.unwrap().status(), 404);
    env.state.sched.web_session_revoke(env.user, &env.sid).await;
    assert_eq!(
        copy(&env, env.legacy_id).send().await.unwrap().status(),
        401
    );
}

#[tokio::test]
async fn key_copy_missing_master_never_stores_plaintext_and_bad_envelopes_fail_closed() {
    let env = setup(false).await;
    let created = create(&env).await;
    let id = created["key_id"].as_i64().unwrap();
    assert_eq!(created["copy_available"], false);
    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT key_ciphertext FROM api_keys WHERE id=$1")
            .bind(id)
            .fetch_one(&env.state.pg)
            .await
            .unwrap();
    assert!(stored.is_none());
    assert_eq!(copy(&env, id).send().await.unwrap().status(), 409);

    let encrypted = setup(true).await;
    let created = create(&encrypted).await;
    let id = created["key_id"].as_i64().unwrap();
    for master in [
        None,
        Some(std::sync::Arc::from(hex::encode([12u8; 32]).as_str())),
    ] {
        let mut state = encrypted.state.clone();
        state.master_key = master;
        let base = serve(state).await;
        let response = reqwest::Client::new()
            .post(format!("{base}/auth/keys/{id}/copy"))
            .header("cookie", &encrypted.cookie)
            .bearer_auth(&encrypted.auth_key)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["code"], "key_copy_unavailable");
        assert!(error.get("api_key").is_none());
    }
    sqlx::query("UPDATE api_keys SET key_ciphertext=$2 WHERE id=$1")
        .bind(id)
        .bind(b"plaintext-is-forbidden".as_slice())
        .execute(&encrypted.state.pg)
        .await
        .unwrap();
    assert_eq!(copy(&encrypted, id).send().await.unwrap().status(), 503);
}
