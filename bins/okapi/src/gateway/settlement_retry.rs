//! Redis journal bridges PG outages; request_id remains the PG idempotency key.
//! Capacity includes quarantined records: old bills are never evicted.
use super::state::AppState;
use fred::interfaces::{HashesInterface, LuaInterface, SortedSetsInterface};
use futures::{StreamExt, stream};
use okapi_ledger::SettlementInput;

const PAYLOADS: &str = "settlement:{retry}:payloads";
const ORDER: &str = "settlement:{retry}:order";
const ATTEMPTS: &str = "settlement:{retry}:attempts";
const QUARANTINE: &str = "settlement:{retry}:quarantine";
const MAX_PAYLOAD_BYTES: usize = 262_144;
const SAVE: &str = "
local order_type=redis.call('TYPE',KEYS[2]).ok
if order_type~='none' and order_type~='zset' then return redis.error_reply('invalid settlement journal order type') end
local existing=redis.call('HEXISTS',KEYS[1],ARGV[1])
local retained=redis.call('HLEN',KEYS[1])+redis.call('HLEN',KEYS[3])
if existing==0 and (retained>=tonumber(ARGV[4]) or redis.call('HEXISTS',KEYS[3],ARGV[1])==1) then return 0 end
redis.call('HSET',KEYS[1],ARGV[1],ARGV[2])
redis.call('ZADD',KEYS[2],'NX',ARGV[3],ARGV[1])
return 1";

fn configured_limit(name: &str, default: i64, max: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0 && *v <= max)
        .unwrap_or(default)
}

pub async fn save(state: &AppState, input: &SettlementInput<'_>) -> anyhow::Result<()> {
    let payload = serde_json::to_string(input)?;
    anyhow::ensure!(
        payload.len() <= MAX_PAYLOAD_BYTES,
        "settlement journal payload too large"
    );
    let accepted: i64 = state
        .sched
        .client()
        .eval(
            SAVE,
            vec![PAYLOADS, ORDER, QUARANTINE],
            vec![
                input.request_id.to_string(),
                payload,
                chrono::Utc::now().timestamp_millis().to_string(),
                configured_limit("OKAPI_SETTLEMENT_JOURNAL_MAX", 100_000, 1_000_000).to_string(),
            ],
        )
        .await?;
    anyhow::ensure!(
        accepted == 1,
        "settlement journal capacity exhausted or entry quarantined"
    );
    Ok(())
}

pub async fn remove(state: &AppState, id: uuid::Uuid) -> anyhow::Result<()> {
    remove_ids(state, vec![id.to_string()]).await
}

async fn remove_ids(state: &AppState, ids: Vec<String>) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let _: i64 = state.sched.client().eval(
        "for _,id in ipairs(ARGV) do redis.call('HDEL',KEYS[1],id); redis.call('ZREM',KEYS[2],id); redis.call('HDEL',KEYS[3],id) end return 1",
        vec![PAYLOADS,ORDER,ATTEMPTS],ids,
    ).await?;
    Ok(())
}

pub async fn recover(state: &AppState) -> anyhow::Result<usize> {
    let batch = configured_limit("OKAPI_SETTLEMENT_RECOVERY_BATCH", 500, 10_000);
    let ids: Vec<String> = state
        .sched
        .client()
        .zrangebyscore(
            ORDER,
            "-inf",
            chrono::Utc::now().timestamp_millis(),
            false,
            Some((0, batch)),
        )
        .await?;
    let results = stream::iter(ids)
        .map(|id| async move { recover_one(state, id).await })
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
    let mut accepted = Vec::new();
    for result in results {
        match result {
            Ok(Some(id)) => accepted.push(id),
            Ok(None) => {}
            Err(error) => tracing::error!(%error,"settlement journal recovery deferred"),
        }
    }
    let recovered = accepted.len();
    remove_ids(state, accepted).await?;
    Ok(recovered)
}

