use super::*;
use okapi::gateway::credentials::health;
use okapi::worker::oauth_refresh::{RefreshPolicy, refresh_once};
use std::time::Duration;

async fn soon(env: &Env, key: i64) {
    let mut credential = read_cred(env, key).await;
    credential.expires_at = chrono::Utc::now().timestamp() + 240;
    okapi_store::admin::write_key_credential(
        &env.pg,
        key,
        &credential.to_plaintext(),
        env.state.master_key.as_deref(),
    )
    .await
    .unwrap();
    env.state.invalidate_routing_caches();
}

async fn reauthorize(env: &Env, channel: i64, key: i64, provider: &str) -> reqwest::Response {
    let client = reqwest::Client::new();
    let issued: Value = client
        .post(format!("http://{}/admin/channels/oauth/start", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"provider":provider,"channel_id":channel,"channel_key_id":key}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    client.post(format!("http://{}/admin/channels/oauth/exchange", env.console))
        .bearer_auth(&env.admin_token)
        .json(&json!({"state":issued["state"],"code":"reauth-code","channel_id":channel,"channel_key_id":key}))
        .send().await.unwrap()
}

#[tokio::test]
async fn background_refresh_is_optional_early_and_preserves_key_cooldown() {
    let env = setup().await;
    let (_, key) = login_channel(&env, "anthropic_max").await;
    soon(&env, key).await;
    okapi_store::channels::mark_key_failure(
        &env.pg,
        key,
        "upstream_rate_limited",
        okapi_store::channels::KeyFailure::RateLimited {
            retry_after_secs: Some(60),
        },
    )
    .await
    .unwrap();
    let mut after = key - 1;
    refresh_once(
        &env.state,
        RefreshPolicy {
            enabled: false,
            ..RefreshPolicy::default()
        },
        &mut after,
    )
    .await
    .unwrap();
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 1);
    refresh_once(&env.state, RefreshPolicy::default(), &mut after)
        .await
        .unwrap();
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 2);
    assert_eq!(read_cred(&env, key).await.access_token, "access-2");
    let status: i16 = sqlx::query_scalar("SELECT status FROM channel_keys WHERE id=$1")
        .bind(key)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 3, "refresh must not clear scheduling cooldown");
    let observed = health::read(&env.state, key).await.unwrap();
    assert!(observed.last_success_at.is_some());
    assert!(observed.error_code.is_none());
    assert_eq!(
        env.state
            .ledger
            .balance(env.user_id)
            .await
            .unwrap()
            .as_micros(),
        10_000_000
    );
}

#[tokio::test]
async fn temporary_failures_back_off_and_manual_refresh_recovers_without_secret_output() {
    let env = setup().await;
    let (channel, key) = login_channel(&env, "anthropic_max").await;
    soon(&env, key).await;
    env.mock_state
        .refresh_unavailable
        .store(true, Ordering::SeqCst);
    let mut after = key - 1;
    refresh_once(&env.state, RefreshPolicy::default(), &mut after)
        .await
        .unwrap();
    let observed = health::read(&env.state, key).await.unwrap();
    assert_eq!(
        observed.error_code.as_deref(),
        Some("oauth_refresh_status_503")
    );
    assert_eq!(observed.consecutive_failures, 1);
    assert!(!observed.retry_ready(chrono::Utc::now().timestamp()));
    after = key - 1;
    refresh_once(&env.state, RefreshPolicy::default(), &mut after)
        .await
        .unwrap();
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 2);
    env.mock_state
        .refresh_unavailable
        .store(false, Ordering::SeqCst);
    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/admin/channels/{channel}/keys/{key}/oauth/refresh",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 3);
    assert_eq!(read_cred(&env, key).await.access_token, "access-3");
    let response = reqwest::Client::new()
        .get(format!("http://{}/admin/channels", env.console))
        .bearer_auth(&env.admin_token)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(response.contains("oauth_refresh"));
    for secret in ["access-3", "refresh-3", "acct-okapi"] {
        assert!(!response.contains(secret));
    }
    assert_eq!(
        health::read(&env.state, key)
            .await
            .unwrap()
            .consecutive_failures,
        0
    );
}

