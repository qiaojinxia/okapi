//! Real gateway settlement of mixed cache lifetimes, including protocol conversion.
use super::{Env, Protocol, anthropic_usage, record, report, request, setup_with_pricing};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::time::Duration;

fn mixed_usage() -> Value {
    let mut usage = anthropic_usage::fixture();
    usage["cache_creation"] =
        json!({"ephemeral_5m_input_tokens":60,"ephemeral_1h_input_tokens":40});
    usage
}

fn cumulative(final_usage: &Value) -> Value {
    json!({"final":final_usage,
        "start":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":20},
        "updates":[{"output_tokens":10},final_usage,final_usage,null]})
}

fn integer(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or_else(|| panic!("not an integer: {value}"))
}

async fn verify(
    env: &Env,
    amount: i64,
    ttl: Option<(i64, i64)>,
    effective_rates: &Value,
    pg_balance_before: i64,
) {
    let row = record(env).await;
    assert_eq!(row["amount_micro"], amount, "{row}");
    assert_eq!(row["original_amount_micro"], amount);
    assert_eq!(row["discount_micro"], 0);
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - amount
    );
    let (balance, used, count): (i64, i64, i64) = sqlx::query_as(
        "SELECT u.balance_micro,k.used_micro,(SELECT count(*) FROM billing_records b WHERE b.user_id=u.id) FROM users u JOIN api_keys k ON k.user_id=u.id WHERE u.id=$1",
    )
    .bind(env.user)
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    assert_eq!(balance, pg_balance_before - amount);
    assert_eq!(used, amount);
    assert_eq!(count, 1, "one actual call must produce one settlement");
    for (name, expected) in [
        ("prompt_tokens", 1000),
        ("completion_tokens", 50),
        ("cached_tokens", 800),
        ("cache_write_tokens", 100),
        ("reasoning_tokens", 20),
    ] {
        assert_eq!(row["usage"][name], expected, "{name}: {row}");
    }
    let (short, long) = ttl.map_or((Value::Null, Value::Null), |(short, long)| {
        (json!(short), json!(long))
    });
    assert_eq!(row["usage"]["cache_write_5m_tokens"], short);
    assert_eq!(row["usage"]["cache_write_1h_tokens"], long);
    let request_id = row["request_id"].as_str().unwrap();
    let (snapshot, epoch, cost): (Value, i64, Option<i64>) = sqlx::query_as(
        "SELECT pricing_snapshot,pricing_epoch,upstream_cost_micro FROM billing_records WHERE request_id::text=$1",
    )
    .bind(request_id)
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    assert_eq!(epoch, env.state.pricebook.epoch());
    assert_eq!(snapshot["modality_ratios"], *effective_rates, "{snapshot}");
    assert_eq!(
        snapshot["cache_write_ratio"],
        serde_json::from_str::<Value>("1.25").unwrap()
    );
    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1",
    )
    .bind(request_id)
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    for name in [
        "prompt_tokens",
        "completion_tokens",
        "cached_tokens",
        "cache_write_tokens",
        "cache_write_5m_tokens",
        "cache_write_1h_tokens",
        "reasoning_tokens",
    ] {
        assert_eq!(payload[name], row["usage"][name], "{name}: {payload}");
    }
    for name in ["amount_micro", "original_amount_micro", "discount_micro"] {
        assert_eq!(payload[name], row[name]);
    }
    assert_eq!(payload["upstream_cost_micro"], json!(cost.unwrap_or(0)));
    assert_eq!(payload["upstream_cost_known"], json!(cost.is_some()));
    assert_eq!(
        serde_json::from_str::<Value>(payload["ratio_snapshot"].as_str().unwrap()).unwrap(),
        snapshot
    );

    verify_delivery(env, &payload, &snapshot, ttl).await;
    let stats = report(env, "/api/me/stats/breakdown?days=1").await;
    assert_eq!(stats["total"]["requests"], 1, "{stats}");
    assert_eq!(stats["total"]["tokens"], 1050);
    assert_eq!(stats["total"]["amount_micro"], amount);
    assert_eq!(stats["total"]["cache_hit_bp"], 8000);
    let stat = report(env, "/api/me/logs/stat").await;
    assert_eq!(stat["cache_write_5m_tokens"], short);
    assert_eq!(stat["cache_write_1h_tokens"], long);
    assert_eq!(stat["cache_write_ttl_samples"], i64::from(ttl.is_some()));
    assert_observed_ttl(&stat, ttl);
    assert_eq!(env.calls.load(Ordering::SeqCst), 1);
}

fn assert_observed_ttl(stat: &Value, ttl: Option<(i64, i64)>) {
    for (name, count) in [
        ("cache_write_5m_tokens", ttl.map(|(short, _)| short)),
        ("cache_write_1h_tokens", ttl.map(|(_, long)| long)),
    ] {
        let observed = &stat["token_detail_observations"][name];
        assert_eq!(observed["observed_records"], i64::from(count.is_some()));
        assert_eq!(observed["tokens"], json!(count));
        assert_eq!(
            observed["coverage_bp"],
            if count.is_some() { 10000 } else { 0 }
        );
    }
}