async fn recover_one(state: &AppState, id: String) -> anyhow::Result<Option<String>> {
    let Some(payload): Option<String> = state.sched.client().hget(PAYLOADS, &id).await? else {
        // A concurrent save may have populated it since HGET. Check again atomically.
        let _: i64=state.sched.client().eval(
            "if redis.call('HEXISTS',KEYS[1],ARGV[1])==0 then redis.call('ZREM',KEYS[2],ARGV[1]);redis.call('HDEL',KEYS[3],ARGV[1]) end return 1",
            vec![PAYLOADS,ORDER,ATTEMPTS],vec![id],
        ).await?;
        return Ok(None);
    };
    let owned: okapi_ledger::pg::OwnedSettlementInput = match serde_json::from_str::<
        okapi_ledger::pg::OwnedSettlementInput,
    >(&payload)
    {
        Ok(input) if uuid::Uuid::parse_str(&id).ok() == Some(input.request_id) => input,
        result => {
            tracing::error!(%id,error=?result.err(),"invalid settlement journal entry quarantined for repair");
            quarantine(state, &id, &payload).await?;
            return Ok(None);
        }
    };
    let input = owned.as_input();
    let _permit = state.settle_gate.acquire().await?;
    let result = if input.log_type == 2 {
        state.persist_success(input.clone()).await
    } else {
        okapi_ledger::record_settlement(&state.pg, input.clone())
            .await
            .map(|()| false)
    };
    match result {
        Ok(inserted) => {
            if inserted {
                let member = sqlx::query_scalar!(
                    "SELECT member_user_id FROM api_keys WHERE id=$1",
                    input.api_key_id
                )
                .fetch_optional(&state.pg)
                .await
                .ok()
                .flatten()
                .flatten();
                super::auth::record_settlement_counters(
                    state,
                    input.user_id,
                    member,
                    input.amount.as_micros(),
                    input.usage.total_raw(),
                )
                .await;
                state
                    .sched
                    .kpi_record(input.usage.total_raw(), input.amount.as_micros(), false)
                    .await;
            }
            Ok(Some(id))
        }
        Err(error) => {
            if permanent_failure(&error) {
                tracing::error!(%id,%error,"settlement journal entry requires repair; quarantined without deletion");
                quarantine(state, &id, &payload).await?;
                return Ok(None);
            }
            tracing::warn!(%id,%error,"settlement journal retry scheduled");
            defer(state, &id).await?;
            Ok(None)
        }
    }
}

fn permanent_failure(error: &okapi_ledger::LedgerError) -> bool {
    use okapi_ledger::LedgerError;
    match error {
        LedgerError::InvalidSettlement
        | LedgerError::ReservationConflict
        | LedgerError::InvalidHold(_)
        | LedgerError::HoldConflict => true,
        LedgerError::Sqlx(sqlx::Error::Database(error)) => error
            .code()
            .is_some_and(|code| code.starts_with("22") || code.starts_with("23")),
        _ => false,
    }
}

async fn defer(state: &AppState, id: &str) -> anyhow::Result<()> {
    let _: i64=state.sched.client().eval(
        "local n=redis.call('HINCRBY',KEYS[2],ARGV[1],1); local wait=math.min(300000,1000*2^math.min(n,9)); redis.call('ZADD',KEYS[1],tonumber(ARGV[2])+wait,ARGV[1]); return 1",
        vec![ORDER,ATTEMPTS],vec![id.to_owned(),chrono::Utc::now().timestamp_millis().to_string()],
    ).await?;
    Ok(())
}

