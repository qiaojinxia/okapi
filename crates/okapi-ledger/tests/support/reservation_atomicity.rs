//! Redis Lua errors do not roll back writes. Every touched counter must be
//! validated before funds or recovery records are changed, even without caps.
use super::*;
use fred::{interfaces::LuaInterface, types::Value};

const MAXIMUM: u64 = 9_007_199_254_740_991;

fn keys(bed: &Bed) -> Vec<String> {
    let slot = format!("{{{}}}:k:{}", bed.request.user_id, bed.request.api_key_id);
    let minute = bed.now.timestamp().div_euclid(60);
    vec![
        bed.balance_key(),
        format!("rl:{slot}:rpm:{minute}"),
        format!("rl:{slot}:tpm:{minute}"),
        format!("rl:{slot}:rpd:{}", bed.now.format("%Y%m%d")),
        format!("conc:{slot}"),
    ]
}

async fn snapshot(bed: &Bed) -> TestResult<Vec<Value>> {
    // DUMP includes the value and type, but not expiry. Seeded keys have no TTL,
    // so PTTL also detects an unsuccessful admission changing their lifetime.
    Ok(bed.redis.eval(
        "local r = {} for _, k in ipairs(KEYS) do table.insert(r, redis.call('DUMP', k)) table.insert(r, redis.call('PTTL', k)) end return r",
        keys(bed), Vec::<String>::new(),
    ).await?)
}

async fn seeded(caps: i64, subscription: bool) -> TestResult<Bed> {
    let mut bed = Bed::new().await?;
    bed.request.caps = LimitCaps {
        rpm: caps,
        tpm: caps,
        rpd: caps,
        concurrency: caps,
    };
    for key in keys(&bed).into_iter().skip(1) {
        bed.redis
            .set::<(), _, _>(key, "5", None, None, false)
            .await?;
    }
    if subscription {
        bed.ledger
            .sub_set(
                bed.request.user_id,
                Money::from_micros(500),
                bed.now.timestamp() + 3600,
            )
            .await?;
    }
    Ok(bed)
}

async fn rejected_unchanged(bed: &Bed) -> TestResult {
    let before = snapshot(bed).await?;
    let result = bed.ledger.reserve(bed.request, bed.now).await;
    assert!(
        result.is_err(),
        "invalid admission must fail closed: {result:?}"
    );
    assert_eq!(
        snapshot(bed).await?,
        before,
        "failed admission changed balance, reservation, counters or TTLs"
    );
    assert!(
        bed.ledger
            .list_reservations(bed.request.user_id)
            .await?
            .is_empty()
    );
    Ok(())
}

async fn corrupt_types(subscription: bool) -> TestResult {
    for caps in [0, -1, 100] {
        for axis in 1..=4 {
            let bed = seeded(caps, subscription).await?;
            let key = &keys(&bed)[axis];
            bed.redis.del::<(), _>(key).await?;
            bed.redis.hset::<(), _, _>(key, ("bad", "type")).await?;
            rejected_unchanged(&bed).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn wrong_counter_types_cannot_partially_debit_wallet() -> TestResult {
    corrupt_types(false).await
}

#[tokio::test]
async fn wrong_counter_types_cannot_partially_debit_subscription() -> TestResult {
    corrupt_types(true).await
}

#[tokio::test]
async fn malformed_and_overflowing_counters_cannot_change_funds() -> TestResult {
    for caps in [0, 100] {
        for axis in 1..=4 {
            for invalid in [
                "bad",
                "",
                "01",
                "+1",
                "-1",
                "1.0",
                "1e2",
                " 1",
                "9007199254740991",
                "9007199254740992",
                "9223372036854775807",
            ] {
                let bed = seeded(caps, false).await?;
                bed.redis
                    .set::<(), _, _>(&keys(&bed)[axis], invalid, None, None, false)
                    .await?;
                rejected_unchanged(&bed).await?;
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn invalid_estimates_fail_before_any_write() -> TestResult {
    for (amount, tokens) in [(-1, 10), (1_000, MAXIMUM + 1), (1_000, u64::MAX)] {
        let mut bed = seeded(0, false).await?;
        bed.request.est = Money::from_micros(amount);
        bed.request.est_tokens = tokens;
        rejected_unchanged(&bed).await?;
    }
    Ok(())
}

#[tokio::test]
async fn unsafe_balances_and_limits_are_rejected_without_writes() -> TestResult {
    for field in ["avail", "sub", "sub_until"] {
        for invalid in ["01", "1e2", "9007199254740992", "-9007199254740992"] {
            let bed = seeded(0, false).await?;
            bed.redis
                .hset::<(), _, _>(bed.balance_key(), (field, invalid))
                .await?;
            rejected_unchanged(&bed).await?;
        }
    }
    let mut bed = seeded(i64::MAX, false).await?;
    rejected_unchanged(&bed).await?;
    bed.request.caps = LimitCaps::default();
    bed.request.est = Money::from_micros(i64::MAX);
    rejected_unchanged(&bed).await
}

#[tokio::test]
async fn largest_safe_integers_and_zero_cost_admit_exactly() -> TestResult {
    let mut bed = Bed::new().await?;
    let maximum = i64::try_from(MAXIMUM)?;
    bed.redis
        .hset::<(), _, _>(bed.balance_key(), ("avail", maximum))
        .await?;
    bed.request.est = Money::from_micros(maximum);
    bed.request.est_tokens = MAXIMUM;
    assert!(matches!(
        bed.ledger.reserve(bed.request, bed.now).await?,
        ReserveOutcome::Reserved {
            balance_after: Money::ZERO,
            pool: Pool::Wallet
        }
    ));
    assert_eq!(
        bed.ledger.list_reservations(bed.request.user_id).await?[0]
            .amount
            .as_micros(),
        maximum
    );
    let tokens: String = bed.redis.get(&keys(&bed)[2]).await?;
    assert_eq!(tokens, MAXIMUM.to_string());

    let mut free = Bed::new().await?;
    free.request.est = Money::ZERO;
    free.request.est_tokens = 0;
    assert!(matches!(
        free.ledger.reserve(free.request, free.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    free.check_one_hold(0).await?;
    assert_eq!(
        free.ledger.balance(free.request.user_id).await?.as_micros(),
        10_000
    );
    let tokens: String = free.redis.get(&keys(&free)[2]).await?;
    assert_eq!(tokens, "0");

    let edge = seeded(0, false).await?;
    for (index, key) in keys(&edge).into_iter().enumerate().skip(1) {
        let increment = if index == 2 { 10 } else { 1 };
        edge.redis
            .set::<(), _, _>(key, maximum - increment, None, None, false)
            .await?;
    }
    assert!(matches!(
        edge.ledger.reserve(edge.request, edge.now).await?,
        ReserveOutcome::Reserved { .. }
    ));
    for key in keys(&edge).into_iter().skip(1) {
        let value: String = edge.redis.get(key).await?;
        assert_eq!(value, MAXIMUM.to_string());
    }
    Ok(())
}