async fn verify_delivery(env: &Env, payload: &Value, snapshot: &Value, ttl: Option<(i64, i64)>) {
    let request_id = payload["request_id"].as_str().unwrap();
    let (short, long) = ttl.map_or((Value::Null, Value::Null), |(short, long)| {
        (json!(short), json!(long))
    });
    let ch = env.state.ch.as_ref().expect("isolated CH is required");
    ch.ensure_schema().await.unwrap();
    for _ in 0..100 {
        if okapi::worker::chsink::process_once(&env.state.pg, ch)
            .await
            .unwrap()
            == 0
        {
            break;
        }
    }
    let mut rows = Vec::new();
    for _ in 0..50 {
        rows = ch.query_with_params(
            "SELECT prompt_tokens,completion_tokens,cached_tokens,cache_write_tokens,cache_write_5m_tokens,cache_write_1h_tokens,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,upstream_cost_known,ratio_snapshot FROM request_log_raw WHERE request_id={request:UUID}",
            &[("request", request_id)],
        ).await.unwrap();
        if !rows.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rows.len(), 1, "CH must contain exactly this call");
    let delivered = &rows[0];
    for name in [
        "prompt_tokens",
        "completion_tokens",
        "cached_tokens",
        "cache_write_tokens",
        "amount_micro",
        "original_amount_micro",
        "discount_micro",
        "upstream_cost_micro",
    ] {
        assert_eq!(integer(&delivered[name]), integer(&payload[name]), "{name}");
    }
    assert_eq!(delivered["cache_write_5m_tokens"], short);
    assert_eq!(delivered["cache_write_1h_tokens"], long);
    assert_eq!(
        integer(&delivered["upstream_cost_known"]),
        i64::from(payload["upstream_cost_known"].as_bool().unwrap())
    );
    assert_eq!(
        serde_json::from_str::<Value>(delivered["ratio_snapshot"].as_str().unwrap()).unwrap(),
        *snapshot
    );
}

async fn matrix(usage: Value, rates: Value, amount: i64, effective: Value) {
    let ttl = usage["cache_creation"].as_object().map(|_| {
        (
            integer(&usage["cache_creation"]["ephemeral_5m_input_tokens"]),
            integer(&usage["cache_creation"]["ephemeral_1h_input_tokens"]),
        )
    });
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let env = setup_with_pricing(
                Protocol::Anthropic,
                cumulative(&usage),
                Some(("1.25", rates.clone())),
            )
            .await;
            let before: i64 = sqlx::query_scalar("SELECT balance_micro FROM users WHERE id=$1")
                .bind(env.user)
                .fetch_one(&env.state.pg)
                .await
                .unwrap();
            let response =
                request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, 200, "{ingress:?} stream={stream}: {body}");
            assert!(!body.contains("upstream_error"), "{body}");
            verify(&env, amount, ttl, &effective, before).await;
        }
    }
}

#[tokio::test]
async fn mixed_ttl_rates_replace_generic_charge_in_all_ingresses() {
    // Ordinary 200 + reads 800 + 5m writes 150 + 1h writes 160 + output 200.
    matrix(
        mixed_usage(),
        json!({"cache_write_5m":"1.25","cache_write_1h":"2"}),
        1510,
        serde_json::from_str(r#"{"cache_write_5m":1.25,"cache_write_1h":2}"#).unwrap(),
    )
    .await;
}

#[tokio::test]
async fn unreported_ttl_retains_generic_rate_and_unknown_counters() {
    let mut usage = mixed_usage();
    usage.as_object_mut().unwrap().remove("cache_creation");
    matrix(
        usage,
        json!({"cache_write_5m":"1.25","cache_write_1h":"2"}),
        1450,
        Value::Null,
    )
    .await;
}

#[tokio::test]
async fn reported_ttl_without_overrides_preserves_generic_price() {
    matrix(mixed_usage(), json!({}), 1450, Value::Null).await;
}

#[tokio::test]
async fn zero_ttl_prices_do_not_fall_back_or_double_charge() {
    matrix(
        mixed_usage(),
        json!({"cache_write_5m":"0","cache_write_1h":"0"}),
        1200,
        json!({"cache_write_5m":0,"cache_write_1h":0}),
    )
    .await;
}

#[tokio::test]
async fn single_ttl_override_only_replaces_that_lifetime() {
    matrix(
        mixed_usage(),
        json!({"cache_write_1h":"2"}),
        1510,
        json!({"cache_write_1h":2}),
    )
    .await;
}

#[tokio::test]
async fn contradictory_ttl_snapshots_refund_and_cannot_be_healed_or_replayed() {
    let valid = mixed_usage();
    let mut invalid = valid.clone();
    invalid["cache_creation"]["ephemeral_1h_input_tokens"] = json!(41);
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            let mock = json!({"final":invalid,"start":{"input_tokens":100,"output_tokens":1},
                "updates":[invalid,valid,valid]});
            let env = setup_with_pricing(
                Protocol::Anthropic,
                mock,
                Some((
                    "1.25",
                    json!({"cache_write_5m":"1.25","cache_write_1h":"2"}),
                )),
            )
            .await;
            let response =
                request(&env, ingress, stream, matches!(ingress, Protocol::Gemini)).await;
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(
                status,
                if stream { 200 } else { 502 },
                "{ingress:?}: {body}"
            );
            assert!(body.contains("upstream_error"), "{body}");
            let row = record(&env).await;
            assert_eq!(row["amount_micro"], 0, "{row}");
            assert_eq!(
                env.state
                    .ledger
                    .balance(env.user)
                    .await
                    .unwrap()
                    .as_micros(),
                50_000_000
            );
            let (used, count): (i64, i64) = sqlx::query_as(
                "SELECT used_micro,(SELECT count(*) FROM billing_records WHERE user_id=$1) FROM api_keys WHERE user_id=$1",
            ).bind(env.user).fetch_one(&env.state.pg).await.unwrap();
            assert_eq!((used, count), (0, 1));
            assert_eq!(env.calls.load(Ordering::SeqCst), 1);
        }
    }
}