#[tokio::test]
async fn invalid_credentials_require_targeted_reauthorization_and_keep_key_identity() {
    let env = setup().await;
    let (channel, key) = login_channel(&env, "codex").await;
    soon(&env, key).await;
    env.mock_state.reject_refresh.store(true, Ordering::SeqCst);
    let mut after = key - 1;
    refresh_once(&env.state, RefreshPolicy::default(), &mut after)
        .await
        .unwrap();
    let status: i16 = sqlx::query_scalar("SELECT status FROM channel_keys WHERE id=$1")
        .bind(key)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, 6);
    after = key - 1;
    refresh_once(&env.state, RefreshPolicy::default(), &mut after)
        .await
        .unwrap();
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 2);
    env.mock_state.reject_refresh.store(false, Ordering::SeqCst);
    let response = reauthorize(&env, channel, key, "codex").await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["channel_key_id"], key);
    let keys = okapi_store::admin::list_channel_keys_for(&env.pg, &[channel])
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].status, 1);
    assert_eq!(keys[0].failed_count, 0);
    assert_eq!(
        health::read(&env.state, key)
            .await
            .unwrap()
            .consecutive_failures,
        0
    );
}

#[tokio::test]
async fn late_refresh_success_and_invalid_grant_cannot_overwrite_reauthorization() {
    for invalid in [false, true] {
        let env = setup().await;
        let (channel, key) = login_channel(&env, "codex").await;
        soon(&env, key).await;
        env.mock_state.delay_refresh.store(true, Ordering::SeqCst);
        let state = env.state.clone();
        let worker = tokio::spawn(async move {
            let mut after = key - 1;
            refresh_once(&state, RefreshPolicy::default(), &mut after)
                .await
                .unwrap();
        });
        tokio::time::timeout(
            Duration::from_secs(10),
            env.mock_state.refresh_started.notified(),
        )
        .await
        .unwrap();
        let response = reauthorize(&env, channel, key, "codex").await;
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
        let new = read_cred(&env, key).await;
        assert_eq!(new.access_token, "access-3");
        env.mock_state
            .reject_refresh
            .store(invalid, Ordering::SeqCst);
        env.mock_state.refresh_continue.notify_one();
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read_cred(&env, key).await, new);
        let status: i16 = sqlx::query_scalar("SELECT status FROM channel_keys WHERE id=$1")
            .bind(key)
            .fetch_one(&env.pg)
            .await
            .unwrap();
        assert_eq!(status, 1);
        assert!(
            health::read(&env.state, key)
                .await
                .unwrap()
                .error_code
                .is_none()
        );
    }
}

#[tokio::test]
async fn reauthorization_rejects_a_different_account_without_changing_credentials() {
    let env = setup().await;
    let (channel, key) = login_channel(&env, "codex").await;
    let previous = read_cred(&env, key).await;
    *env.mock_state.account_id.lock().unwrap() = "acct-other".into();
    let response = reauthorize(&env, channel, key, "codex").await;
    assert_eq!(response.status(), 409, "{}", response.text().await.unwrap());
    let error: Value = response.json().await.unwrap();
    assert_eq!(error["error"]["code"], "oauth_account_mismatch");
    assert_eq!(read_cred(&env, key).await, previous);
}

#[tokio::test]
async fn delayed_reauthorization_cannot_overwrite_a_newer_authorization() {
    let env = Arc::new(setup().await);
    let (channel, key) = login_channel(&env, "codex").await;
    env.mock_state
        .delay_second_exchange
        .store(true, Ordering::SeqCst);
    let delayed_env = env.clone();
    let delayed =
        tokio::spawn(async move { reauthorize(&delayed_env, channel, key, "codex").await });
    tokio::time::timeout(
        Duration::from_secs(5),
        env.mock_state.refresh_started.notified(),
    )
    .await
    .unwrap();
    let response = reauthorize(&env, channel, key, "codex").await;
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    let new = read_cred(&env, key).await;
    assert_eq!(new.access_token, "access-3");
    env.mock_state.refresh_continue.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), delayed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 409, "{}", response.text().await.unwrap());
    assert_eq!(read_cred(&env, key).await, new);
}

