//! Duplicate active reservations must never debit twice or replace their recovery record.
use chrono::{DateTime, TimeDelta, Utc};
use fred::{
    clients::Client,
    interfaces::{HashesInterface, KeysInterface},
};
use okapi_domain::Money;
use okapi_ledger::{BalanceLedger, LedgerError, LimitCaps, Pool, ReserveOutcome, ReserveRequest};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[path = "support/reservation_atomicity.rs"]
mod atomicity;
#[path = "support/settlement_atomicity.rs"]
mod settlement;

struct Bed {
    ledger: BalanceLedger,
    redis: Client,
    request: ReserveRequest,
    now: DateTime<Utc>,
}
impl Bed {
    async fn new() -> TestResult<Self> {
        okapi_store::test_support::assert_isolated();
        let url = std::env::var("OKAPI_REDIS_URL")?;
        let redis = okapi_store::connect_redis(&url).await?;
        let mut bytes = [0; 4];
        bytes.copy_from_slice(&Uuid::new_v4().as_bytes()[..4]);
        let uid = i64::from(u32::from_le_bytes(bytes))
            .checked_add(1)
            .and_then(i64::checked_neg)
            .ok_or("test uid overflow")?;
        let ledger = BalanceLedger::new(redis.clone());
        ledger.credit(uid, Money::from_micros(10_000)).await?;
        Ok(Self {
            ledger,
            redis,
            request: ReserveRequest {
                user_id: uid,
                api_key_id: 7,
                request_id: Uuid::new_v4(),
                est: Money::from_micros(1_000),
                caps: LimitCaps::default(),
                est_tokens: 10,
            },
            now: Utc::now(),
        })
    }
    fn balance_key(&self) -> String {
        format!("bal:{{{}}}", self.request.user_id)
    }
    async fn snapshot(&self) -> TestResult<(BTreeMap<String, String>, BTreeMap<String, String>)> {
        let balance = self.redis.hgetall(self.balance_key()).await?;
        let mut keys = BTreeSet::new();
        for kid in [7, 8] {
            for elapsed in [0, 61, 601] {
                let now = self
                    .now
                    .checked_add_signed(TimeDelta::seconds(elapsed))
                    .ok_or("test time overflow")?;
                let slot = format!("{{{}}}:k:{kid}", self.request.user_id);
                let minute = now.timestamp().div_euclid(60);
                keys.extend([
                    format!("conc:{slot}"),
                    format!("rl:{slot}:rpm:{minute}"),
                    format!("rl:{slot}:tpm:{minute}"),
                    format!("rl:{slot}:rpd:{}", now.format("%Y%m%d")),
                ]);
            }
        }
        let mut counters = BTreeMap::new();
        for key in keys {
            if let Some(value) = self.redis.get::<Option<String>, _>(&key).await? {
                counters.insert(key, value);
            }
        }
        Ok((balance, counters))
    }
    async fn check_one_hold(&self, amount: i64) -> TestResult {
        let reservations = self.ledger.list_reservations(self.request.user_id).await?;
        assert_eq!(reservations.len(), 1);
        assert_eq!(reservations[0].amount.as_micros(), amount);
        assert_eq!(reservations[0].api_key_id, self.request.api_key_id);
        let concurrency: i64 = self
            .redis
            .get(format!(
                "conc:{{{}}}:k:{}",
                self.request.user_id, self.request.api_key_id
            ))
            .await?;
        assert_eq!(concurrency, 1);
        Ok(())
    }
}

