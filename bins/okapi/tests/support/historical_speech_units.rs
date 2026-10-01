use super::token_detail_aggregates::{key_id, request};
use super::ttft_statistics::{insert, row};
use super::{Env, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

fn legacy_speech(env: &Env, key: i64) -> Value {
    let mut value = row(env, Value::Null, false);
    value["api_key_id"] = json!(key);
    value["endpoint"] = json!("/v1/audio/speech");
    value["upstream_endpoint"] = json!("/v1/audio/speech");
    value["input_unit"] = json!("");
    value["input_characters"] = Value::Null;
    value["prompt_tokens"] = json!(11);
    value["completion_tokens"] = json!(0);
    value["cached_tokens"] = json!(0);
    value["cache_write_tokens"] = json!(0);
    value["cache_read_reported"] = json!(0);
    value["cache_write_reported"] = json!(0);
    value["reasoning_tokens"] = json!(0);
    for field in [
        "audio_prompt_tokens",
        "audio_completion_tokens",
        "image_prompt_tokens",
        "image_completion_tokens",
    ] {
        value[field] = json!(0);
    }
    value["prompt_source"] = json!("unknown");
    value["completion_source"] = json!("unknown");
    value["upstream_prompt_tokens"] = Value::Null;
    value["upstream_completion_tokens"] = Value::Null;
    value["pricing_epoch"] = json!(41);
    value["ratio_snapshot"] = json!(
        json!({
            "epoch":41,"mode":"ratio","model_ratio":1,"completion_ratio":1,"cache_ratio":1,
            "group":"default","group_ratio":1,"user_multiplier":1,"rules":[],
            "final_unit_price_input_per_1m_usd":2
        })
        .to_string()
    );
    value["amount_micro"] = json!(22);
    value["original_amount_micro"] = json!(22);
    value["discount_micro"] = json!(0);
    value["upstream_cost_micro"] = json!(22);
    value["latency_ms"] = json!(9000);
    value
}

#[tokio::test]
async fn historical_speech_characters_do_not_enter_primary_token_totals() {
    let database = format!("okapi_old_speech_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_legacy(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_legacy(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let speech = legacy_speech(env, key);
    let mut tokens = row(env, Value::Null, false);
    tokens["api_key_id"] = json!(key);
    tokens["prompt_tokens"] = json!(100);
    tokens["completion_tokens"] = json!(100);
    tokens["cached_tokens"] = json!(0);
    tokens["cache_write_tokens"] = json!(0);
    tokens["amount_micro"] = json!(1000);
    insert(ch, &[speech, tokens]).await;
    assert_eq!(
        okapi::worker::legacy_speech::process_once(ch, 1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        okapi::worker::legacy_speech::process_once(ch, 1)
            .await
            .unwrap(),
        0
    );
    let body = request(
        env,
        &format!("/admin/stats/trend?days=2&user_id={}", env.user_id),
        false,
    )
    .await;
    assert_eq!(body["total"]["requests"], 2);
    assert_eq!(body["total"]["amount_micro"], 1022);
    assert_eq!(
        body["total"]["prompt_tokens"], 100,
        "historical characters must not be Token input"
    );
    assert_eq!(body["total"]["tokens"], 200);
    assert_eq!(body["total"]["input_units"]["observed_characters"], 11);
    assert_eq!(body["total"]["input_units"]["character_requests"], 1);
    check_primary_views(env, key).await;
    ch.ensure_schema().await.unwrap();
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_primary_views(env, key).await;
}

async fn check_primary_views(env: &Env, key: i64) {
    let overview = request(env, "/admin/stats/overview?days=2", false).await;
    assert_eq!(overview["window"]["tokens"], 200, "{overview}");
    assert_eq!(overview["window"]["amount_micro"], 1022);
    for (path, personal, field) in [
        ("/admin/stats/models?days=2", false, "tokens"),
        ("/admin/stats/clients?days=2", false, "tokens"),
        ("/admin/stats/groups?days=2", false, "tokens"),
        ("/api/me/stats/daily?days=2", true, "tokens"),
        ("/api/me/stats/activity?scope=user", true, "prompt_tokens"),
    ] {
        let body = request(env, path, personal).await;
        let expected = if field == "prompt_tokens" { 100 } else { 200 };
        assert_eq!(body["data"][0][field], expected, "{path}: {body}");
        assert_eq!(body["data"][0]["amount_micro"], 1022, "{path}: {body}");
        if path == "/admin/stats/models?days=2" {
            assert_eq!(body["data"][0]["avg_output_tps_milli"], 100_000, "{body}");
            assert_eq!(
                body["data"][0]["output_tps_unit_known_requests"], 2,
                "{body}"
            );
            assert_eq!(body["data"][0]["output_tps_duration_ms"], 1000, "{body}");
        }
    }
    for (kind, id) in [("user", env.user_id), ("api_key", key)] {
        let body = request(
            env,
            &format!("/admin/stats/entity-usage?kind={kind}&ids={id}&days=2"),
            false,
        )
        .await;
        let value = &body["data"][id.to_string()];
        assert_eq!(value["window_micro"], 1022, "{body}");
        assert_eq!(value["input_units"]["characters"], 11, "{body}");
        assert_eq!(
            value["token_provenance"]["prompt"]["unknown"]["tokens"], 0,
            "{body}"
        );
    }
    for scope in ["key", "user"] {
        let body = request(
            env,
            &format!("/api/me/stats/breakdown?days=2&scope={scope}"),
            true,
        )
        .await;
        assert_eq!(body["total"]["prompt_tokens"], 100, "{body}");
        assert_eq!(body["total"]["tokens"], 200, "{body}");
        assert_eq!(body["total"]["input_units"]["characters"], 11, "{body}");
    }
}

#[tokio::test]
async fn historical_speech_partial_upgrade_retains_proof_without_inventing_unknown_units() {
    let database = format!("okapi_speech_upgrade_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_partial_upgrade(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_partial_upgrade(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    for table in [
        "mv_analysis_hour",
        "mv_input_units_5min",
        "mv_output_rate_5min",
    ] {
        ch.execute(&format!("DROP TABLE {table} SYNC"))
            .await
            .unwrap();
    }
    let speech = legacy_speech(env, key);
    let mut unknown = row(env, Value::Null, false);
    unknown["api_key_id"] = json!(key);
    unknown["input_unit"] = json!("");
    unknown["prompt_tokens"] = json!(50);
    unknown["completion_tokens"] = json!(0);
    insert(ch, &[speech, unknown]).await;
    ch.ensure_schema().await.unwrap();
    let mut token = row(env, Value::Null, false);
    token["api_key_id"] = json!(key);
    token["prompt_tokens"] = json!(100);
    token["completion_tokens"] = json!(100);
    insert(ch, &[token]).await;
    while okapi::worker::legacy_speech::process_once(ch, 1)
        .await
        .unwrap()
        > 0
    {}
    check_partial_views(env).await;
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_partial_views(env).await;
}

async fn check_partial_views(env: &Env) {
    let path = format!("/admin/stats/trend?days=2&user_id={}", env.user_id);
    let body = request(env, &path, false).await;
    let total = &body["total"];
    assert_eq!(total["requests"], 3, "{body}");
    assert_eq!(total["prompt_tokens"], 150, "{body}");
    assert_eq!(total["tokens"], 250, "{body}");
    assert_eq!(total["amount_micro"], 2022, "{body}");
    assert_eq!(total["input_units"]["observed_characters"], 11, "{body}");
    assert_eq!(total["input_units"]["character_requests"], 1, "{body}");
    assert_eq!(total["input_units"]["unknown_requests"], 1, "{body}");
    assert_eq!(total["output_tps_unit_known_requests"], 2, "{body}");
    assert!(total["input_units"]["characters"].is_null(), "{body}");
    assert!(total["tokens_per_1k_sec"].is_null(), "{body}");
}

#[tokio::test]
async fn historical_characters_are_excluded_before_token_rank_flow_and_bound_filters() {
    let database = format!("okapi_speech_rank_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_rank_filters(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_rank_filters(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let mut speech = legacy_speech(env, key);
    let special = format!("{}-'字符\\", env.model);
    speech["model"] = json!(special);
    speech["prompt_tokens"] = json!(1000);
    speech["amount_micro"] = json!(2000);
    let mut token = row(env, Value::Null, false);
    token["api_key_id"] = json!(key);
    token["prompt_tokens"] = json!(100);
    token["completion_tokens"] = json!(100);
    let mut outside = speech.clone();
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["api_key_id"] = json!(key + 1_000_000);
    outside["prompt_tokens"] = json!(99_999);
    insert(ch, &[speech, token, outside]).await;
    while okapi::worker::legacy_speech::process_once(ch, 1)
        .await
        .unwrap()
        > 0
    {}
    assert_rank_filters(env, key, &special).await;
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    assert_rank_filters(env, key, &special).await;
}

async fn assert_rank_filters(env: &Env, key: i64, special: &str) {
    let body = request(
        env,
        &format!(
            "/admin/stats/breakdown?days=2&user_id={}&by=model&metric=tokens&limit=1",
            env.user_id
        ),
        false,
    )
    .await;
    assert_eq!(body["data"][0]["key"], env.model, "{body}");
    assert_eq!(body["data"][0]["tokens"], 200, "{body}");
    assert_eq!(body["data"][0]["token_share_bp"], 10000, "{body}");
    let flow=request(env,&format!("/admin/stats/flow?days=2&user_id={}&metric=tokens&limit=1&stages=%5B%22model%22%2C%22channel%22%5D",env.user_id),false).await;
    assert_eq!(flow["total"], 200, "{flow}");
    let links = flow["links"].as_array().unwrap();
    assert_eq!(
        links
            .iter()
            .map(|link| link["value"].as_i64().unwrap())
            .sum::<i64>(),
        200,
        "{flow}"
    );
    let mut url = reqwest::Url::parse("http://localhost/admin/stats/trend").unwrap();
    url.query_pairs_mut()
        .append_pair("days", "2")
        .append_pair("user_id", &env.user_id.to_string())
        .append_pair("api_key_id", &key.to_string())
        .append_pair("model", special)
        .append_pair("endpoint", "/v1/audio/speech");
    let filtered = request(
        env,
        &format!("{}?{}", url.path(), url.query().unwrap()),
        false,
    )
    .await;
    let total = &filtered["total"];
    assert_eq!(total["requests"], 1, "{filtered}");
    assert_eq!(total["tokens"], 0, "{filtered}");
    assert_eq!(total["amount_micro"], 2000, "{filtered}");
    assert_eq!(total["input_units"]["characters"], 1000, "{filtered}");
}

#[tokio::test]
async fn legacy_speech_calibration_retains_evidence_with_idempotent_resume() {
    let database = format!("okapi_speech_evidence_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_evidence(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_evidence(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let old = legacy_speech(env, key);
    let mut zero = legacy_speech(env, key);
    zero["prompt_tokens"] = json!(0);
    let mut unknown = legacy_speech(env, key);
    unknown["ratio_snapshot"] = json!("{}");
    insert(ch, &[old.clone(), old, zero, unknown]).await;
    for _ in 0..4 {
        if okapi::worker::legacy_speech::process_once(ch, 1)
            .await
            .unwrap()
            == 0
        {
            break;
        }
    }
    let rows=ch.query_json_each_row("SELECT count() AS n,sum(copies) AS requests,sum(toUInt64(characters)*copies) AS characters FROM legacy_speech_units_v1 FINAL").await.unwrap();
    let number = |field: &str| {
        rows[0][field]
            .as_u64()
            .or_else(|| rows[0][field].as_str().and_then(|s| s.parse().ok()))
    };
    assert_eq!(number("n"), Some(2));
    assert_eq!(number("requests"), Some(3));
    assert_eq!(number("characters"), Some(22));
    ch.execute("TRUNCATE TABLE legacy_speech_calibration_v1")
        .await
        .unwrap();
    for _ in 0..4 {
        if okapi::worker::legacy_speech::process_once(ch, 1)
            .await
            .unwrap()
            == 0
        {
            break;
        }
    }
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    let retained=ch.query_json_each_row("SELECT count() AS n,sum(copies) AS requests,sum(toUInt64(characters)*copies) AS characters FROM legacy_speech_units_v1 FINAL").await.unwrap();
    assert_eq!(
        retained, rows,
        "rescan and raw expiry must not duplicate or lose evidence"
    );
    assert_eq!(
        okapi::worker::legacy_speech::process_once(ch, 1)
            .await
            .unwrap(),
        0
    );
}
