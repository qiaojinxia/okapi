use super::token_detail_aggregates::{key_id, request};
use super::ttft_statistics::{insert, row};
use super::{Env, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn character_duration_does_not_dilute_token_output_speed() {
    let database = format!("okapi_output_rate_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_mixed(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn output_rate_upgrade_recovers_raw_once_and_preserves_partial_history_after_expiry() {
    for legacy_dimensions in [false, true] {
        let database = format!("okapi_rate_upgrade_{}", Uuid::new_v4().simple());
        let env = setup_with_ch_database(&database).await;
        let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
        ch.ensure_schema().await.unwrap();
        let result = std::panic::AssertUnwindSafe(check_upgrade(&env, ch, legacy_dimensions))
            .catch_unwind()
            .await;
        ch.execute(&format!("DROP DATABASE {database} SYNC"))
            .await
            .unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}

async fn check_upgrade(env: &Env, ch: &ChClient, legacy_dimensions: bool) {
    let key = key_id(env).await;
    ch.execute("DROP TABLE mv_output_rate_5min SYNC")
        .await
        .unwrap();
    if legacy_dimensions {
        ch.execute("DROP TABLE mv_analysis_hour SYNC")
            .await
            .unwrap();
    }
    let mut old = sample(env, key, "tokens", 200, 1000);
    old["node"] = json!("rate-legacy");
    let mut speech = sample(env, key, "characters", 0, 9000);
    speech["node"] = json!("rate-legacy");
    insert(ch, &[old, speech]).await;
    ch.ensure_schema().await.unwrap();
    let mut new = sample(env, key, "tokens", 300, 3000);
    new["node"] = json!("rate-modern");
    insert(ch, &[new]).await;
    check_upgrade_views(env, key, false).await;
    ch.execute(
        "ALTER TABLE request_log_raw DELETE WHERE node='rate-legacy' SETTINGS mutations_sync=1",
    )
    .await
    .unwrap();
    check_upgrade_views(env, key, true).await;
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_upgrade_views(env, key, true).await;
}

async fn check_upgrade_views(env: &Env, key: i64, partial: bool) {
    let base = format!("/admin/stats/trend?days=2&user_id={}", env.user_id);
    let trend = request(env, &base, false).await;
    assert_eq!(trend["total"]["amount_micro"], 3000);
    assert_eq!(trend["total"]["tokens"], 500);
    assert_eq!(trend["total"]["avg_latency_ms"], 4333);
    let mut rows = vec![trend["total"].clone()];
    let portal = request(env, "/api/me/stats/breakdown?days=2&scope=user", true).await;
    rows.push(portal["total"].clone());
    for (kind, id) in [("user", env.user_id), ("api_key", key)] {
        let entity = request(
            env,
            &format!("/admin/stats/entity-usage?kind={kind}&ids={id}&days=2"),
            false,
        )
        .await;
        rows.push(entity["data"][id.to_string()].clone());
    }
    for path in [
        "/api/me/stats/daily?days=2".to_owned(),
        "/api/me/stats/activity?scope=user".to_owned(),
        "/admin/stats/models?days=2&limit=100".to_owned(),
        "/admin/stats/channels?days=2&limit=100".to_owned(),
        format!("/admin/stats/channels/{}/timeline?hours=48", env.channel_id),
    ] {
        let result = request(env, &path, path.starts_with("/api/")).await;
        rows.push(
            result["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| {
                    row["model"] == env.model
                        || row["channel_id"] == env.channel_id
                        || row["requests"] == 3
                })
                .unwrap_or_else(|| panic!("{path}: {result}"))
                .clone(),
        );
    }
    let expected = if partial {
        [3, 1, 1, 1, 3000, 300]
    } else {
        [3, 3, 2, 2, 4000, 500]
    };
    for row in rows {
        rate(
            &row,
            expected,
            if partial { None } else { Some(125_000) },
            Some(if partial { 100_000 } else { 125_000 }),
        );
        assert_eq!(
            row["output_tps_observed_requests"],
            if partial { 1 } else { 3 },
            "{row}"
        );
        assert_eq!(
            row["output_tps_history_coverage_bp"],
            if partial { 3333 } else { 10_000 },
            "{row}"
        );
        assert_eq!(row["output_tps_history_complete"], !partial, "{row}");
    }
    let new = request(env, &format!("{base}&node=rate-modern"), false).await;
    rate(
        &new["total"],
        [1, 1, 1, 1, 3000, 300],
        Some(100_000),
        Some(100_000),
    );
}

fn sample(env: &Env, key: i64, unit: &str, output: i64, duration: i64) -> Value {
    let mut value = row(env, json!(0), false);
    value["api_key_id"] = json!(key);
    value["prompt_tokens"] = json!(0);
    value["completion_tokens"] = json!(output);
    value["cached_tokens"] = json!(0);
    value["cache_write_tokens"] = json!(0);
    value["reasoning_tokens"] = json!(0);
    value["latency_ms"] = json!(duration);
    value["latency_reported"] = json!(1);
    value["input_unit"] = json!(unit);
    value["input_characters"] = if unit == "characters" {
        json!(11)
    } else {
        Value::Null
    };
    value
}

async fn check_mixed(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let tokens = sample(env, key, "tokens", 200, 1000);
    let mut speech = sample(env, key, "characters", 0, 9000);
    speech["endpoint"] = json!("/v1/audio/speech");
    insert(ch, &[tokens, speech]).await;
    let trend = request(
        env,
        &format!("/admin/stats/trend?days=2&user_id={}", env.user_id),
        false,
    )
    .await;
    assert_eq!(trend["total"]["requests"], 2);
    assert_eq!(trend["total"]["completion_tokens"], 200);
    assert_eq!(trend["total"]["amount_micro"], 2000);
    assert_eq!(trend["total"]["avg_latency_ms"], 5000);
    assert_eq!(trend["total"]["tokens_per_1k_sec"], 200_000);
    let mut other = sample(env, key, "tokens", 900, 3000);
    other["model"] = json!(format!("{}-other'\\t", env.model));
    let mut zero = sample(env, key, "tokens", 0, 0);
    zero["node"] = json!("zero");
    let mut empty = sample(env, key, "tokens", 0, 1000);
    empty["node"] = json!("empty");
    empty["is_error"] = json!(1);
    empty["amount_micro"] = json!(0);
    let mut missing = sample(env, key, "tokens", 10_000, 0);
    missing["latency_reported"] = json!(0);
    missing["node"] = json!("missing");
    insert(ch, &[other, zero, empty, missing]).await;
    let mut seeded = std::collections::HashSet::new();
    seed_pg(env, ch, key, &mut seeded).await;
    check_views(env, key, false).await;
    check_logs(env, false).await;
    check_quality(env, false).await;
    check_filters(env).await;
    check_pg_states(env).await;
    let mut unknown = sample(env, key, "", 9000, 9000);
    unknown["node"] = json!("unknown");
    let invalid = sample(env, key, "characters", 1, 5000);
    let mut outside = sample(env, key + 1_000_000, "tokens", 900_000, 1);
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["channel_id"] = json!(0);
    outside["model"] = json!("outside-owner");
    insert(ch, &[unknown, invalid, outside]).await;
    seed_pg(env, ch, key, &mut seeded).await;
    check_views(env, key, true).await;
    check_logs(env, true).await;
    ch.ensure_schema().await.unwrap();
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_views(env, key, true).await;
    check_quality(env, true).await;
    check_filters(env).await;
}

fn rate(value: &Value, expected: [i64; 6], full: Option<i64>, subset: Option<i64>) {
    for (field, count) in [
        "requests",
        "output_tps_unit_known_requests",
        "output_tps_token_requests",
        "output_tps_samples",
        "output_tps_duration_ms",
        "output_tps_completion_tokens",
    ]
    .into_iter()
    .zip(expected)
    {
        let actual = if field == "requests" && value.get(field).is_none() {
            &value["records"]
        } else {
            &value[field]
        };
        assert_eq!(actual, count, "{field}: {value}");
    }
    assert_eq!(value["tokens_per_1k_sec"], json!(full), "{value}");
    assert_eq!(value["avg_output_tps_milli"], json!(full), "{value}");
    assert_eq!(value["observed_output_tps_milli"], json!(subset), "{value}");
    assert_eq!(
        value["output_tps_unknown_unit_requests"],
        expected[0] - expected[1],
        "{value}"
    );
    assert_eq!(
        value["output_tps_unit_coverage_bp"],
        expected[1] * 10_000 / expected[0],
        "{value}"
    );
    assert_eq!(
        value["output_tps_unit_complete"],
        expected[0] == expected[1],
        "{value}"
    );
    assert_eq!(
        value["output_tps_sample_coverage_bp"],
        if expected[2] > 0 {
            json!(expected[3] * 10_000 / expected[2])
        } else {
            Value::Null
        },
        "{value}"
    );
    assert_eq!(
        value["output_tps_basis"],
        "settled_token_output_per_measured_token_request_latency"
    );
}

async fn check_views(env: &Env, key: i64, partial: bool) {
    let (requests, amount, tokens, latency, full) = if partial {
        (8, 7000, 20_101, 4000, None)
    } else {
        (6, 5000, 11_100, 2800, Some(220_000))
    };
    let trend = request(
        env,
        &format!("/admin/stats/trend?days=2&user_id={}", env.user_id),
        false,
    )
    .await;
    assert_eq!(trend["total"]["amount_micro"], amount);
    assert_eq!(trend["total"]["tokens"], tokens);
    assert_eq!(trend["total"]["avg_latency_ms"], latency);
    rate(
        &trend["total"],
        [requests, 6, 5, 4, 5000, 1100],
        full,
        Some(220_000),
    );
    for by in ["provider", "user", "channel", "group"] {
        let result = request(
            env,
            &format!(
                "/admin/stats/breakdown?days=2&user_id={}&by={by}",
                env.user_id
            ),
            false,
        )
        .await;
        rate(
            &result["data"][0],
            [requests, 6, 5, 4, 5000, 1100],
            full,
            Some(220_000),
        );
    }
    for (kind, id) in [("user", env.user_id), ("api_key", key)] {
        let result = request(
            env,
            &format!("/admin/stats/entity-usage?kind={kind}&ids={id}&days=2"),
            false,
        )
        .await;
        rate(
            &result["data"][id.to_string()],
            [requests, 6, 5, 4, 5000, 1100],
            full,
            Some(220_000),
        );
    }
    for scope in ["key", "user"] {
        let result = request(
            env,
            &format!("/api/me/stats/breakdown?days=2&scope={scope}"),
            true,
        )
        .await;
        rate(
            &result["total"],
            [requests, 6, 5, 4, 5000, 1100],
            full,
            Some(220_000),
        );
    }
    for path in [
        "/api/me/stats/daily?days=2",
        "/api/me/stats/activity?scope=user",
    ] {
        let result = request(env, path, true).await;
        let values = result["data"].as_array().unwrap();
        let own = values.iter().find(|r| r["model"] == env.model).unwrap();
        rate(
            own,
            [requests - 1, 5, 4, 3, 2000, 200],
            if partial { None } else { Some(100_000) },
            Some(100_000),
        );
    }
    let stacked = request(
        env,
        &format!(
            "/admin/stats/trend?days=2&user_id={}&stack=model&limit=1",
            env.user_id
        ),
        false,
    )
    .await;
    rate(
        &stacked["data"][0]["values"]["__other"],
        [1, 1, 1, 1, 3000, 900],
        Some(300_000),
        Some(300_000),
    );
}

async fn check_filters(env: &Env) {
    let base = format!("/admin/stats/trend?days=2&user_id={}", env.user_id);
    for (node, ms, samples, speed) in [
        ("empty", 1000, 1, Some(0)),
        ("zero", 0, 1, None),
        ("missing", 0, 0, None),
    ] {
        let result = request(env, &format!("{base}&node={node}"), false).await;
        rate(&result["total"], [1, 1, 1, samples, ms, 0], speed, speed);
    }
    let speech = request(
        env,
        &format!("{base}&endpoint=%2Fv1%2Faudio%2Fspeech"),
        false,
    )
    .await;
    rate(&speech["total"], [1, 1, 0, 0, 0, 0], None, None);
}

async fn check_quality(env: &Env, partial: bool) {
    for (path, key, id, counts, subset) in [
        (
            "/admin/stats/models?days=2&limit=100".to_owned(),
            "model",
            json!(env.model),
            [if partial { 7 } else { 5 }, 5, 4, 3, 2000, 200],
            100_000,
        ),
        (
            "/admin/stats/channels?days=2&limit=100".to_owned(),
            "channel_id",
            json!(env.channel_id),
            [if partial { 8 } else { 6 }, 6, 5, 4, 5000, 1100],
            220_000,
        ),
        (
            format!("/admin/stats/channels/{}/timeline?hours=48", env.channel_id),
            "requests",
            json!(if partial { 8 } else { 6 }),
            [if partial { 8 } else { 6 }, 6, 5, 4, 5000, 1100],
            220_000,
        ),
    ] {
        let result = request(env, &path, false).await;
        let own = result["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[key] == id)
            .unwrap_or_else(|| panic!("{result}"));
        rate(
            own,
            counts,
            if partial { None } else { Some(subset) },
            Some(subset),
        );
    }
}

async fn check_logs(env: &Env, partial: bool) {
    for (path, portal) in [
        (
            format!("/admin/logs/stat?hours=48&user_id={}", env.user_id),
            false,
        ),
        ("/api/me/logs/stat?scope=user".to_owned(), true),
        ("/api/me/logs/stat".to_owned(), true),
    ] {
        let result = request(env, &path, portal).await;
        rate(
            &result,
            [if partial { 8 } else { 6 }, 6, 5, 4, 5000, 1100],
            if partial { None } else { Some(220_000) },
            Some(220_000),
        );
    }
}

async fn check_pg_states(env: &Env) {
    sqlx::query("UPDATE billing_records SET status=CASE completion_tokens WHEN 900 THEN 10 ELSE 30 END WHERE user_id=$1 AND completion_tokens IN (900,200)")
        .bind(env.user_id).execute(&env.pg).await.unwrap();
    let result = request(env, "/api/me/logs/stat?scope=user", true).await;
    rate(
        &result,
        [6, 6, 5, 3, 2000, 200],
        Some(100_000),
        Some(100_000),
    );
    assert_eq!(result["pending"], 1);
    assert_eq!(result["refunded"], 1);
    assert_eq!(result["failed"], 1);
    assert_eq!(result["amount_micro"], 3000);
    assert_eq!(result["refunded_amount_micro"], 1000);
    sqlx::query("UPDATE billing_records SET status=20 WHERE user_id=$1 AND status IN (10,30)")
        .bind(env.user_id)
        .execute(&env.pg)
        .await
        .unwrap();
}

async fn seed_pg(env: &Env, ch: &ChClient, key: i64, seeded: &mut std::collections::HashSet<Uuid>) {
    let rows = ch
        .query_json_each_row(&format!(
            "SELECT * FROM request_log_raw WHERE user_id={}",
            env.user_id
        ))
        .await
        .unwrap();
    for item in rows {
        let id = Uuid::parse_str(item["request_id"].as_str().unwrap()).unwrap();
        if seeded.contains(&id) {
            continue;
        }
        let amount = item["amount_micro"].as_i64().unwrap_or_else(|| {
            item["amount_micro"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()
        });
        let duration = if item["latency_reported"] == 1 {
            item["latency_ms"]
                .as_i64()
                .map(|n| i32::try_from(n).unwrap())
        } else {
            None
        };
        sqlx::query("INSERT INTO billing_records(request_id,user_id,api_key_id,model_name,status,latency_ms,completion_tokens,amount_micro,usage_details) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(id).bind(env.user_id).bind(key).bind(item["model"].as_str().unwrap())
            .bind(if item["is_error"]==1 {40_i16} else {20}).bind(duration).bind(i32::try_from(item["completion_tokens"].as_i64().unwrap()).unwrap()).bind(amount)
            .bind(json!({"input_unit":item["input_unit"],"input_characters":item["input_characters"]}))
            .execute(&env.pg).await.unwrap();
        seeded.insert(id);
    }
}