#[tokio::test]
async fn active_replay_never_debits_again_or_extends_its_deadline() -> TestResult {
    let bed = Bed::new().await?;
    assert!(matches!(
        bed.ledger.reserve(bed.request, bed.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    let before = bed.snapshot().await?;
    for elapsed in [0, 61, 601] {
        let now = bed
            .now
            .checked_add_signed(TimeDelta::seconds(elapsed))
            .ok_or("test time overflow")?;
        let result = bed.ledger.reserve(bed.request, now).await;
        assert!(
            matches!(result, Err(LedgerError::ReservationExists)),
            "existing admission cannot be reused: {result:?}"
        );
        assert_eq!(bed.snapshot().await?, before);
    }
    bed.check_one_hold(1_000).await?;
    assert_eq!(
        bed.ledger.balance(bed.request.user_id).await?.as_micros(),
        9_000
    );
    let refund = bed
        .ledger
        .refund(
            bed.request.user_id,
            bed.request.api_key_id,
            bed.request.request_id,
        )
        .await?;
    assert_eq!(refund.released.as_micros(), 1_000);
    assert_eq!(
        bed.ledger.balance(bed.request.user_id).await?.as_micros(),
        10_000
    );
    Ok(())
}

#[tokio::test]
async fn changed_amount_key_or_limits_cannot_replace_an_active_hold() -> TestResult {
    let bed = Bed::new().await?;
    bed.ledger.reserve(bed.request, bed.now).await?;
    let before = bed.snapshot().await?;
    let variants = [
        ReserveRequest {
            est: Money::from_micros(500),
            ..bed.request
        },
        ReserveRequest {
            api_key_id: 8,
            ..bed.request
        },
        ReserveRequest {
            est_tokens: 999,
            ..bed.request
        },
        ReserveRequest {
            caps: LimitCaps {
                concurrency: 1,
                ..LimitCaps::default()
            },
            ..bed.request
        },
    ];
    for request in variants {
        assert!(matches!(
            bed.ledger.reserve(request, bed.now).await,
            Err(LedgerError::ReservationExists)
        ));
        assert_eq!(bed.snapshot().await?, before);
    }
    bed.check_one_hold(1_000).await
}

#[tokio::test]
async fn concurrent_duplicate_admissions_debit_and_count_exactly_once() -> TestResult {
    let bed = Bed::new().await?;
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let ledger = bed.ledger.clone();
        let request = bed.request;
        let now = bed.now;
        calls.spawn(async move { ledger.reserve(request, now).await });
    }
    let mut admitted = 0;
    let mut rejected = 0;
    while let Some(result) = calls.join_next().await {
        match result? {
            Ok(ReserveOutcome::Reserved { .. }) => admitted += 1,
            Err(LedgerError::ReservationExists) => rejected += 1,
            other => return Err(format!("unexpected admission: {other:?}").into()),
        }
    }
    assert_eq!((admitted, rejected), (1, 31));
    assert_eq!(
        bed.ledger.balance(bed.request.user_id).await?.as_micros(),
        9_000
    );
    bed.check_one_hold(1_000).await?;
    let (_, counters) = bed.snapshot().await?;
    assert_eq!(counters.len(), 4);
    for (key, count) in counters {
        assert_eq!(count, if key.contains(":tpm:") { "10" } else { "1" });
    }
    Ok(())
}

#[tokio::test]
async fn subscription_overdraw_replay_cannot_switch_to_wallet() -> TestResult {
    let bed = Bed::new().await?;
    let until = bed
        .now
        .timestamp()
        .checked_add(3_600)
        .ok_or("test time overflow")?;
    bed.ledger
        .sub_set(bed.request.user_id, Money::from_micros(500), until)
        .await?;
    assert!(matches!(
        bed.ledger.reserve(bed.request, bed.now).await?,
        ReserveOutcome::Reserved {
            pool: Pool::Subscription,
            ..
        }
    ));
    let before = bed.snapshot().await?;
    assert!(matches!(
        bed.ledger.reserve(bed.request, bed.now).await,
        Err(LedgerError::ReservationExists)
    ));
    assert_eq!(bed.snapshot().await?, before);
    assert_eq!(
        bed.ledger.balance(bed.request.user_id).await?.as_micros(),
        10_000
    );
    bed.check_one_hold(1_000).await?;
    let refund = bed
        .ledger
        .refund(bed.request.user_id, 7, bed.request.request_id)
        .await?;
    assert_eq!(refund.pool, Pool::Subscription);
    assert_eq!(refund.balance_after.as_micros(), 500);
    Ok(())
}

#[tokio::test]
async fn zero_cost_and_legacy_or_malformed_records_still_block_reentry() -> TestResult {
    let mut bed = Bed::new().await?;
    bed.request.est = Money::ZERO;
    bed.ledger.reserve(bed.request, bed.now).await?;
    for record in ["0|123|7|0", "0|123|7", "corrupt", ""] {
        let _: i64 = bed
            .redis
            .hset(
                bed.balance_key(),
                (format!("r:{}", bed.request.request_id), record),
            )
            .await?;
        let before = bed.snapshot().await?;
        assert!(matches!(
            bed.ledger.reserve(bed.request, bed.now).await,
            Err(LedgerError::ReservationExists)
        ));
        assert_eq!(bed.snapshot().await?, before);
    }
    Ok(())
}
