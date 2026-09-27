use super::*;
use std::{collections::BTreeMap, time::Duration};

async fn interrupted(
    subscription: bool,
    failed_upstream: bool,
    endpoint: &'static str,
) -> TestResult {
    let bed = Arc::new(Bed::new(subscription).await?);
    bed.gate
        .mode
        .store(if failed_upstream { 2 } else { 1 }, Ordering::SeqCst);
    let request = {
        let bed = bed.clone();
        tokio::spawn(async move { bed.generate(endpoint).await })
    };
    tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
    let balance_key = format!("bal:{{{}}}", bed.uid);
    let before: BTreeMap<String, String> = bed.redis.hgetall(&balance_key).await?;
    let held = bed.ledger.list_reservations(bed.uid).await?;
    assert_eq!(held.len(), 1);
    let id = held[0].request_id;
    assert!(held[0].amount.as_micros() > 0);
    let key = format!("conc:{{{}}}:k:{}", bed.uid, bed.kid);
    bed.redis.del::<(), _>(&key).await?;
    bed.redis
        .hset::<(), _, _>(&key, ("invalid", "counter"))
        .await?;
    bed.gate.release.notify_one();
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), request).await???;
    let expected_status = if !failed_upstream {
        200
    } else if endpoint == "/v1/chat/completions" {
        400
    } else {
        502
    };
    assert_eq!(status, expected_status, "{body}");
    if failed_upstream && endpoint != "/v1/chat/completions" {
        assert_eq!(body["error"]["code"], "upstream_error");
        assert_eq!(body["error"]["param"], "upstream_status_400");
    }
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(
        bed.pending.in_flight(),
        0,
        "settlement task must have attempted closure"
    );
    assert_eq!(bed.hits.load(Ordering::SeqCst), 1);
    let after: BTreeMap<String, String> = bed.redis.hgetall(&balance_key).await?;
    assert_eq!(
        after, before,
        "failed closure destroyed the recovery record or moved funds"
    );
    let corrupt: String = bed.redis.hget(&key, "invalid").await?;
    assert_eq!(corrupt, "counter");
    let rows: Vec<(i16, i16, i64)> =
        sqlx::query_as("SELECT status,pool,amount_micro FROM billing_records WHERE request_id=$1")
            .bind(id)
            .fetch_all(&bed.pg)
            .await?;
    if failed_upstream {
        assert_eq!(
            rows,
            vec![(40, i16::from(subscription), 0)],
            "failure metadata must preserve the actual funding pool"
        );
    } else {
        assert_eq!(
            rows,
            vec![(20, i16::from(subscription), 24)],
            "completed usage must survive a Redis failure in the durable ledger"
        );
    }
    recover_and_check(
        &bed,
        id,
        held[0].amount.as_micros(),
        subscription,
        failed_upstream,
        &before,
    )
    .await
}

async fn recover_and_check(
    bed: &Bed,
    id: Uuid,
    reserved: i64,
    subscription: bool,
    failed_upstream: bool,
    before: &BTreeMap<String, String>,
) -> TestResult {
    let balance_key = format!("bal:{{{}}}", bed.uid);
    let key = format!("conc:{{{}}}:k:{}", bed.uid, bed.kid);
    bed.redis.del::<(), _>(&key).await?;
    bed.redis
        .set::<(), _, _>(&key, "1", None, None, false)
        .await?;
    if failed_upstream {
        let future = chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::minutes(11))
            .ok_or("test time overflow")?;
        let recovered =
            okapi::worker::sweep_expired_reservations(&bed.pg, &bed.ledger, future).await?;
        assert!(
            recovered.iter().any(|r| r.request_id == id
                && r.action == "refund"
                && r.released_micro == reserved)
        );
        assert!(
            !okapi::worker::sweep_expired_reservations(&bed.pg, &bed.ledger, future)
                .await?
                .iter()
                .any(|r| r.request_id == id)
        );
    } else {
        let future = chrono::Utc::now() + chrono::TimeDelta::minutes(11);
        // A new ledger instance has no request-local usage: the worker must recover
        // the actual charge from PG, never refund this successful request.
        let recovered_ledger = okapi_ledger::BalanceLedger::new(bed.redis.clone());
        okapi::worker::sweep_expired_reservations(&bed.pg, &recovered_ledger, future).await?;
        okapi::worker::sweep_expired_reservations(&bed.pg, &recovered_ledger, future).await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM billing_events WHERE request_id=$1 AND event_type='commit'",
        )
        .bind(id)
        .fetch_one(&bed.pg)
        .await?;
        assert_eq!(count, 1);
    }
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    let concurrency: i64 = bed.redis.get(&key).await?;
    assert_eq!(concurrency, 0);
    let field = if subscription { "sub" } else { "avail" };
    let balance: i64 = bed.redis.hget(&balance_key, field).await?;
    assert_eq!(
        balance,
        before[field].parse::<i64>()? + reserved - if failed_upstream { 0 } else { 24 }
    );
    let other = if subscription { "avail" } else { "sub" };
    let unchanged: Option<String> = bed.redis.hget(&balance_key, other).await?;
    assert_eq!(unchanged.as_ref(), before.get(other));
    Ok(())
}

