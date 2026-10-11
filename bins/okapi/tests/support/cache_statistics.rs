use super::token_detail_aggregates::{key_id, request, sample};
use super::ttft_statistics::insert;
use super::{Env, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_cache(value: &Value, requests: i64, known: i64, tokens: Option<i64>) {
    assert_eq!(value["requests"], requests, "{value}");
    assert_eq!(value["cache_write_known_requests"], known, "{value}");
    assert_eq!(
        value["cache_write_tokens"],
        tokens.map_or(Value::Null, |n| json!(n)),
        "{value}"
    );
}

#[tokio::test]
async fn cache_samples_require_numbers_and_retained_counts_stay_in_scope() {
    let database = format!("okapi_cache_samples_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("requires isolated ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_samples(&env, ch))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_samples(env: &Env, ch: &ChClient) {
    let key = key_id(env).await;
    let mut zero = sample(env, key, Some(true), true);
    zero["cache_write_tokens"] = json!(0);
    let mut missing = zero.clone();
    missing["cache_write_tokens"] = Value::Null;
    // A true flag without a numeric value must never create an observed zero.
    let mut unknown = zero.clone();
    unknown["cache_write_tokens"] = json!(999);
    unknown["cache_write_reported"] = Value::Null;
    unknown["cache_read_reported"] = Value::Null;
    let mut outside = zero.clone();
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["api_key_id"] = json!(key + 1_000_000);
    outside["cache_write_tokens"] = json!(9000);
    insert(ch, &[zero, missing, unknown, outside]).await;
    let path = format!("/admin/stats/trend?user_id={}&days=2", env.user_id);
    for expired in [false, true] {
        if expired {
            super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
                .await
                .unwrap();
        }
        let admin = request(env, &path, false).await;
        let portal = request(env, "/api/me/stats/breakdown?days=2&scope=user", true).await;
        assert_eq!(portal["total"]["recorded_cache_write_tokens"], 999);
        for row in portal["data"].as_array().unwrap() {
            assert_eq!(row["recorded_cache_write_tokens"], 999);
        }
        for body in [admin, portal] {
            assert_cache(&body["total"], 3, 1, None);
            assert_eq!(body["total"]["cache_read_known_requests"], 2);
        }
    }
    super::population_storage::execute(
        ch,
        "INSERT INTO mv_cache_totals_5min SELECT * FROM mv_cache_totals_5min",
    )
    .await
    .unwrap();
    // 计数超过请求数的来源不能被当成完整：缓存写入总量保持未知
    let corrupted = request(env, &path, false).await;
    assert_eq!(corrupted["total"]["requests"], 3, "{corrupted}");
    assert!(
        corrupted["total"]["cache_write_tokens"].is_null(),
        "{corrupted}"
    );
}
