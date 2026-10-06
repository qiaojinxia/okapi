//! Deterministic security/concurrency regressions from the architecture audit.
//! The test runner provisions disposable stores; never load the developer .env.
use axum::http::HeaderMap;
use fred::interfaces::{KeysInterface, SetsInterface};
use okapi::{
    console,
    gateway::{self, state::AppState},
};
use okapi_domain::Money;
use okapi_ledger::{BalanceLedger, holds::UserGuard};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
async fn state() -> Result<AppState> {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL")?;
    let pg = okapi_store::connect_pg(&database).await?;
    okapi_store::run_migrations(&pg).await?;
    drop(pg);
    Ok(gateway::build_state(
        &database,
        &std::env::var("OKAPI_REDIS_URL")?,
        "audit-regressions",
        None,
        None,
    )
    .await?)
}
async fn user(pg: &PgPool) -> Result<i64> {
    Ok(
        okapi_store::provision::create_user(pg, &format!("audit-{}", Uuid::new_v4().simple()))
            .await?,
    )
}
async fn token(pg: &PgPool, uid: i64) -> Result<(i64, String, String)> {
    let token = format!("sk-okapi-audit-{}", Uuid::new_v4().simple());
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let id = okapi_store::provision::create_api_key(pg, uid, &hash, "sk-okapi-audit").await?;
    Ok((id, token, hash))
}
async fn serve(state: AppState) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let app = console::router(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Ok((url, task))
}
#[tokio::test]
async fn sessions_reject_orphans_slide_all_ttls_and_revoke_atomically() -> Result {
    let state = state().await?;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?;
    let uid = user(&state.pg).await?;
    let sid = Uuid::new_v4().to_string();
    let index = format!("sess:idx:{uid}");
    let _: () = redis.set(&index, "wrong-type", None, None, false).await?;
    state.sched.web_session_set(&sid, uid, None, None).await;
    assert_eq!(state.sched.web_session_get(&sid).await, None);
    let exists: bool = redis.exists(format!("sess:web:{sid}")).await?;
    assert!(
        !exists,
        "index write failure must not leave an authorized orphan"
    );
    let _: i64 = redis.del(&index).await?;
    state
        .sched
        .web_session_set(&sid, uid, Some("192.0.2.1"), None)
        .await;
    for key in [
        &index,
        &format!("sess:web:{sid}"),
        &format!("sess:meta:{sid}"),
    ] {
        let _: bool = redis.expire(key, 2, None).await?;
    }
    assert_eq!(state.sched.web_session_get(&sid).await, Some(uid));
    for key in [
        &index,
        &format!("sess:web:{sid}"),
        &format!("sess:meta:{sid}"),
    ] {
        let ttl: i64 = redis.ttl(key).await?;
        assert!(ttl > 600_000, "{key}: {ttl}");
    }
    state.sched.web_session_revoke_user(uid).await;
    assert_eq!(state.sched.web_session_get(&sid).await, None);
    let indexed: bool = redis.sismember(index, &sid).await?;
    assert!(!indexed);
    Ok(())
}
#[tokio::test]
async fn late_auth_snapshot_cannot_restore_revoked_key() -> Result {
    let state = state().await?;
    let uid = user(&state.pg).await?;
    let (id, _, hash) = token(&state.pg, uid).await?;
    let version = state.sched.auth_version().await.unwrap_or(0);
    let snapshot = okapi_store::auth::find_key_by_hash(&state.pg, &hash)
        .await?
        .unwrap();
    sqlx::query("UPDATE api_keys SET status=2 WHERE id=$1")
        .bind(id)
        .execute(&state.pg)
        .await?;
    state.sched.auth_flush().await;
    state.sched.auth_set(&hash, &snapshot, version).await;
    assert!(state.sched.auth_get(&hash).await.is_none());
    Ok(())
}
#[tokio::test]
async fn database_failure_does_not_cache_an_open_registration_policy() -> Result {
    let mut state = state().await?;
    sqlx::query("INSERT INTO settings(key,value) VALUES('registration_policy',$1) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").bind(json!({"mode":"closed"})).execute(&state.pg).await?;
    state.settings_cache.invalidate_all();
    state.pg.close().await;
    assert!(
        console::registration::RegistrationPolicy::load(&state)
            .await
            .is_err()
    );
    state.pg = okapi_store::connect_pg(&std::env::var("DATABASE_URL")?).await?;
    assert_eq!(
        console::registration::RegistrationPolicy::load(&state)
            .await
            .unwrap()
            .mode,
        console::registration::RegisterMode::Closed
    );
    sqlx::query("DELETE FROM settings WHERE key='registration_policy'")
        .execute(&state.pg)
        .await?;
    Ok(())
}
#[tokio::test]
async fn concurrent_oauth_first_login_creates_no_orphan_users() -> Result {
    let state = state().await?;
    let subject = Uuid::new_v4().to_string();
    let display = format!("fixture-{}", &subject[..8]);
    let provider = format!("oauth-{}", &subject[..8]);
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (pg, subject, display, provider, barrier) = (
            state.pg.clone(),
            subject.clone(),
            display.clone(),
            provider.clone(),
            barrier.clone(),
        );
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            okapi_store::identity::link_oauth_user(&pg, &provider, &subject, &display).await
        }));
    }
    let mut ids = Vec::new();
    for task in tasks {
        ids.push(task.await??);
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 1);
    let links: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oauth_identities WHERE provider=$1 AND subject=$2",
    )
    .bind(&provider)
    .bind(subject)
    .fetch_one(&state.pg)
    .await?;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username LIKE $1")
        .bind(format!("{provider}-{display}%"))
        .fetch_one(&state.pg)
        .await?;
    assert_eq!((users, links), (1, 1));
    Ok(())
}
#[tokio::test]
async fn duplicate_preserves_oauth_kind_and_pool_overrides() -> Result {
    let state = state().await?;
    let channel:i64=sqlx::query_scalar("INSERT INTO channels(name,provider,api_base,models) VALUES($1,'anthropic_max','https://example.test','[]') RETURNING id").bind(Uuid::new_v4().to_string()).fetch_one(&state.pg).await?;
    sqlx::query("INSERT INTO channel_keys(channel_id,credential_ciphertext,credential_kind) VALUES($1,$2,1)").bind(channel).bind(b"synthetic-oauth-credential".as_slice()).execute(&state.pg).await?;
    sqlx::query("INSERT INTO pool_channels(channel_id,pool_code,priority_override,weight_override) VALUES($1,'default',7,11)").bind(channel).execute(&state.pg).await?;
    let copy =
        okapi_store::mutate::duplicate_channel(&state.pg, channel, &Uuid::new_v4().to_string())
            .await?
            .unwrap();
    let kind: i16 =
        sqlx::query_scalar("SELECT credential_kind FROM channel_keys WHERE channel_id=$1")
            .bind(copy)
            .fetch_one(&state.pg)
            .await?;
    let overrides: (Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT priority_override,weight_override FROM pool_channels WHERE channel_id=$1",
    )
    .bind(copy)
    .fetch_one(&state.pg)
    .await?;
    assert_eq!(kind, 1);
    assert_eq!(overrides, (Some(7), Some(11)));
    Ok(())
}
#[tokio::test]
async fn failed_pricing_batch_rolls_back_every_model() -> Result {
    let state = state().await?;
    let uid = user(&state.pg).await?;
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(uid)
        .execute(&state.pg)
        .await?;
    let (_, token, _) = token(&state.pg, uid).await?;
    let tag = Uuid::new_v4().simple().to_string();
    let first = format!("a-{tag}");
    let last = format!("z-{tag}");
    okapi_store::admin::upsert_model_ratio(
        &state.pg,
        &first,
        okapi_store::admin::RatioAxes::basic("1", "1", "1"),
    )
    .await?;
    let (url, server) = serve(state.clone()).await?;
    let response=reqwest::Client::new().post(format!("{url}/admin/pricing/sync/apply")).bearer_auth(token).json(&json!({"changes":[{"model":first,"axis":"completion_ratio","value":"3"},{"model":last,"axis":"cache_ratio","value":"0.1"}]})).send().await?;
    assert_eq!(response.status(), 400, "{}", response.text().await?);
    let ratio:String=sqlx::query_scalar("SELECT p.completion_ratio::text FROM model_pricing p JOIN models m ON m.id=p.model_id WHERE m.model_name=$1").bind(first).fetch_one(&state.pg).await?;
    assert_eq!(ratio, "1.000000");
    server.abort();
    Ok(())
}
#[tokio::test]
async fn team_members_cannot_delete_peer_keys_and_banned_members_cannot_authenticate() -> Result {
    let state = state().await?;
    let member = user(&state.pg).await?;
    let other = user(&state.pg).await?;
    let team: i64 =
        sqlx::query_scalar("INSERT INTO users(username,kind) VALUES($1,'team') RETURNING id")
            .bind(Uuid::new_v4().to_string())
            .fetch_one(&state.pg)
            .await?;
    for uid in [member, other] {
        sqlx::query(
            "INSERT INTO team_members(team_user_id,member_user_id,role) VALUES($1,$2,'member')",
        )
        .bind(team)
        .bind(uid)
        .execute(&state.pg)
        .await?;
    }
    let (member_key, token, _) = token(&state.pg, team).await?;
    sqlx::query("UPDATE api_keys SET member_user_id=$2 WHERE id=$1")
        .bind(member_key)
        .bind(member)
        .execute(&state.pg)
        .await?;
    let (other_key, other_token, _) = self::token(&state.pg, team).await?;
    sqlx::query("UPDATE api_keys SET member_user_id=$2 WHERE id=$1")
        .bind(other_key)
        .bind(other)
        .execute(&state.pg)
        .await?;
    let (url, server) = serve(state.clone()).await?;
    let response = reqwest::Client::new()
        .delete(format!("{url}/api/me/keys/{other_key}"))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(response.status(), 403);
    let deleted: bool =
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM api_keys WHERE id=$1")
            .bind(other_key)
            .fetch_one(&state.pg)
            .await?;
    assert!(!deleted);
    okapi_store::mutate::manage_user(&state.pg, member, okapi_store::mutate::UserAction::Ban)
        .await?;
    state.sched.auth_flush().await;
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse()?);
    assert!(
        gateway::auth::authenticate_data_plane(&state, &headers)
            .await
            .is_err()
    );
    headers.insert("authorization", format!("Bearer {other_token}").parse()?);
    let authed = gateway::auth::authenticate_data_plane(&state, &headers)
        .await
        .unwrap();
    assert_eq!(authed.actor_user_id(), other);
    assert_eq!(authed.user_id, team);
    server.abort();
    Ok(())
}
#[tokio::test]
async fn http_and_mcp_share_protected_user_policy() -> Result {
    let state = state().await?;
    let actor = user(&state.pg).await?;
    let target = user(&state.pg).await?;
    sqlx::query("UPDATE users SET role=CASE WHEN id=$1 THEN 10 ELSE 100 END WHERE id=ANY($2)")
        .bind(actor)
        .bind(vec![actor, target])
        .execute(&state.pg)
        .await?;
    let role:i64=sqlx::query_scalar("INSERT INTO admin_roles(role_code,display_name,permissions) VALUES($1,'Restricted operator',$2) RETURNING id").bind(Uuid::new_v4().to_string()).bind(json!(["user.manage","mcp.write"])).fetch_one(&state.pg).await?;
    sqlx::query("UPDATE users SET admin_role_id=$2 WHERE id=$1")
        .bind(actor)
        .bind(role)
        .execute(&state.pg)
        .await?;
    let (_, token, _) = token(&state.pg, actor).await?;
    sqlx::query("INSERT INTO settings(key,value) VALUES('mcp_write_enabled','true'::jsonb) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").execute(&state.pg).await?;
    let (url, server) = serve(state.clone()).await?;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{url}/admin/users/{target}/manage"))
        .bearer_auth(&token)
        .json(&json!({"action":"ban"}))
        .send()
        .await?;
    assert_eq!(response.status(), 403);
    let response:Value=client.post(format!("{url}/mcp")).bearer_auth(&token).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"user_ban","arguments":{"user_id":target,"confirm":true}}})).send().await?.json().await?;
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "{response}"
    );
    let status: i16 = sqlx::query_scalar("SELECT status FROM users WHERE id=$1")
        .bind(target)
        .fetch_one(&state.pg)
        .await?;
    assert_eq!(status, 1);
    server.abort();
    sqlx::query("DELETE FROM settings WHERE key='mcp_write_enabled'")
        .execute(&state.pg)
        .await?;
    Ok(())
}
#[tokio::test]
async fn user_guard_reuses_unlocked_connection_and_cancellation_releases_lock() -> Result {
    let state = state().await?;
    let uid = user(&state.pg).await?;
    let pg = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;
    let mut guard = UserGuard::acquire(&pg, uid).await?;
    let first: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(guard.connection()?)
        .await?;
    drop(guard);
    let mut guard =
        tokio::time::timeout(Duration::from_secs(5), UserGuard::acquire(&pg, uid)).await??;
    let second: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(guard.connection()?)
        .await?;
    assert_eq!(first, second);
    drop(guard);
    // Block in PostgreSQL's advisory-lock query, not merely waiting for a pool slot.
    let blocker = UserGuard::acquire(&state.pg, uid).await?;
    let other = pg.clone();
    let task = tokio::spawn(async move { UserGuard::acquire(&other, uid).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')",
            ).bind(second).fetch_one(&state.pg).await?;
            if waiting { break Ok::<_, sqlx::Error>(()); }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await??;
    task.abort();
    let _ = task.await;
    drop(blocker);
    let guard =
        tokio::time::timeout(Duration::from_secs(5), UserGuard::acquire(&pg, uid)).await??;
    drop(guard);
    pg.close().await;
    Ok(())
}
#[tokio::test]
async fn locked_user_does_not_starve_other_pending_settlements() -> Result {
    let state = state().await?;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?;
    let ledger = BalanceLedger::new(redis.clone());
    let mut users = Vec::new();
    for _ in 0..2 {
        let uid = user(&state.pg).await?;
        sqlx::query("INSERT INTO billing_sync(request_id,user_id,api_key_id,amount_micro,pool) VALUES($1,$2,1,0,0)").bind(Uuid::new_v4()).bind(uid).execute(&state.pg).await?;
        users.push(uid);
    }
    let guard = UserGuard::acquire(&state.pg, users[0]).await?;
    okapi_ledger::sync::recover_pending(&state.pg, &ledger, 1000).await?;
    let remaining: Vec<i64> = sqlx::query_scalar(
        "SELECT user_id FROM billing_sync WHERE user_id=ANY($1) ORDER BY user_id",
    )
    .bind(&users)
    .fetch_all(&state.pg)
    .await?;
    assert_eq!(remaining, vec![users[0]]);
    drop(guard);
    okapi_ledger::sync::recover_pending(&state.pg, &ledger, 1000).await?;
    Ok(())
}
#[tokio::test]
async fn old_margin_evaluation_cannot_overwrite_manual_lift() -> Result {
    use okapi::margin::{BlockEntry, BlockState};
    okapi_store::test_support::assert_isolated();
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?;
    let now = chrono::Utc::now().timestamp();
    let field = okapi::margin::field(&Uuid::new_v4().to_string(), 999_999);
    let blocked = BlockEntry {
        state: BlockState::Blocked,
        since: now - 300,
        until: now + 3600,
        requests: 25,
        amount_micro: 1000,
        cost_micro: 1_000_000,
        margin_bp: -10000,
    };
    okapi::margin::set_block(&redis, &field, &blocked).await?;
    let lifted = BlockEntry {
        state: BlockState::Lifted,
        since: now,
        until: now + 86400,
        ..blocked.clone()
    };
    okapi::margin::set_block(&redis, &field, &lifted).await?;
    assert!(!okapi::margin::compare_set_block(&redis, &field, Some(&blocked), &blocked).await?);
    okapi::margin::prune_expired(&redis, now + 4000).await?;
    let after = okapi::margin::load_blocks(&redis).await.unwrap();
    assert_eq!(after[&field].state, BlockState::Lifted);
    assert_eq!(after[&field].until, lifted.until);
    Ok(())
}
#[tokio::test]
async fn unlimited_key_admission_waits_for_balance_expiry_fence() -> Result {
    let state = state().await?;
    let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL")?).await?;
    let uid = user(&state.pg).await?;
    let (kid, _, _) = token(&state.pg, uid).await?;
    let ledger = BalanceLedger::new(redis.clone());
    okapi_ledger::operations::credit(
        &state.pg,
        &ledger,
        uid,
        Money::from_micros(10000),
        "adjust",
        "audit",
        json!({}),
    )
    .await?;
    sqlx::query("UPDATE users SET balance_expires_at=now()-interval '1 minute' WHERE id=$1")
        .bind(uid)
        .execute(&state.pg)
        .await?;
    sqlx::raw_sql("CREATE FUNCTION audit_pause_expire() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='expire' THEN PERFORM pg_sleep(1); END IF; RETURN NEW; END $$; CREATE TRIGGER audit_pause BEFORE INSERT ON billing_events FOR EACH ROW EXECUTE FUNCTION audit_pause_expire();").execute(&state.pg).await?;
    let (pg, copy) = (state.pg.clone(), ledger.clone());
    let expiry = tokio::spawn(async move {
        okapi_ledger::operations::expire(&pg, &copy, uid, chrono::Utc::now()).await
    });
    tokio::time::timeout(Duration::from_secs(10),async {loop {
        let paused:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event='PgSleep')").fetch_one(&state.pg).await.unwrap();
        if paused {break;}tokio::time::sleep(Duration::from_millis(5)).await;
    }}).await?;
    let outcome = ledger
        .reserve_for_key(
            &state.pg,
            false,
            okapi_ledger::ReserveRequest {
                user_id: uid,
                api_key_id: kid,
                request_id: Uuid::new_v4(),
                est: Money::from_micros(1000),
                caps: okapi_ledger::LimitCaps::default(),
                est_tokens: 1,
            },
            chrono::Utc::now(),
        )
        .await?;
    assert!(matches!(
        outcome,
        okapi_ledger::ReserveOutcome::Insufficient { .. }
    ));
    assert_eq!(expiry.await??.as_micros(), 10000);
    assert_eq!(ledger.balance(uid).await?.as_micros(), 0);
    sqlx::raw_sql(
        "DROP TRIGGER audit_pause ON billing_events; DROP FUNCTION audit_pause_expire();",
    )
    .execute(&state.pg)
    .await?;
    Ok(())
}
#[tokio::test]
async fn replacing_empty_membership_is_serialized_on_the_parent() -> Result {
    use okapi_store::admin::{PoolMember, set_channel_pools, set_user_groups};
    let state = state().await?;
    let tag = Uuid::new_v4().simple().to_string()[..12].to_owned();
    let a = format!("a-{tag}");
    let b = format!("b-{tag}");
    for code in [&a, &b] {
        sqlx::query("INSERT INTO channel_pools(pool_code) VALUES($1)")
            .bind(code)
            .execute(&state.pg)
            .await?;
        sqlx::query("INSERT INTO price_groups(group_code,group_ratio) VALUES($1,1)")
            .bind(code)
            .execute(&state.pg)
            .await?;
    }
    let channel:i64=sqlx::query_scalar("INSERT INTO channels(name,provider,api_base,models) VALUES($1,'openai','https://example.test/v1','[]') RETURNING id").bind(tag).fetch_one(&state.pg).await?;
    let uid = user(&state.pg).await?;
    let mut fence = state.pg.acquire().await?;
    for (table, column, function, parent) in [
        ("pool_channels", "pool_code", "audit_pause_pool", channel),
        ("user_groups", "group_code", "audit_pause_group", uid),
    ] {
        sqlx::query("SELECT pg_advisory_lock(190631)")
            .execute(&mut *fence)
            .await?;
        let sql = format!(
            "CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.{column}='{a}' THEN PERFORM pg_advisory_xact_lock(190631); END IF; RETURN NEW; END $$; CREATE TRIGGER {function} BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION {function}();"
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&state.pg)
            .await?;
        let (pg, code) = (state.pg.clone(), a.clone());
        let pools = table == "pool_channels";
        let first = tokio::spawn(async move {
            if pools {
                set_channel_pools(
                    &pg,
                    parent,
                    &[PoolMember {
                        pool_code: code,
                        priority_override: None,
                        weight_override: None,
                    }],
                )
                .await
            } else {
                set_user_groups(&pg, parent, &[(code, 1)]).await
            }
        });
        tokio::time::timeout(Duration::from_secs(5),async {loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND objid=190631 AND NOT granted)").fetch_one(&state.pg).await.unwrap();
            if waiting {break;}tokio::time::sleep(Duration::from_millis(5)).await;
        }}).await?;
        let (pg, code) = (state.pg.clone(), b.clone());
        let second = tokio::spawn(async move {
            if pools {
                set_channel_pools(
                    &pg,
                    parent,
                    &[PoolMember {
                        pool_code: code,
                        priority_override: None,
                        weight_override: None,
                    }],
                )
                .await
            } else {
                set_user_groups(&pg, parent, &[(code, 2)]).await
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !second.is_finished(),
            "replacement must wait even with no member rows"
        );
        sqlx::query("SELECT pg_advisory_unlock(190631)")
            .execute(&mut *fence)
            .await?;
        first.await??;
        second.await??;
        let id_column = if pools { "channel_id" } else { "user_id" };
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT {column} FROM {table} WHERE {id_column}=$1"
        )))
        .bind(parent)
        .fetch_all(&state.pg)
        .await?;
        assert_eq!(rows, vec![b.clone()]);
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER {function} ON {table}; DROP FUNCTION {function}();"
        )))
        .execute(&state.pg)
        .await?;
    }
    Ok(())
}
#[tokio::test]
async fn credential_sealing_preserves_a_concurrent_rotation() -> Result {
    let state = state().await?;
    let channel: i64 =
        sqlx::query_scalar("INSERT INTO channels(name,provider) VALUES($1,'openai') RETURNING id")
            .bind(Uuid::new_v4().to_string())
            .fetch_one(&state.pg)
            .await?;
    let ids:Vec<i64>=sqlx::query_scalar("INSERT INTO channel_keys(channel_id,credential_ciphertext) SELECT $1,convert_to('synthetic_old','UTF8') FROM generate_series(1,2) RETURNING id").bind(channel).fetch_all(&state.pg).await?;
    let first = ids[0];
    let last = ids[1];
    let key = "07".repeat(32);
    let mut fence = state.pg.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock(190632)")
        .execute(&mut *fence)
        .await?;
    let trigger = format!(
        "CREATE FUNCTION audit_pause_seal() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.id={first} THEN PERFORM pg_advisory_xact_lock(190632); END IF; RETURN NEW; END $$; CREATE TRIGGER audit_pause_seal BEFORE UPDATE ON channel_keys FOR EACH ROW EXECUTE FUNCTION audit_pause_seal();"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(trigger))
        .execute(&state.pg)
        .await?;
    let (pg, master) = (state.pg.clone(), key.clone());
    let sealing =
        tokio::spawn(async move { okapi_store::credential::seal_existing(&pg, &master).await });
    tokio::time::timeout(Duration::from_secs(5),async {loop {
        let paused:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND objid=190632 AND NOT granted)").fetch_one(&state.pg).await.unwrap();
        if paused{break;}tokio::time::sleep(Duration::from_millis(5)).await;
    }}).await?;
    sqlx::query("UPDATE channel_keys SET credential_ciphertext=convert_to('synthetic_rotated','UTF8') WHERE id=$1").bind(last).execute(&state.pg).await?;
    sqlx::query("SELECT pg_advisory_unlock(190632)")
        .execute(&mut *fence)
        .await?;
    sealing.await??;
    let bytes: Vec<u8> =
        sqlx::query_scalar("SELECT credential_ciphertext FROM channel_keys WHERE id=$1")
            .bind(last)
            .fetch_one(&state.pg)
            .await?;
    assert_eq!(
        okapi_store::credential::open(Some(&key), &bytes)?,
        "synthetic_rotated"
    );
    sqlx::raw_sql(
        "DROP TRIGGER audit_pause_seal ON channel_keys; DROP FUNCTION audit_pause_seal();",
    )
    .execute(&state.pg)
    .await?;
    Ok(())
}
#[tokio::test]
async fn calendar_rewrites_leave_literals_and_identifiers_unchanged() -> Result {
    okapi_store::test_support::assert_isolated();
    let ch = okapi_store::ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL")?, "default")?;
    let rows = ch
        .query_json_each_row(
            "SELECT 'today()' AS literal, 'timezone()' AS zone, 'toDate(hour)' AS bucket",
        )
        .await?;
    assert_eq!(rows[0]["literal"], "today()");
    assert_eq!(rows[0]["zone"], "timezone()");
    assert_eq!(rows[0]["bucket"], "toDate(hour)");
    Ok(())
}
#[tokio::test]
async fn development_reset_removes_billing_history_and_preserves_unrelated_streams() -> Result {
    okapi_store::test_support::assert_isolated();
    let url = std::env::var("OKAPI_NATS_URL")?;
    let client = async_nats::connect(&url).await?;
    let js = okapi::worker::nats_relay::ensure_topology(&client).await?;
    let other = format!("FIXTURE_{}", Uuid::new_v4().simple());
    js.create_stream(async_nats::jetstream::stream::Config {
        name: other.clone(),
        subjects: vec![format!("fixture.{other}")],
        ..Default::default()
    })
    .await?;
    js.publish(
        "billing.audit",
        bytes::Bytes::from_static(b"old synthetic event"),
    )
    .await?
    .await?;
    let mut stream = js.get_stream("BILLING").await?;
    stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some("fixture-consumer".into()),
            ..Default::default()
        })
        .await?;
    assert_eq!(stream.info().await?.state.messages, 1);
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/nats-reset-stream.py");
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new("python3")
            .arg(script)
            .env("OKAPI_NATS_URL", url)
            .status()
    })
    .await??;
    assert!(status.success());
    assert!(js.get_stream("BILLING").await.is_err());
    assert!(js.get_stream(&other).await.is_ok());
    okapi::worker::nats_relay::ensure_topology(&client).await?;
    let mut stream = js.get_stream("BILLING").await?;
    assert_eq!(stream.info().await?.state.messages, 0);
    assert_eq!(stream.info().await?.state.consumer_count, 0);
    js.delete_stream(other).await?;
    Ok(())
}

