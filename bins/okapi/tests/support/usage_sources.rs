use super::ttft_statistics::{insert, row};
use super::{Env, get, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

async fn request(env: &Env, path: &str, portal: bool) -> Value {
    let (status, body) = get(
        env,
        path,
        if portal {
            &env.user_token
        } else {
            &env.super_token
        },
    )
    .await;
    assert_eq!(status, 200, "{path}: {body}");
    body
}

fn sample(env: &Env, input: i64, output: i64, cached: i64, state: &str) -> Value {
    let mut value = row(env, json!(10), true);
    value["prompt_tokens"] = json!(input);
    value["completion_tokens"] = json!(output);
    value["cached_tokens"] = json!(cached);
    value["cache_read_reported"] = json!(1);
    value["prompt_source"] = json!(state);
    value["completion_source"] = json!(if state == "unknown" {
        "unknown"
    } else {
        "upstream"
    });
    value["upstream_prompt_tokens"] = match state {
        "upstream" => json!(input),
        "local_override" => json!(100),
        _ => Value::Null,
    };
    value["upstream_completion_tokens"] = if state == "unknown" {
        Value::Null
    } else {
        json!(output)
    };
    value["node"] = json!(state);
    value
}

fn check_mixed(value: &Value) {
    let provenance = &value["token_provenance"];
    assert_eq!(provenance["observed_requests"], 5, "{value}");
    assert_eq!(provenance["history_complete"], true);
    for (axis, state, n, tokens) in [
        ("prompt", "upstream", 2, 100),
        ("prompt", "estimated", 1, 900),
        ("prompt", "local_override", 1, 200),
        ("prompt", "unknown", 1, 300),
        ("completion", "upstream", 4, 100),
        ("completion", "unknown", 1, 100),
    ] {
        assert_eq!(
            provenance[axis][state]["requests"], n,
            "{axis}/{state}: {value}"
        );
        assert_eq!(provenance[axis][state]["tokens"], tokens, "{value}");
    }
    assert_eq!(provenance["prompt"]["estimated"]["request_share_bp"], 2000);
    assert_eq!(provenance["prompt"]["estimated"]["token_share_bp"], 6000);
    assert_eq!(value["measured_cache_hit_requests"], 2);
    assert_eq!(value["measured_cache_hit_coverage_bp"], 4000);
    assert_eq!(value["measured_cache_hit_bp"], 8000);
    assert_eq!(value["settled_cache_hit_bp"], 1466);
    assert!(
        value["cache_hit_bp"].is_null(),
        "estimated input is not measured: {value}"
    );
    assert_eq!(value["cache_hit_basis"], "upstream_prompt_tokens");
}

async fn totals(env: &Env) -> Vec<Value> {
    let trend = request(
        env,
        &format!("/admin/stats/trend?user_id={}&days=2", env.user_id),
        false,
    )
    .await;
    let portal = request(env, "/api/me/stats/breakdown?days=2&scope=user", true).await;
    let logs = request(
        env,
        &format!("/admin/logs/stat?user_id={}&hours=48", env.user_id),
        false,
    )
    .await;
    vec![trend["total"].clone(), portal["total"].clone(), logs]
}

async fn seed_pg(env: &Env, rows: &[Value], key: i64) {
    for value in rows {
        let detail = json!({"prompt_source":value["prompt_source"],"completion_source":value["completion_source"],
            "tokens":{"upstream_usage":{"prompt_tokens":value["upstream_prompt_tokens"],"completion_tokens":value["upstream_completion_tokens"]},"cache_read_reported":true}});
        sqlx::query("INSERT INTO billing_records(request_id,user_id,api_key_id,model_name,status,prompt_tokens,completion_tokens,cached_tokens,amount_micro,usage_details) VALUES($1,$2,$3,$4,20,$5,$6,$7,1000,$8)")
            .bind(Uuid::new_v4()).bind(env.user_id).bind(key).bind(value["model"].as_str().unwrap())
            .bind(i32::try_from(value["prompt_tokens"].as_i64().unwrap()).unwrap())
            .bind(i32::try_from(value["completion_tokens"].as_i64().unwrap()).unwrap())
            .bind(i32::try_from(value["cached_tokens"].as_i64().unwrap()).unwrap()).bind(detail).execute(&env.pg).await.unwrap();
    }
}

#[tokio::test]
async fn sources_cache_pairs_filters_and_weighted_folding_survive_retention() {
    let database = format!("okapi_sources_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_endpoints(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn seed_mixed(env: &Env, ch: &ChClient) {
    let me = request(env, "/api/me", true).await;
    let key = me["key_id"].as_i64().unwrap();
    let mut rows = vec![
        sample(env, 100, 20, 80, "upstream"),
        sample(env, 900, 80, 90, "estimated"),
        sample(env, 200, 0, 20, "local_override"),
        sample(env, 300, 100, 30, "unknown"),
        sample(env, 0, 0, 0, "upstream"),
    ];
    let timestamp = rows[0]["ts"].clone();
    for value in &mut rows {
        value["api_key_id"] = json!(key);
        value["ts"] = timestamp.clone();
    }
    for (index, value) in rows.iter_mut().enumerate().skip(2) {
        value["model"] = json!(format!("{}-other-{index}'\\t", env.model));
    }
    seed_pg(env, &rows, key).await;
    let mut other = sample(env, 10000, 10000, 9000, "upstream");
    other["user_id"] = json!(env.user_id + 1_000_000);
    other["channel_id"] = json!(0);
    other["model"] = json!("outside");
    let mut all = rows.clone();
    all.push(other);
    insert(ch, &all).await;
    ch.ensure_schema().await.unwrap();
}

async fn check_endpoints(env: &Env, ch: &ChClient) {
    seed_mixed(env, ch).await;
    for value in totals(env).await {
        check_mixed(&value);
    }
    let pg = request(env, "/api/me/logs/stat?scope=user&limit=1", true).await;
    check_mixed(&pg);
    assert_eq!(pg["records"], 5, "summary is independent of the page");
    assert_eq!(pg["amount_micro"], 5000);
    check_folding(env).await;
    check_retention(env, ch).await;
}

async fn check_folding(env: &Env) {
    for path in [
        format!(
            "/admin/stats/breakdown?user_id={}&by=provider&days=2",
            env.user_id
        ),
        format!(
            "/admin/stats/breakdown?user_id={}&by=user&days=2",
            env.user_id
        ),
    ] {
        let value = request(env, &path, false).await;
        check_mixed(&value["data"][0]);
    }
    let flow = request(
        env,
        &format!(
            "/admin/stats/flow?user_id={}&metric=tokens&days=2",
            env.user_id
        ),
        false,
    )
    .await;
    check_mixed(&flow["metrics"]);
    let selected = request(
        env,
        &format!(
            "/admin/stats/trend?user_id={}&node=upstream&days=2",
            env.user_id
        ),
        false,
    )
    .await;
    assert_eq!(selected["total"]["requests"], 2);
    assert_eq!(selected["total"]["cache_hit_bp"], 8000);
    assert_eq!(
        selected["total"]["token_provenance"]["prompt"]["unknown"]["requests"],
        0
    );
    let stacked = request(
        env,
        &format!(
            "/admin/stats/trend?user_id={}&stack=model&limit=1&metric=requests&days=2",
            env.user_id
        ),
        false,
    )
    .await;
    let other = &stacked["data"][0]["values"]["__other"];
    assert_eq!(other["requests"], 3, "{stacked}");
    assert_eq!(other["settled_cache_hit_bp"], 1000);
    assert_eq!(
        other["token_provenance"]["prompt"]["unknown"]["tokens"],
        300
    );
    assert_eq!(
        other["token_provenance"]["prompt"]["local_override"]["tokens"],
        200
    );
    assert!(
        other["measured_cache_hit_bp"].is_null(),
        "zero denominator is not a zero hit rate"
    );
}

async fn check_retention(env: &Env, ch: &ChClient) {
    let overview = request(env, "/admin/stats/overview?days=2", false).await;
    let total = &overview["window"];
    assert_eq!(total["requests"], 6, "{overview}");
    assert_eq!(total["tokens"], 21700);
    assert_eq!(total["token_provenance"]["history_complete"], true);
    assert_eq!(
        total["token_provenance"]["prompt"]["upstream"]["tokens"],
        10100
    );
    assert_eq!(
        total["token_provenance"]["prompt"]["unknown"]["tokens"],
        300
    );
    assert_eq!(total["measured_cache_hit_requests"], 3);
    assert_eq!(total["measured_cache_hit_coverage_bp"], 5000);
    assert_eq!(total["measured_cache_hit_bp"], 8990);
    assert!(total["cache_hit_bp"].is_null());
    // Same timestamp bucket, model and channel endpoint sources have the same measured subset.
    for path in [
        "/admin/stats/models?days=2&limit=100".to_owned(),
        "/admin/stats/channels?days=2&limit=100".to_owned(),
        format!("/admin/stats/channels/{}/timeline?hours=48", env.channel_id),
    ] {
        let value = request(env, &path, false).await;
        let first = &value["data"][0];
        assert!(first["token_provenance"].is_object(), "{path}: {value}");
        if path.contains("channels") {
            check_mixed(first);
        }
    }
    super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
        .await
        .unwrap();
    let retained = totals(env).await;
    for value in &retained[..2] {
        check_mixed(value);
    }
    let activity = request(env, "/api/me/stats/activity?scope=user", true).await;
    assert_eq!(
        activity["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["requests"].as_i64().unwrap())
            .sum::<i64>(),
        5
    );
    assert!(
        activity["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["token_provenance"]["history_complete"] == true)
    );
}

#[tokio::test]
async fn source_upgrade_recovers_raw_without_fabricating_legacy_measurement() {
    let database = format!("okapi_source_upgrade_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_upgrade(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_upgrade(env: &Env, ch: &ChClient) {
    check_empty_overview(env).await;
    super::population_storage::execute(ch, "DROP TABLE mv_usage_sources_5min SYNC")
        .await
        .unwrap();
    super::population_storage::execute(ch, "DROP TABLE mv_analysis_hour SYNC")
        .await
        .unwrap();
    let old = vec![
        sample(env, 100, 20, 80, "unknown"),
        sample(env, 100, 20, 80, "unknown"),
    ];
    insert(ch, &old).await;
    ch.ensure_schema().await.unwrap();
    insert(ch, &[sample(env, 100, 20, 80, "upstream")]).await;
    let before = totals(env).await;
    for value in &before[..2] {
        assert_eq!(
            value["token_provenance"]["history_complete"], true,
            "{value}"
        );
        assert_eq!(value["token_provenance"]["observed_requests"], 3);
        assert_eq!(
            value["token_provenance"]["prompt"]["unknown"]["requests"],
            2
        );
        assert_eq!(
            value["token_provenance"]["prompt"]["unknown"]["tokens"],
            200
        );
        assert_eq!(
            value["token_provenance"]["prompt"]["upstream"]["requests"],
            1
        );
        assert!(value["cache_hit_bp"].is_null());
        assert_eq!(value["measured_cache_hit_bp"], 8000);
    }
    super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
        .await
        .unwrap();
    let after = totals(env).await;
    for value in &after[..2] {
        assert_eq!(
            value["token_provenance"]["history_complete"], false,
            "{value}"
        );
        assert_eq!(value["token_provenance"]["observed_requests"], 1);
        assert_eq!(
            value["token_provenance"]["prompt"]["unknown"]["tokens"],
            200
        );
        assert_eq!(value["tokens"], 360);
        assert_eq!(value["amount_micro"], 3000);
        assert!(value["cache_hit_bp"].is_null());
    }
    let overview = request(env, "/admin/stats/overview?days=2", false).await;
    assert!(overview["window"]["token_provenance"].is_object());
    assert_eq!(
        overview["window"]["token_provenance"]["prompt"]["unknown"]["tokens"],
        Value::Null,
        "old summary cannot allocate unknown axes: {overview}"
    );
    // Inflate the new states: after raw expiry they cannot be advertised as complete.
    super::population_storage::execute(
        ch,
        "INSERT INTO mv_usage_sources_5min SELECT * FROM mv_usage_sources_5min",
    )
    .await
    .unwrap();
    let value = request(
        env,
        &format!(
            "/admin/stats/trend?user_id={}&node=upstream&days=2",
            env.user_id
        ),
        false,
    )
    .await;
    assert_eq!(value["total"]["token_provenance"]["observed_requests"], 0);
    assert!(value["total"]["cache_hit_bp"].is_null());
}

async fn check_empty_overview(env: &Env) {
    let value = request(env, "/admin/stats/overview?days=2", false).await;
    for name in ["today", "yesterday", "window"] {
        let total = &value[name];
        assert_eq!(total["requests"], 0, "{value}");
        assert_eq!(total["token_provenance"]["observed_requests"], 0);
        assert_eq!(total["token_provenance"]["history_complete"], true);
        assert_eq!(total["token_provenance"]["prompt"]["unknown"]["tokens"], 0);
        assert!(total["token_provenance"]["history_coverage_bp"].is_null());
        assert!(total["cache_hit_bp"].is_null());
        assert!(total["measured_cache_hit_bp"].is_null());
    }
}

#[tokio::test]
async fn calendar_rank_uses_full_scope_and_exact_days() {
    let database = format!("okapi_calendar_rank_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_calendar_rank(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_calendar_rank(env: &Env, ch: &ChClient) {
    let clock = ch
        .query_json_each_row("SELECT toString(today()) AS day")
        .await
        .unwrap();
    let today =
        chrono::NaiveDate::parse_from_str(clock[0]["day"].as_str().unwrap(), "%Y-%m-%d").unwrap();
    let mut rows = Vec::new();
    for index in 0..52 {
        let mut value = sample(env, 100, 20, 0, "upstream");
        value["group_code"] = json!(format!("rank-{index}"));
        value["client_type"] = json!(format!("client-{index}"));
        value["ts"] = json!(format!("{today} 12:00:00.000"));
        rows.push(value);
    }
    for (name, offset) in [("yesterday", 1), ("outside", 7), ("future", -1)] {
        let mut value = sample(env, 100, 20, 0, "upstream");
        value["group_code"] = json!(name);
        value["client_type"] = json!(name);
        value["ts"] = json!(format!(
            "{} 12:00:00.000",
            today - chrono::Duration::days(offset)
        ));
        rows.push(value);
    }
    insert(ch, &rows).await;
    for (days, expected) in [(1, 52), (7, 53)] {
        let clients = request(
            env,
            &format!("/admin/stats/clients?days={days}&limit=1"),
            false,
        )
        .await;
        assert_eq!(clients["total_requests"], expected, "{clients}");
        assert_eq!(clients["total_clients"], expected);
        assert_eq!(clients["data"].as_array().unwrap().len(), 1);
        assert_eq!(clients["data"][0]["share_bp"], 10000 / expected);
        let groups = request(env, &format!("/admin/stats/groups?days={days}"), false).await;
        assert_eq!(groups["total_amount_micro"], expected * 1000, "{groups}");
        assert_eq!(groups["total_groups"], expected);
        assert_eq!(groups["data"].as_array().unwrap().len(), 50);
        for value in groups["data"].as_array().unwrap() {
            assert_eq!(value["share_bp"], 10000 / expected);
        }
        let daily = request(env, &format!("/api/me/stats/daily?days={days}"), true).await;
        let data = daily["data"].as_array().unwrap();
        assert_eq!(
            data.iter()
                .map(|v| v["requests"].as_i64().unwrap())
                .sum::<i64>(),
            expected
        );
        assert_eq!(
            data.iter()
                .map(|v| v["tokens"].as_i64().unwrap())
                .sum::<i64>(),
            expected * 120
        );
        assert!(
            data.iter()
                .all(|v| v["token_provenance"]["history_complete"] == true)
        );
        for value in [&clients, &groups, &daily] {
            assert_eq!(value["days"], days);
            assert_eq!(value["window"]["end_date"], today.to_string());
            assert_eq!(
                value["window"]["start_date"],
                (today - chrono::Duration::days(days - 1)).to_string()
            );
        }
    }
}
