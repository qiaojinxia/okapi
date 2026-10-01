//! Redis journal bridges PG outages; request_id remains the PG idempotency key.
//! Payload and retry order use one Cluster slot and have no expiry.
use super::state::AppState;
use fred::interfaces::{HashesInterface, LuaInterface, SortedSetsInterface};
use okapi_ledger::SettlementInput;

const PAYLOADS: &str = "settlement:{retry}:payloads";
const ORDER: &str = "settlement:{retry}:order";

pub async fn save(state: &AppState, input: &SettlementInput<'_>) -> anyhow::Result<()> {
    let payload = serde_json::to_string(input)?;
    let _: i64 = state.sched.client().eval(
        "redis.call('HSET',KEYS[1],ARGV[1],ARGV[2]); redis.call('ZADD',KEYS[2],'NX',ARGV[3],ARGV[1]); return 1",
        vec![PAYLOADS,ORDER],
        vec![input.request_id.to_string(),payload,chrono::Utc::now().timestamp_millis().to_string()],
    ).await?;
    Ok(())
}

pub async fn remove(state: &AppState, id: uuid::Uuid) -> anyhow::Result<()> {
    let _: i64 = state
        .sched
        .client()
        .eval(
            "redis.call('HDEL',KEYS[1],ARGV[1]); redis.call('ZREM',KEYS[2],ARGV[1]); return 1",
            vec![PAYLOADS, ORDER],
            vec![id.to_string()],
        )
        .await?;
    Ok(())
}

pub async fn recover(state: &AppState) -> anyhow::Result<usize> {
    let ids: Vec<String> = state
        .sched
        .client()
        .zrange(ORDER, 0, 99, None, false, None, false)
        .await?;
    let mut recovered = 0;
    for id in ids {
        let Some(payload): Option<String> = state.sched.client().hget(PAYLOADS, &id).await? else {
            continue;
        };
        let owned: okapi_ledger::pg::OwnedSettlementInput = match serde_json::from_str(&payload) {
            Ok(input) => input,
            Err(error) => {
                tracing::error!(%id,%error,"invalid settlement journal entry retained for repair");
                defer(state, &id).await?;
                continue;
            }
        };
        let input = owned.as_input();
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
                remove(state, input.request_id).await?;
                recovered += 1;
            }
            Err(error) => {
                tracing::error!(%id,%error,"settlement journal recovery deferred");
                defer(state, &id).await?;
            }
        }
    }
    Ok(recovered)
}

// Move failed entries behind the rest so one poison entry cannot starve newer bills.
async fn defer(state: &AppState, id: &str) -> anyhow::Result<()> {
    let _: i64 = state
        .sched
        .client()
        .eval(
            "redis.call('ZADD',KEYS[1],ARGV[1],ARGV[2]); return 1",
            vec![ORDER],
            vec![
                chrono::Utc::now().timestamp_millis().to_string(),
                id.to_owned(),
            ],
        )
        .await?;
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
            usage: TokenUsage::default(),
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
    }
}