#[tokio::test]
async fn all_chat_ingresses_authenticate_before_parsing_or_estimating_body() -> Result {
    let state = state().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let app = gateway::router(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for path in [
        "/v1/chat/completions",
        "/v1/messages",
        "/v1/responses",
        "/v1beta/models/fixture:generateContent",
    ] {
        // Invalid JSON would be 400 if any body probing preceded authentication.
        let response = reqwest::Client::new()
            .post(format!("{url}{path}"))
            .header("content-type", "application/json")
            .body("not-json".repeat(100_000))
            .send()
            .await?;
        assert_eq!(response.status(), 401, "{path}");
    }
    server.abort();
    Ok(())
}

#[tokio::test]
async fn media_routers_share_the_channel_concurrency_fence_and_refund_denied_admission() -> Result {
    use gateway::sched_redis::channel_permit::ChannelPermit;
    let state = state().await?;
    let uid = user(&state.pg).await?;
    let (_, token, _) = token(&state.pg, uid).await?;
    let model = format!("audit-media-{}", Uuid::new_v4().simple());
    okapi_store::admin::upsert_model_per_call(&state.pg, &model, 1000).await?;
    let (_, key) = okapi_store::provision::create_channel(
        &state.pg,
        &model,
        "openai",
        "http://127.0.0.1:9/v1",
        "synthetic",
        &[&model],
        false,
        None,
    )
    .await?;
    sqlx::query("UPDATE channel_keys SET max_concurrency=1 WHERE id=$1")
        .bind(key)
        .execute(&state.pg)
        .await?;
    let source = okapi_store::pricing::load_pricing_source_rows(&state.pg).await?;
    let mut snapshot = serde_json::to_value(source)?;
    snapshot["base_price_per_1m_micro"] = json!(okapi_pricing::book::BASE_PRICE_PER_1M_MICRO);
    okapi_store::admin::publish_epoch(&state.pg, uid, &snapshot).await?;
    gateway::refresh_pricebook_if_newer(&state).await?;
    state
        .ledger
        .credit(uid, Money::from_micros(1_000_000))
        .await?;
    let candidates =
        okapi_store::channels::candidates_for_model(&state.pg, &model, &["default"], None).await?;
    let permit = ChannelPermit::acquire(&state.sched, &candidates[0])
        .await?
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let app = gateway::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    for (path, body) in [
        (
            "/v1/audio/speech",
            json!({"model":model,"input":"fixture","voice":"alloy"}),
        ),
        (
            "/v1/images/generations",
            json!({"model":model,"prompt":"fixture","n":1}),
        ),
        (
            "/v1/videos",
            json!({"model":model,"prompt":"fixture","seconds":4}),
        ),
        ("/v1/embeddings", json!({"model":model,"input":"fixture"})),
    ] {
        let response = reqwest::Client::new()
            .post(format!("{url}{path}"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let error: Value = response.json().await?;
        assert_eq!(status, 503, "{path}: {error}");
        assert_eq!(
            error["error"]["code"], "no_available_channel",
            "{path}: {error}"
        );
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.ledger.balance(uid).await? == Money::from_micros(1_000_000) {
                break Ok::<_, okapi_ledger::LedgerError>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    permit.release().await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn stacked_cost_buckets_preserve_financial_records_coverage_and_margin() -> Result {
    let mut state = state().await?;
    let ch = okapi_store::ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL")?, "okapi")?;
    ch.ensure_schema().await?;
    state.ch = Some(ch.clone());
    let actor = user(&state.pg).await?;
    let owner = user(&state.pg).await?;
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(actor)
        .execute(&state.pg)
        .await?;
    let (_, admin_token, _) = token(&state.pg, actor).await?;
    let (key, _, _) = token(&state.pg, owner).await?;
    let model = format!("audit-stack-{}", Uuid::new_v4().simple());
    for _ in 0..2 {
        let payload = json!({"request_id":Uuid::new_v4(),"input_unit":"tokens","input_characters":null,
            "user_id":owner,"api_key_id":key,"group":"default","model":model,"channel_id":0,"channel_key_id":0,
            "log_type":2,"prompt_source":"upstream","completion_source":"upstream",
            "upstream_usage":{"prompt_tokens":100,"completion_tokens":200},"prompt_tokens":100,"cached_tokens":40,
            "cache_read_reported":true,"completion_tokens":200,"reasoning_tokens":0,"amount_micro":1000,
            "original_amount_micro":1000,"discount_micro":0,"upstream_cost_micro":500,"upstream_cost_known":true,"pricing_epoch":1,
            "latency_ms":1000,"ttft_ms":120,"is_stream":true,"sticky_layer":0,"failover_count":0,
            "error_code":"","node":"audit-stack","client_type":"test"});
        sqlx::query("INSERT INTO billing_outbox(topic,payload) VALUES('request_log',$1)")
            .bind(payload)
            .execute(&state.pg)
            .await?;
    }
    for _ in 0..20 {
        if okapi::worker::chsink::process_once(&state.pg, &ch).await? == 0 {
            break;
        }
    }
    let (url, server) = serve(state.clone()).await?;
    for suffix in ["", "&stack=model"] {
        let response = reqwest::Client::new()
            .get(format!(
                "{url}/admin/stats/trend?days=1&user_id={owner}&compare=false&cached=false{suffix}"
            ))
            .bearer_auth(&admin_token)
            .send()
            .await?;
        let status = response.status();
        let result: Value = response.json().await?;
        assert_eq!(status, 200, "{result}");
        let bucket = if suffix.is_empty() {
            &result["data"][0]
        } else {
            &result["data"][0]["values"][&model]
        };
        for metrics in [&result["total"], bucket] {
            assert_eq!(metrics["financial_records"], 2, "{metrics}");
            assert_eq!(metrics["cost_coverage_bp"], 10_000, "{metrics}");
            assert_eq!(metrics["margin_micro"], 1000, "{metrics}");
        }
    }
    server.abort();
    Ok(())
}