#[tokio::test]
async fn successful_http_keeps_durable_usage_for_worker_recovery() -> TestResult {
    for subscription in [false, true] {
        interrupted(subscription, false, "/v1/chat/completions").await?;
    }
    Ok(())
}

#[tokio::test]
async fn failed_http_refund_preserves_pool_and_worker_can_recover_once() -> TestResult {
    for subscription in [false, true] {
        interrupted(subscription, true, "/v1/chat/completions").await?;
    }
    Ok(())
}

#[tokio::test]
async fn embeddings_and_rerank_deferred_refunds_keep_the_subscription_pool() -> TestResult {
    for endpoint in ["/v1/embeddings", "/v1/rerank"] {
        interrupted(true, true, endpoint).await?;
    }
    Ok(())
}

#[tokio::test]
async fn repeated_http_refunds_preserve_the_original_subscription_pool() -> TestResult {
    for endpoint in ["/v1/chat/completions", "/v1/embeddings", "/v1/rerank"] {
        let bed = Arc::new(Bed::new(true).await?);
        let balance_key = format!("bal:{{{}}}", bed.uid);
        let initial: BTreeMap<String, String> = bed.redis.hgetall(&balance_key).await?;
        bed.gate.mode.store(2, Ordering::SeqCst);
        let request = {
            let bed = bed.clone();
            tokio::spawn(async move { bed.generate(endpoint).await })
        };
        tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
        let held = bed.ledger.list_reservations(bed.uid).await?;
        assert_eq!(held.len(), 1);
        let id = held[0].request_id;
        let refunded = bed.ledger.refund(bed.uid, bed.kid, id).await?;
        assert_eq!(refunded.pool, okapi_ledger::Pool::Subscription);
        bed.gate.release.notify_one();
        let (status, body) = tokio::time::timeout(Duration::from_secs(5), request).await???;
        assert_eq!(
            status,
            if endpoint == "/v1/chat/completions" {
                400
            } else {
                502
            },
            "{body}"
        );
        bed.pending.wait_idle(Duration::from_secs(5)).await;
        assert_eq!(bed.pending.in_flight(), 0);
        let rows: Vec<(i16, i16, i64)> = sqlx::query_as(
            "SELECT status,pool,amount_micro FROM billing_records WHERE request_id=$1",
        )
        .bind(id)
        .fetch_all(&bed.pg)
        .await?;
        assert_eq!(rows, vec![(40, 1, 0)], "{endpoint}: original funding pool");
        let after: BTreeMap<String, String> = bed.redis.hgetall(&balance_key).await?;
        assert_eq!(after, initial, "repeated refund must not credit again");
        let concurrency: i64 = bed
            .redis
            .get(format!("conc:{{{}}}:k:{}", bed.uid, bed.kid))
            .await?;
        assert_eq!(concurrency, 0);
        assert_eq!(bed.hits.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

#[tokio::test]
async fn json_success_waits_for_a_durable_bill() -> TestResult {
    let bed = Arc::new(Bed::new(false).await?);
    bed.gate.mode.store(1, Ordering::SeqCst);
    let mut request = {
        let bed = bed.clone();
        tokio::spawn(async move { bed.chat().await })
    };
    tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
    let guard = okapi_ledger::holds::UserGuard::acquire(&bed.pg, bed.uid).await?;
    bed.gate.release.notify_one();
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut request)
            .await
            .is_err(),
        "JSON success must not escape while its durable write is blocked"
    );
    drop(guard);
    let (status, body) = tokio::time::timeout(Duration::from_secs(5), request).await???;
    assert_eq!(status, 200, "{body}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM billing_records WHERE user_id=$1 AND status=20")
            .bind(bed.uid)
            .fetch_one(&bed.pg)
            .await?;
    assert_eq!(
        count, 1,
        "bill must already exist when the client gets success"
    );
    Ok(())
}

#[tokio::test]
async fn pg_rejection_is_an_http_error_without_losing_or_charging_the_hold() -> TestResult {
    let bed = Bed::new(false).await?;
    let rule = format!("test_no_success_{}", bed.uid);
    // Per-user constraint injects a real PG failure only for this request's
    // success bill; failure logging and unrelated tests remain available.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_records ADD CONSTRAINT {rule} CHECK(user_id<>{} OR status<>20)",
        bed.uid
    )))
    .execute(&bed.pg)
    .await?;
    let result = bed.chat().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE billing_records DROP CONSTRAINT {rule}"
    )))
    .execute(&bed.pg)
    .await?;
    let (status, body) = result?;
    assert_eq!(status, 500, "{body}");
    assert_eq!(body["error"]["code"], "internal_error");
    bed.pending.wait_idle(Duration::from_secs(5)).await;
    assert_eq!(bed.ledger.balance(bed.uid).await?.as_micros(), 10_000_000);
    assert!(bed.ledger.list_reservations(bed.uid).await?.is_empty());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM billing_sync WHERE user_id=$1")
        .bind(bed.uid)
        .fetch_one(&bed.pg)
        .await?;
    assert_eq!(count, 0);
    assert_eq!(
        bed.hits.load(Ordering::SeqCst),
        1,
        "settlement failure must not retry generation"
    );
    let (status, body) = bed.chat().await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        bed.ledger.balance(bed.uid).await?.as_micros(),
        10_000_000 - 24
    );
    Ok(())
}

