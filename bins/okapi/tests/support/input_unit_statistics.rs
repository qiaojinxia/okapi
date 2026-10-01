use super::token_detail_aggregates::{key_id, request};
use super::ttft_statistics::{insert, row};
use super::{Env, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn input_units_keep_characters_zero_unknown_filters_and_totals_after_raw_expiry() {
    let database = format!("okapi_input_units_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_retained(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn sample(
    env: &Env,
    key: i64,
    unit: &str,
    characters: Value,
    prompt: i64,
    completion: i64,
) -> Value {
    let mut value = row(env, json!(0), false);
    value["api_key_id"] = json!(key);
    value["prompt_tokens"] = json!(prompt);
    value["completion_tokens"] = json!(completion);
    value["cached_tokens"] = json!(0);
    value["cache_write_tokens"] = json!(0);
    value["reasoning_tokens"] = json!(0);
    value["input_unit"] = json!(unit);
    value["input_characters"] = characters;
    value
}

async fn check_retained(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let mut speech = sample(env, key, "characters", json!(11), 0, 0);
    speech["endpoint"] = json!("/v1/audio/speech");
    let mut zero = sample(env, key, "characters", json!(0), 0, 0);
    zero["model"] = json!(format!("{}-'zero\\t", env.model));
    zero["amount_micro"] = json!(0);
    let tokens = sample(env, key, "tokens", Value::Null, 100, 20);
    let unknown = sample(env, key, "", Value::Null, 50, 10);
    let invalid = sample(env, key, "characters", json!(11), 11, 0);
    let mut outside = sample(env, key + 1_000_000, "characters", json!(90000), 0, 0);
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["channel_id"] = json!(0);
    outside["model"] = json!("outside");
    insert(ch, &[speech, zero, tokens, unknown, invalid, outside]).await;
    let logs = request(
        env,
        &format!("/admin/logs/stat?hours=48&user_id={}", env.user_id),
        false,
    )
    .await;
    assert_units(&logs, 5, 11, 2, 1, 2);
    assert_eq!(logs["tokens"], 191);
    assert_eq!(logs["amount_micro"], 4000);
    check_views(env, key).await;
    // Re-running installation and batch replay leave aggregate populations unchanged.
    ch.ensure_schema().await.unwrap();
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_views(env, key).await;
}

fn assert_units(
    value: &Value,
    requests: i64,
    characters: i64,
    char_n: i64,
    token_n: i64,
    unknown: i64,
) {
    let metric = &value["input_units"];
    assert_eq!(metric["observed_characters"], characters, "{value}");
    assert_eq!(
        metric["characters"],
        if unknown == 0 {
            json!(characters)
        } else {
            Value::Null
        },
        "{value}"
    );
    assert_eq!(metric["character_requests"], char_n, "{value}");
    assert_eq!(metric["token_requests"], token_n, "{value}");
    assert_eq!(metric["unknown_requests"], unknown, "{value}");
    assert_eq!(metric["known_requests"], requests - unknown, "{value}");
    assert_eq!(
        metric["coverage_bp"],
        (requests - unknown) * 10000 / requests,
        "{value}"
    );
}

async fn check_views(env: &Env, key: i64) {
    let trend = request(
        env,
        &format!("/admin/stats/trend?days=2&user_id={}", env.user_id),
        false,
    )
    .await;
    assert_units(&trend["total"], 5, 11, 2, 1, 2);
    assert_eq!(trend["total"]["tokens"], 191);
    assert_eq!(trend["total"]["amount_micro"], 4000);
    for by in ["provider", "channel", "user", "group"] {
        let body = request(
            env,
            &format!(
                "/admin/stats/breakdown?days=2&user_id={}&by={by}",
                env.user_id
            ),
            false,
        )
        .await;
        assert_units(&body["data"][0], 5, 11, 2, 1, 2);
    }
    for (kind, id) in [("user", env.user_id), ("api_key", key)] {
        let body = request(
            env,
            &format!("/admin/stats/entity-usage?kind={kind}&ids={id}&days=2"),
            false,
        )
        .await;
        assert_units(&body["data"][id.to_string()], 5, 11, 2, 1, 2);
    }
    let filtered = request(
        env,
        &format!(
            "/admin/stats/trend?days=2&user_id={}&endpoint=%2Fv1%2Faudio%2Fspeech",
            env.user_id
        ),
        false,
    )
    .await;
    assert_units(&filtered["total"], 1, 11, 1, 0, 0);
    assert_eq!(filtered["total"]["tokens"], 0);
    for scope in ["key", "user"] {
        let body = request(
            env,
            &format!("/api/me/stats/breakdown?days=2&scope={scope}"),
            true,
        )
        .await;
        assert_units(&body["total"], 5, 11, 2, 1, 2);
        assert_eq!(body["total"]["tokens"], 191);
    }
    for path in [
        "/api/me/stats/activity?scope=user",
        "/api/me/stats/daily?days=2",
    ] {
        let body = request(env, path, true).await;
        let rows = body["data"].as_array().unwrap();
        let main = rows.iter().find(|r| r["model"] == env.model).unwrap();
        assert_units(main, 4, 11, 1, 1, 2);
        let zero = rows.iter().find(|r| r["model"] != env.model).unwrap();
        assert_units(zero, 1, 0, 1, 0, 0);
    }
}

#[tokio::test]
async fn upgraded_character_aggregate_recovers_raw_once_then_marks_missing_history_unknown() {
    let database = format!("okapi_input_upgrade_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated ClickHouse required");
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
    ch.execute("DROP TABLE mv_input_units_5min SYNC")
        .await
        .unwrap();
    insert(ch, &[sample(env, key, "characters", json!(11), 0, 0)]).await;
    ch.ensure_schema().await.unwrap();
    insert(ch, &[sample(env, key, "characters", json!(0), 0, 0)]).await;
    let path = format!("/admin/stats/trend?days=2&user_id={}", env.user_id);
    let body = request(env, &path, false).await;
    assert_units(&body["total"], 2, 11, 2, 0, 0);
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    let body = request(env, &path, false).await;
    assert_units(&body["total"], 2, 0, 1, 0, 1);
    assert_eq!(body["total"]["tokens"], 0);
    assert_eq!(body["total"]["amount_micro"], 2000);
}
