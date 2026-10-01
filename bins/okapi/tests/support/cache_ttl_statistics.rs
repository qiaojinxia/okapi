use super::token_detail_aggregates::{key_id, request, sample};
use super::ttft_statistics::insert;
use super::{Env, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_ttl(value: &Value, requests: i64, samples: i64, short: i64, long: i64) {
    assert_eq!(value["requests"], requests, "{value}");
    for (name, total) in [
        ("cache_write_5m_tokens", short),
        ("cache_write_1h_tokens", long),
    ] {
        let observed = &value["token_detail_observations"][name];
        assert_eq!(observed["observed_records"], samples, "{name}: {value}");
        assert_eq!(observed["observed_tokens"], total, "{name}: {value}");
        assert_eq!(observed["coverage_bp"], samples * 10000 / requests);
        assert_eq!(observed["complete"], samples == requests);
        assert_eq!(
            observed["tokens"],
            if samples == requests {
                json!(total)
            } else {
                Value::Null
            }
        );
    }
}

async fn views(env: &Env, key: i64, partial: bool) {
    let (calls, samples, amount) = if partial { (3, 2, 2000) } else { (1, 1, 1000) };
    let trend = request(
        env,
        &format!("/admin/stats/trend?user_id={}&days=2", env.user_id),
        false,
    )
    .await;
    assert_ttl(&trend["total"], calls, samples, 60, 40);
    assert_eq!(trend["total"]["amount_micro"], amount);
    assert_eq!(trend["total"]["tokens"], calls * 1500);
    for by in ["model", "provider", "channel", "group", "user"] {
        let breakdown = request(
            env,
            &format!(
                "/admin/stats/breakdown?user_id={}&days=2&by={by}",
                env.user_id
            ),
            false,
        )
        .await;
        assert_ttl(&breakdown["data"][0], calls, samples, 60, 40);
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
    assert_ttl(&flow["metrics"], calls, samples, 60, 40);
    for (kind, id) in [("user", env.user_id), ("api_key", key)] {
        let entity = request(
            env,
            &format!("/admin/stats/entity-usage?kind={kind}&ids={id}&days=2"),
            false,
        )
        .await;
        assert_ttl(&entity["data"][id.to_string()], calls, samples, 60, 40);
    }
    for scope in ["key", "user"] {
        let portal = request(
            env,
            &format!("/api/me/stats/breakdown?days=2&scope={scope}"),
            true,
        )
        .await;
        assert_ttl(&portal["total"], calls, samples, 60, 40);
        assert_eq!(portal["total"]["amount_micro"], amount);
    }
}

async fn check(env: &Env, ch: &ChClient, partial: bool, upgrade: bool) {
    let key = key_id(env).await;
    let mut mixed = sample(env, key, Some(true), false);
    mixed["cache_write_5m_tokens"] = json!(60);
    mixed["cache_write_1h_tokens"] = json!(40);
    let mut rows = vec![mixed.clone()];
    if partial {
        let mut zero = sample(env, key, Some(true), true);
        zero["cache_write_tokens"] = json!(0);
        zero["cache_write_5m_tokens"] = json!(0);
        zero["cache_write_1h_tokens"] = json!(0);
        rows.push(zero);
        rows.push(sample(env, key, None, false));
        let mut refund = mixed;
        refund["request_id"] = json!(Uuid::new_v4());
        refund["log_type"] = json!(6);
        for field in ["amount_micro", "original_amount_micro"] {
            refund[field] = json!(-1000);
        }
        refund["discount_micro"] = json!(0);
        rows.push(refund);
    }
    let mut outside = sample(env, key, Some(true), false);
    outside["user_id"] = json!(env.user_id + 1_000_000);
    outside["api_key_id"] = json!(key + 1_000_000);
    outside["model"] = json!("outside-ttl");
    outside["prompt_tokens"] = json!(9000);
    outside["cache_write_tokens"] = json!(2000);
    outside["cache_write_5m_tokens"] = json!(1000);
    outside["cache_write_1h_tokens"] = json!(1000);
    rows.push(outside);
    insert(ch, &rows).await;
    if partial {
        views(env, key, true).await;
    }
    if upgrade {
        super::population_storage::execute(ch, "TRUNCATE TABLE mv_cache_ttl_5min")
            .await
            .unwrap();
        views(env, key, true).await;
    }
    super::population_storage::execute(ch, "TRUNCATE TABLE request_log_raw")
        .await
        .unwrap();
    if upgrade {
        for (path, portal) in [
            (
                format!("/admin/stats/trend?user_id={}&days=2", env.user_id),
                false,
            ),
            ("/api/me/stats/breakdown?days=2&scope=user".into(), true),
        ] {
            let body = request(env, &path, portal).await;
            let total = &body["total"];
            assert_eq!(total["requests"], 3);
            assert_eq!(total["tokens"], 4500);
            assert_eq!(total["amount_micro"], 2000);
            assert_eq!(
                total["token_detail_observations"]["image_prompt_tokens"]["observed_records"],
                2
            );
            assert_eq!(
                total["token_detail_observations"]["image_prompt_tokens"]["observed_tokens"],
                13
            );
            for name in ["cache_write_5m_tokens", "cache_write_1h_tokens"] {
                let axis = &total["token_detail_observations"][name];
                assert_eq!(axis["observed_records"], 0, "{total}");
                assert!(axis["observed_tokens"].is_null());
                assert!(axis["tokens"].is_null());
                assert_eq!(axis["coverage_bp"], 0);
            }
            assert_eq!(total["cache_write_ttl_history"]["observed_requests"], 0);
            assert_eq!(total["cache_write_ttl_history"]["complete"], false);
        }
    } else {
        views(env, key, partial).await;
    }
}

async fn isolated(partial: bool, upgrade: bool) {
    let database = format!("okapi_ttl_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env.state.ch.as_ref().expect("isolated CH required");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check(&env, ch, partial, upgrade))
        .catch_unwind()
        .await;
    super::population_storage::execute(ch, &format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn ttl_counts_remain_queryable_after_raw_expiry() {
    isolated(false, false).await;
}

#[tokio::test]
async fn ttl_zero_unknown_refund_and_scope_survive_raw_expiry() {
    isolated(true, false).await;
}

#[tokio::test]
async fn ttl_upgrade_recovers_raw_and_keeps_old_modality_history_when_unrecoverable() {
    isolated(true, true).await;
}
