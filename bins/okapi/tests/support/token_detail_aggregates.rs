use super::ttft_statistics::{insert, row};

#[tokio::test]
async fn entity_detail_windows_include_exact_calendar_days_for_users_and_keys() {
    let database = format!("okapi_modal_days_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_entity_days(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_entity_days(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let mut rows = Vec::new();
    for days_ago in 0..3 {
        let mut value = sample(env, key, Some(true), true);
        value["ts"] = json!(
            (chrono::Utc::now() - chrono::Duration::days(days_ago))
                .format("%Y-%m-%d %H:%M:%S%.3f")
                .to_string()
        );
        rows.push(value);
    }
    insert(ch, &rows).await;
    for (kind, id) in [("user", env.user_id), ("api_key", key)] {
        for days in [1, 2] {
            let body = request(
                env,
                &format!("/admin/stats/entity-usage?kind={kind}&ids={id}&days={days}"),
                false,
            )
            .await;
            let metric = &body["data"][id.to_string()];
            assert_eq!(metric["requests"], days);
            assert_eq!(metric["window_micro"], days * 1000);
            assert_details(metric, days, days, true);
        }
    }
}

use super::{Env, get, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

const AXES: [(&str, &str, i64); 9] = [
    ("audio_prompt_tokens", "audio_prompt_reported", 11),
    ("image_prompt_tokens", "image_prompt_reported", 13),
    ("audio_completion_tokens", "audio_completion_reported", 17),
    ("image_completion_tokens", "image_completion_reported", 19),
    ("cache_read_audio_tokens", "cache_read_audio_reported", 23),
    ("cache_read_image_tokens", "cache_read_image_reported", 29),
    ("cache_write_audio_tokens", "cache_write_audio_reported", 31),
    ("cache_write_image_tokens", "cache_write_image_reported", 37),
    ("reasoning_tokens", "reasoning_reported", 41),
];

pub(super) async fn request(env: &Env, path: &str, portal: bool) -> Value {
    let token = if portal {
        &env.user_token
    } else {
        &env.super_token
    };
    let (status, body) = get(env, path, token).await;
    assert_eq!(status, 200, "{path}: {body}");
    body
}

pub(super) async fn key_id(env: &Env) -> i64 {
    request(env, "/api/me", true).await["key_id"]
        .as_i64()
        .unwrap()
}

pub(super) fn sample(env: &Env, key: i64, observed: Option<bool>, zero: bool) -> Value {
    let mut value = row(env, json!(10), true);
    value["api_key_id"] = json!(key);
    value["prompt_tokens"] = json!(1000);
    value["completion_tokens"] = json!(500);
    value["upstream_prompt_tokens"] = json!(1000);
    value["upstream_completion_tokens"] = json!(500);
    value["cached_tokens"] = json!(100);
    value["cache_write_tokens"] = json!(100);
    value["cache_read_reported"] = json!(1);
    value["cache_write_reported"] = json!(1);
    for ((counter, flag, amount), legacy) in AXES.into_iter().zip([5, 7, 9, 11, 13, 17, 19, 23, 29])
    {
        value[counter] = json!(if zero {
            0
        } else if observed == Some(true) {
            amount
        } else {
            legacy
        });
        value[flag] = observed.map_or(Value::Null, |known| json!(u8::from(known)));
    }
    value
}

fn assert_details(value: &Value, requests: i64, samples: i64, zero: bool) {
    for (name, _, amount) in AXES {
        let field = &value["token_detail_observations"][name];
        let total = if zero { 0 } else { amount };
        assert_eq!(field["observed_records"], samples, "{name}: {value}");
        assert_eq!(
            field["coverage_bp"],
            samples * 10000 / requests,
            "{name}: {value}"
        );
        assert_eq!(field["observed_tokens"], total, "{name}: {value}");
        assert_eq!(field["complete"], samples == requests, "{name}: {value}");
        assert_eq!(
            field["tokens"],
            if samples == requests {
                json!(total)
            } else {
                Value::Null
            },
            "{name}: {value}"
        );
    }
}

fn assert_unknown(value: &Value, requests: i64) {
    assert_eq!(value["requests"], requests, "{value}");
    for (name, _, _) in AXES {
        let field = &value["token_detail_observations"][name];
        assert_eq!(field["observed_records"], 0, "{value}");
        assert_eq!(field["coverage_bp"], 0, "{value}");
        assert!(field["tokens"].is_null(), "{value}");
        assert!(field["observed_tokens"].is_null(), "{value}");
        assert_eq!(field["complete"], false, "{value}");
    }
}

#[tokio::test]
async fn measured_details_keep_zeros_filters_and_folding_after_raw_retention() {
    let database = format!("okapi_modalities_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_retained_details(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_retained_details(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let mut zero = sample(env, key, Some(true), true);
    zero["model"] = json!(format!("{}-'zero\\t", env.model));
    let mut outside = sample(env, key, Some(false), false);
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["api_key_id"] = json!(key + 1_000_000);
    outside["channel_id"] = json!(0);
    outside["model"] = json!("outside");
    outside["prompt_tokens"] = json!(9000);
    outside["completion_tokens"] = json!(9000);
    outside["amount_micro"] = json!(7000);
    outside["original_amount_micro"] = json!(7250);
    insert(
        ch,
        &[
            sample(env, key, Some(true), false),
            sample(env, key, None, false),
            zero,
            outside,
        ],
    )
    .await;
    ch.ensure_schema().await.unwrap();
    check_views(env, key).await;
    let log = request(
        env,
        &format!("/admin/logs/stat?user_id={}&hours=48", env.user_id),
        false,
    )
    .await;
    assert_details(&log, 3, 2, false);
    assert_eq!(log["tokens"], 4500);
    assert_eq!(log["amount_micro"], 3000);
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_views(env, key).await;
    let empty = request(
        env,
        &format!("/admin/logs/stat?user_id={}&hours=48", env.user_id),
        false,
    )
    .await;
    assert_eq!(empty["requests"], 0);
    assert!(empty["token_detail_observations"]["reasoning_tokens"]["coverage_bp"].is_null());
}

async fn check_views(env: &Env, key: i64) {
    let trend = request(
        env,
        &format!("/admin/stats/trend?user_id={}&days=2", env.user_id),
        false,
    )
    .await;
    assert_details(&trend["total"], 3, 2, false);
    assert_eq!(trend["total"]["tokens"], 4500);
    assert_eq!(trend["total"]["amount_micro"], 3000);
    assert_eq!(
        trend["total"]["token_detail_history"]["observed_requests"],
        3
    );
    assert_eq!(trend["total"]["token_detail_history"]["complete"], true);
    assert!(
        trend["previous"]["token_detail_observations"]["reasoning_tokens"]["coverage_bp"].is_null()
    );
    for by in ["provider", "channel", "user", "group"] {
        let body = request(
            env,
            &format!(
                "/admin/stats/breakdown?user_id={}&days=2&by={by}",
                env.user_id
            ),
            false,
        )
        .await;
        assert_details(&body["data"][0], 3, 2, false);
    }
    let flow = request(
        env,
        &format!(
            "/admin/stats/flow?user_id={}&days=2&metric=tokens",
            env.user_id
        ),
        false,
    )
    .await;
    assert_eq!(flow["total"], 4500);
    assert_details(&flow["metrics"], 3, 2, false);
    for (kind, id) in [("user", env.user_id), ("api_key", key)] {
        let body = request(
            env,
            &format!("/admin/stats/entity-usage?kind={kind}&ids={id}&days=2"),
            false,
        )
        .await;
        assert_details(&body["data"][id.to_string()], 3, 2, false);
    }
    check_portal(env).await;
    check_charts(env).await;
    check_stacked(env).await;
}

async fn check_portal(env: &Env) {
    for scope in ["key", "user"] {
        let body = request(
            env,
            &format!("/api/me/stats/breakdown?days=2&scope={scope}"),
            true,
        )
        .await;
        assert_eq!(body["total"]["requests"], 3);
        assert_eq!(body["total"]["tokens"], 4500);
        assert_eq!(body["total"]["amount_micro"], 3000);
        assert_details(&body["total"], 3, 2, false);
    }
    for path in [
        "/api/me/stats/activity?scope=user",
        "/api/me/stats/daily?days=2",
    ] {
        let body = request(env, path, true).await;
        let data = body["data"].as_array().unwrap();
        let measured = data.iter().find(|r| r["model"] == env.model).unwrap();
        assert_details(measured, 2, 1, false);
        let zero = data.iter().find(|r| r["model"] != env.model).unwrap();
        assert_details(zero, 1, 1, true);
    }
}

async fn check_charts(env: &Env) {
    let models = request(env, "/admin/stats/models?days=2&limit=100", false).await;
    let model = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["model"] == env.model)
        .unwrap();
    assert_details(model, 2, 1, false);
    for path in [
        "/admin/stats/channels?days=2&limit=100".to_owned(),
        format!("/admin/stats/channels/{}/timeline?hours=48", env.channel_id),
    ] {
        let body = request(env, &path, false).await;
        let row = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["channel_id"] == env.channel_id || r.get("bucket").is_some())
            .unwrap();
        assert_details(row, 3, 2, false);
    }
    let overview = request(env, "/admin/stats/overview?days=2", false).await;
    assert_eq!(overview["window"]["requests"], 4);
    assert_eq!(overview["window"]["tokens"], 22500);
    assert_details(&overview["window"], 4, 2, false);
}

async fn check_stacked(env: &Env) {
    let trend = request(
        env,
        &format!(
            "/admin/stats/trend?user_id={}&days=2&stack=model&limit=1",
            env.user_id
        ),
        false,
    )
    .await;
    let bucket = &trend["data"][0]["values"];
    assert_details(&bucket[&env.model], 2, 1, false);
    assert_details(&bucket["__other"], 1, 1, true);
    let models = request(env, "/admin/stats/model-trend?days=2&limit=1", false).await;
    let bucket = &models["data"][0]["values"];
    assert_eq!(bucket["__other"]["requests"], 3);
    assert_details(&bucket["__other"], 3, 2, false);
    assert_unknown(&bucket["outside"], 1);
}

#[tokio::test]
async fn legacy_detail_gaps_recover_raw_and_remain_partial_after_expiry() {
    let database = format!("okapi_modal_upgrade_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_upgrade(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_upgrade(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    ch.execute("DROP TABLE mv_token_details_5min SYNC")
        .await
        .unwrap();
    ch.execute("DROP TABLE mv_analysis_hour SYNC")
        .await
        .unwrap();
    ch.execute("DROP TABLE IF EXISTS mv_cache_totals_5min SYNC")
        .await
        .unwrap();
    insert(
        ch,
        &[
            sample(env, key, Some(true), false),
            sample(env, key, None, false),
        ],
    )
    .await;
    ch.ensure_schema().await.unwrap();
    insert(ch, &[sample(env, key, Some(true), true)]).await;
    let path = format!("/admin/stats/trend?user_id={}&days=2", env.user_id);
    let before = request(env, &path, false).await;
    assert_details(&before["total"], 3, 2, false);
    assert_eq!(
        before["total"]["token_detail_history"]["observed_requests"],
        3
    );
    assert_eq!(before["total"]["tokens"], 4500);
    assert_eq!(before["total"]["amount_micro"], 3000);
    assert_eq!(before["total"]["cache_write_tokens"], 300);
    assert_eq!(before["total"]["cache_write_known_requests"], 3);
    assert_eq!(before["total"]["cache_read_known_requests"], 3);
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    for body in [
        request(env, &path, false).await,
        request(env, "/api/me/stats/breakdown?days=2&scope=user", true).await,
    ] {
        assert_details(&body["total"], 3, 1, true);
        assert_eq!(
            body["total"]["token_detail_history"]["observed_requests"],
            1
        );
        assert_eq!(body["total"]["token_detail_history"]["complete"], false);
        assert_eq!(body["total"]["tokens"], 4500);
        assert_eq!(body["total"]["amount_micro"], 3000);
        assert_eq!(body["total"]["cache_write_tokens"], 300);
        assert_eq!(body["total"]["cache_write_known_requests"], 3);
    }
    // Invalid aggregate counts may not become valid detail observations.
    for _ in 0..2 {
        ch.execute("INSERT INTO mv_token_details_5min SELECT * FROM mv_token_details_5min")
            .await
            .unwrap();
    }
    let polluted = request(env, &path, false).await;
    assert_unknown(&polluted["total"], 3);
    assert_eq!(polluted["total"]["tokens"], 4500);
    assert_eq!(polluted["total"]["amount_micro"], 3000);
}

#[tokio::test]
async fn detail_coverage_checks_each_grain_even_when_total_counts_match() {
    let database = format!("okapi_modal_grains_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_grains(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_grains(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    insert(ch, &[sample(env, key, Some(true), false)]).await;
    // Shift the detail grain while keeping its overall count exactly equal.
    ch.execute("INSERT INTO mv_token_details_5min SELECT * REPLACE ('wrong-model' AS model) FROM mv_token_details_5min").await.unwrap();
    ch.execute("ALTER TABLE mv_token_details_5min DELETE WHERE model != 'wrong-model' SETTINGS mutations_sync=2").await.unwrap();
    let path = format!("/admin/stats/trend?user_id={}&days=2", env.user_id);
    let body = request(env, &path, false).await;
    assert_details(&body["total"], 1, 1, false);
    assert_eq!(body["total"]["token_detail_history"]["complete"], true);
}