#[tokio::test]
async fn worker_and_request_refresh_the_same_key_once() {
    let env = setup().await;
    let (_, key) = login_channel(&env, "anthropic_max").await;
    expire_cred(&env, key).await;
    let mut after = key - 1;
    let (worker, request) = tokio::join!(
        refresh_once(&env.state, RefreshPolicy::default(), &mut after),
        chat(&env)
    );
    worker.unwrap();
    assert_eq!(request.status(), 200, "{}", request.text().await.unwrap());
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn separate_process_gates_wait_for_a_slow_refresh_instead_of_replaying() {
    let env = setup().await;
    let (_, key) = login_channel(&env, "codex").await;
    expire_cred(&env, key).await;
    env.mock_state.delay_refresh.store(true, Ordering::SeqCst);
    let mut worker_state = env.state.clone();
    worker_state.refresh_gate = Arc::default();
    let worker = tokio::spawn(async move {
        refresh_once(&worker_state, RefreshPolicy::default(), &mut (key - 1))
            .await
            .unwrap();
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        env.mock_state.refresh_started.notified(),
    )
    .await
    .unwrap();
    let request = reqwest::Client::new()
        .post(format!("http://{}/v1/responses", env.gateway))
        .bearer_auth(&env.user_token)
        .json(&json!({"model":env.model,"input":"hi","max_output_tokens":64}))
        .send();
    tokio::pin!(request);
    assert!(
        tokio::time::timeout(Duration::from_millis(700), &mut request)
            .await
            .is_err(),
        "expired credential must wait for the other process's refresh"
    );
    env.mock_state.refresh_continue.notify_one();
    worker.await.unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 2);
    assert_eq!(env.mock_state.codex_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refresh_lease_release_cannot_remove_a_new_owner() {
    use fred::interfaces::KeysInterface;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
        .await
        .unwrap();
    let sched = gateway::sched_redis::SchedulerRedis::new(redis.clone());
    let key = i64::MAX - 100;
    let old = sched.cred_lock_acquire(key).await.unwrap().unwrap();
    let _: i64 = redis.del(format!("lock:cred:{key}")).await.unwrap();
    let new = sched.cred_lock_acquire(key).await.unwrap().unwrap();
    sched.cred_lock_release(key, &old).await;
    let owner: Option<String> = redis.get(format!("lock:cred:{key}")).await.unwrap();
    assert_eq!(owner.as_deref(), Some(new.as_str()));
    sched.cred_lock_release(key, &new).await;
}

#[tokio::test]
async fn bound_oauth_state_cannot_be_swapped_to_another_key_or_administrator() {
    let env = setup().await;
    let (channel, key) = login_channel(&env, "codex").await;
    let (_, other_key) = login_channel(&env, "codex").await;
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", env.user_id)
        .execute(&env.pg)
        .await
        .unwrap();
    let client = reqwest::Client::new();
    for (token, requested_key) in [(&env.admin_token, other_key), (&env.user_token, key)] {
        let issued: Value = client
            .post(format!("http://{}/admin/channels/oauth/start", env.console))
            .bearer_auth(&env.admin_token)
            .json(&json!({"provider":"codex","channel_id":channel,"channel_key_id":key}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let response = client.post(format!("http://{}/admin/channels/oauth/exchange", env.console))
            .bearer_auth(token)
            .json(&json!({"state":issued["state"],"code":"code","channel_id":channel,"channel_key_id":requested_key}))
            .send().await.unwrap();
        assert_eq!(response.status(), 400, "{}", response.text().await.unwrap());
        assert_eq!(
            env.mock_state.token_calls.load(Ordering::SeqCst),
            2,
            "a rejected state must never reach the token endpoint"
        );
    }
}

#[tokio::test]
async fn scan_cursor_reaches_due_keys_beyond_a_fresh_first_page() {
    let env = setup().await;
    let (_, first) = login_channel(&env, "codex").await;
    let (_, second) = login_channel(&env, "codex").await;
    let (disabled, third) = login_channel(&env, "codex").await;
    soon(&env, second).await;
    soon(&env, third).await;
    sqlx::query("UPDATE channels SET status=2 WHERE id=$1")
        .bind(disabled)
        .execute(&env.pg)
        .await
        .unwrap();
    let policy = RefreshPolicy {
        batch_size: 1,
        ..RefreshPolicy::default()
    };
    let mut after = first - 1;
    refresh_once(&env.state, policy, &mut after).await.unwrap();
    assert_eq!(after, first);
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 3);
    refresh_once(&env.state, policy, &mut after).await.unwrap();
    assert_eq!(after, second);
    assert_eq!(read_cred(&env, second).await.access_token, "access-4");
    refresh_once(&env.state, policy, &mut after).await.unwrap();
    assert_eq!(after, 0);
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 4);
    assert_eq!(read_cred(&env, third).await.access_token, "access-3");
}