async fn quarantine(state: &AppState, id: &str, payload: &str) -> anyhow::Result<()> {
    let _: i64=state.sched.client().eval(
        "local p=redis.call('HGET',KEYS[1],ARGV[1]); if p==ARGV[2] then redis.call('HSET',KEYS[3],ARGV[1],p);redis.call('HDEL',KEYS[1],ARGV[1]);redis.call('ZREM',KEYS[2],ARGV[1]);redis.call('HDEL',KEYS[4],ARGV[1]) end return 1",
        vec![PAYLOADS,ORDER,QUARANTINE,ATTEMPTS],vec![id.to_owned(),payload.to_owned()],
    ).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use okapi_domain::{BillingState, Money, TokenUsage};
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn pg_outage_journals_usage_and_replay_charges_once() {
        dotenvy::dotenv().ok();
        let state = crate::gateway::build_state(
            &std::env::var("DATABASE_URL").unwrap(),
            &std::env::var("OKAPI_REDIS_URL").unwrap(),
            "journal-test",
            None,
            None,
        )
        .await
        .unwrap();
        let suffix = uuid::Uuid::new_v4().to_string();
        let user = okapi_store::provision::create_user(&state.pg, &format!("journal-{suffix}"))
            .await
            .unwrap();
        let kid = okapi_store::provision::create_api_key(&state.pg, user, &suffix, "journal")
            .await
            .unwrap();
        okapi_ledger::operations::credit(
            &state.pg,
            &state.ledger,
            user,
            Money::from_micros(10_000),
            "adjust",
            "test",
            serde_json::json!({}),
        )
        .await
        .unwrap();
        let id = uuid::Uuid::new_v4();
        state
            .ledger
            .reserve_for_key(
                &state.pg,
                false,
                okapi_ledger::ReserveRequest {
                    user_id: user,
                    api_key_id: kid,
                    request_id: id,
                    est: Money::from_micros(1000),
                    caps: okapi_ledger::LimitCaps::default(),
                    est_tokens: 0,
                },
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        let input = SettlementInput {
            source_window: None,
            dimensions: okapi_ledger::pg::UsageDimensions::new(
                "test",
                "test",
                "/v1/chat/completions",
                "/v1/chat/completions",
            ),
            request_id: id,
            log_type: 2,
            user_id: user,
            api_key_id: kid,
            group_code: "default",
            model_name: "test",
            channel_id: None,
            channel_key_id: None,
            state: BillingState::Committed,
            usage: TokenUsage {
                prompt_tokens: 20,
                completion_tokens: 5,
                cached_tokens: 2,
                cache_write_tokens: 3,
                reasoning_tokens: 1,
                cache_read_reported: true,
                cache_write_reported: true,
                ..TokenUsage::default()
            },
            amount: Money::from_micros(500),
            original: Money::from_micros(500),
            discount: Money::ZERO,
            list_price: Money::from_micros(500),
            upstream_cost: None,
            pricing_epoch: None,
            pricing_snapshot: None,
            latency_ms: 1,
            ttft_ms: None,
            is_stream: false,
            retry_count: 0,
            failover_count: 0,
            upstream_status: Some(200),
            error_code: None,
            upstream_request_id: None,
            node: "test\"quoted\nnode",
            sticky_layer: 0,
            client_type: "test",
            client_ip: None,
            delta_micro: -500,
            balance_after: None,
            event_type: "commit",
            pool: okapi_ledger::Pool::Wallet,
        };
        let mut unavailable = state.clone();
        unavailable.pg = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://test:test@127.0.0.1:9/unavailable")
            .unwrap();
        assert!(!unavailable.settle_success(input.clone()).await.unwrap());
        assert_eq!(state.ledger.balance(user).await.unwrap().as_micros(), 9000);
        let journal: String = state
            .sched
            .client()
            .hget(PAYLOADS, id.to_string())
            .await
            .unwrap();
        let journal: serde_json::Value = serde_json::from_str(&journal).unwrap();
        assert_eq!(journal["usage"]["prompt_tokens"], 20);
        assert_eq!(journal["usage"]["completion_tokens"], 5);
        assert_eq!(journal["usage"]["cache_write_tokens"], 3);
        recover(&state).await.unwrap();
        assert_eq!(state.ledger.balance(user).await.unwrap().as_micros(), 9500);
        save(&state, &input).await.unwrap();
        recover(&state).await.unwrap();
        assert_eq!(state.ledger.balance(user).await.unwrap().as_micros(), 9500);
        let bills: i64 =
            sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE request_id=$1")
                .bind(id)
                .fetch_one(&state.pg)
                .await
                .unwrap();
        assert_eq!(bills, 1);
        let bill: (i32, i32, i32, i32, i64) = sqlx::query_as("SELECT prompt_tokens,cached_tokens,completion_tokens,reasoning_tokens,amount_micro FROM billing_records WHERE request_id=$1")
            .bind(id).fetch_one(&state.pg).await.unwrap();
        assert_eq!(bill, (20, 2, 5, 1, 500));
        let payloads: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT payload FROM billing_outbox WHERE payload->>'request_id'=$1",
        )
        .bind(id.to_string())
        .fetch_all(&state.pg)
        .await
        .unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["cache_write_tokens"], 3);
        assert_eq!(payloads[0]["amount_micro"], 500);
        let remaining: Option<String> = state
            .sched
            .client()
            .hget(PAYLOADS, id.to_string())
            .await
            .unwrap();
        assert!(remaining.is_none());
        let poison = uuid::Uuid::new_v4().to_string();
        let _:i64=state.sched.client().eval("redis.call('HSET',KEYS[1],ARGV[1],'invalid-json');redis.call('ZADD',KEYS[2],0,ARGV[1]);return 1",vec![PAYLOADS,ORDER],vec![poison.clone()]).await.unwrap();
        recover(&state).await.unwrap();
        let quarantined: Option<String> = state
            .sched
            .client()
            .hget(QUARANTINE, &poison)
            .await
            .unwrap();
        assert_eq!(quarantined.as_deref(), Some("invalid-json"));
        let active: Option<String> = state.sched.client().hget(PAYLOADS, &poison).await.unwrap();
        assert!(active.is_none());
        let _: i64 = state
            .sched
            .client()
            .hdel(QUARANTINE, &poison)
            .await
            .unwrap();

        // Valid JSON with an invalid amount must not retry forever either.
        let mut invalid = input.clone();
        invalid.request_id = uuid::Uuid::new_v4();
        invalid.amount = Money::from_micros(-1);
        save(&state, &invalid).await.unwrap();
        recover(&state).await.unwrap();
        let invalid_payload: Option<String> = state
            .sched
            .client()
            .hget(QUARANTINE, invalid.request_id.to_string())
            .await
            .unwrap();
        assert!(invalid_payload.is_some());
        let _: i64 = state
            .sched
            .client()
            .hdel(QUARANTINE, invalid.request_id.to_string())
            .await
            .unwrap();

        let config = fred::types::config::Config::from_url("redis://127.0.0.1:9").unwrap();
        let client = fred::prelude::Builder::from_config(config)
            .with_performance_config(|config| {
                config.default_command_timeout = std::time::Duration::from_millis(100);
            })
            .build()
            .unwrap();
        unavailable.sched = crate::gateway::sched_redis::SchedulerRedis::new(client);
        unavailable.settle_gate = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        unavailable.settle_backlog = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        unavailable.settlements = crate::shutdown::Pending::default();
        let mut failed = input.clone();
        failed.request_id = uuid::Uuid::new_v4();
        failed.log_type = 5;
        failed.state = BillingState::Failed;
        failed.amount = Money::ZERO;
        failed.original = Money::ZERO;
        failed.list_price = Money::ZERO;
        failed.delta_micro = 0;
        failed.error_code = Some(okapi_api::codes::UPSTREAM_TIMEOUT);
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            unavailable.settle_write(failed),
        )
        .await
        .unwrap();
        assert_eq!(unavailable.settle_gate.available_permits(), 1);
        assert_eq!(unavailable.settlements.in_flight(), 1);
        assert_eq!(
            unavailable
                .settle_backlog
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
    #[tokio::test]
    async fn journal_capacity_keeps_existing_payloads() {
        dotenvy::dotenv().ok();
        let client = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
            .await
            .unwrap();
        let suffix = uuid::Uuid::new_v4();
        let keys = vec![
            format!("settlement:{{test-{suffix}}}:payloads"),
            format!("settlement:{{test-{suffix}}}:order"),
            format!("settlement:{{test-{suffix}}}:quarantine"),
        ];
        for (id, expected) in [("a", 1), ("b", 0), ("a", 1)] {
            let result: i64 = client
                .eval(SAVE, keys.clone(), vec![id, "payload", "0", "1"])
                .await
                .unwrap();
            assert_eq!(result, expected);
        }
        let payload: Option<String> = client.hget(&keys[0], "a").await.unwrap();
        assert_eq!(payload.as_deref(), Some("payload"));
        let _: i64 = fred::interfaces::KeysInterface::del(&client, keys)
            .await
            .unwrap();
    }
}
