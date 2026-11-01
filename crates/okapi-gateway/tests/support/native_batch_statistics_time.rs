use super::*;
use chrono::{Datelike, TimeZone};
use fred::interfaces::KeysInterface;
use okapi_store::image_batches::statistics as db;

async fn counter(redis: &fred::clients::Client, key: &str) -> i64 {
    redis.get::<Option<i64>, _>(key).await.unwrap().unwrap_or(0)
}

#[tokio::test]
async fn late_statistics_use_original_month_and_never_resurrect_expired_buckets() {
    let env = Env::new().await;
    let original = member(&env).await;
    let job = env.submit(1, "month-boundary").await;
    archive::finish(&env, &job).await;
    let mut row = delivery(&env, &job).await;
    let redis = redis().await;
    let now = chrono::Utc::now();
    row.batch_id = Uuid::new_v4();
    row.recorded_at = chrono::Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .unwrap()
        - chrono::Duration::seconds(1);
    let old_month = row.recorded_at.format("%Y%m");
    let old_tokens = format!("tok:{{{}}}:{old_month}", env.uid);
    let old_spend = format!("spend:tm:{}:{original}:{old_month}", env.uid);
    env.state.sched.record_batch_statistics(&row).await.unwrap();
    env.state.sched.record_batch_statistics(&row).await.unwrap();
    assert_eq!(counter(&redis, &old_tokens).await, 22);
    assert_eq!(counter(&redis, &old_spend).await, PRICE / 2);
    assert_eq!(env.state.sched.monthly_tokens_get(env.uid).await, 22);
    assert_eq!(
        env.state.sched.member_spend_get(env.uid, original).await,
        PRICE / 2
    );
    row.batch_id = Uuid::new_v4();
    row.recorded_at = now - chrono::Duration::days(80);
    env.state.sched.record_batch_statistics(&row).await.unwrap();
    let ancient = format!("tok:{{{}}}:{}", env.uid, row.recorded_at.format("%Y%m"));
    assert!(!redis.exists::<bool, _>(&ancient).await.unwrap());
    let old_kpi = format!("kpi:{{kpi}}:req:{}", row.recorded_at.timestamp());
    assert!(!redis.exists::<bool, _>(&old_kpi).await.unwrap());
    env.close().await;
}

#[tokio::test]
async fn statistics_kpi_replay_and_large_integer_addition_are_exact() {
    let env = Env::new().await;
    let job = env.submit(1, "kpi").await;
    archive::finish(&env, &job).await;
    let mut row = delivery(&env, &job).await;
    row.batch_id = Uuid::new_v4();
    row.recorded_at = chrono::Utc::now() - chrono::Duration::seconds(5);
    row.is_error = true;
    let redis = redis().await;
    let keys: Vec<_> = ["req", "tok", "amt", "err"]
        .map(|s| format!("kpi:{{kpi}}:{s}:{}", row.recorded_at.timestamp()))
        .into();
    let mut before = Vec::new();
    for key in &keys {
        before.push(counter(&redis, key).await);
    }
    let spend = format!("usd:{{{}}}:{}", env.uid, row.recorded_at.format("%Y%m"));
    let large = 9_007_199_254_740_991_i64;
    redis
        .set::<(), _, _>(&spend, large, None, None, false)
        .await
        .unwrap();
    env.state.sched.record_batch_statistics(&row).await.unwrap();
    env.state.sched.record_batch_statistics(&row).await.unwrap();
    assert_eq!(counter(&redis, &spend).await, large + PRICE / 2);
    for ((key, before), delta) in keys.iter().zip(before).zip([1, 22, PRICE / 2, 1]) {
        assert_eq!(counter(&redis, key).await, before + delta);
    }
    env.close().await;
}

#[tokio::test]
async fn statistics_lease_is_exclusive_and_late_ack_cannot_finish_a_new_lease() {
    let env = Env::new().await;
    let job = env.submit(1, "statistics-leases").await;
    archive::finish(&env, &job).await;
    sqlx::query("UPDATE image_batch_statistics SET delivered_at=NULL,next_attempt_at=now() WHERE batch_id=$1")
        .bind(id(&job)).execute(&env.state.pg).await.unwrap();
    let first = db::claim(&env.state.pg, Some(id(&job)))
        .await
        .unwrap()
        .unwrap();
    assert!(
        db::claim(&env.state.pg, Some(id(&job)))
            .await
            .unwrap()
            .is_none()
    );
    due(&env, &job).await;
    let next = db::claim(&env.state.pg, Some(id(&job)))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.lease_id, next.lease_id);
    assert!(db::acknowledge(&env.state.pg, &first, true).await.is_err());
    env.state
        .sched
        .record_batch_statistics(&next)
        .await
        .unwrap();
    db::acknowledge(&env.state.pg, &next, true).await.unwrap();
    assert_eq!(env.state.sched.monthly_tokens_get(env.uid).await, 22);
    env.money(&job, PRICE / 2).await;
    env.close().await;
}