#[tokio::test]
async fn sse_streams_content_but_waits_for_durable_usage_before_done() -> TestResult {
    use futures::StreamExt;
    let bed = Arc::new(Bed::new(false).await?);
    // This test asserts the mock's exact reported usage. Untrusted-channel
    // recounting is tested separately and may legitimately raise the output.
    sqlx::query("UPDATE channels SET trust_upstream_usage=true WHERE name=$1")
        .bind(&bed.model)
        .execute(&bed.pg)
        .await?;
    bed.gate.mode.store(3, Ordering::SeqCst);
    let request = {
        let bed = bed.clone();
        tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://{}/v1/chat/completions", bed.address))
                .bearer_auth(&bed.token)
                .json(&json!({"model":bed.model,"stream":true,
                    "messages":[{"role":"user","content":"hello"}]}))
                .send()
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), bed.gate.entered.notified()).await?;
    let guard = okapi_ledger::holds::UserGuard::acquire(&bed.pg, bed.uid).await?;
    bed.gate.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), request).await???;
    assert_eq!(response.status(), 200);
    let mut stream = response.bytes_stream();
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !text.contains("hello") {
            let chunk = stream.next().await.ok_or("stream ended before content")??;
            text.push_str(&String::from_utf8(chunk.to_vec())?);
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    assert!(!text.contains("[DONE]"));
    // Drain any usage frame already sent, but no completion may be acknowledged
    // while the PG commit is blocked by this real advisory lock.
    let before = tokio::time::timeout(Duration::from_millis(150), async {
        while let Some(chunk) = stream.next().await {
            text.push_str(&String::from_utf8(chunk?.to_vec())?);
            if text.contains("[DONE]") {
                return Ok(true);
            }
        }
        Ok::<bool, Box<dyn std::error::Error + Send + Sync>>(false)
    })
    .await;
    assert!(
        before.is_err(),
        "stream must stay open until durable settlement"
    );
    drop(guard);
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            text.push_str(&String::from_utf8(chunk?.to_vec())?);
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    assert_eq!(text.matches("[DONE]").count(), 1);
    let charged: (i32,i32,i64) = sqlx::query_as(
        "SELECT prompt_tokens,completion_tokens,amount_micro FROM billing_records WHERE user_id=$1 AND status=20",
    )
    .bind(bed.uid)
    .fetch_one(&bed.pg)
    .await?;
    assert_eq!(charged, (10, 2, 24));
    Ok(())
}
