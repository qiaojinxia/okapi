//! 多副本 HA 协调验收：Redis 维护任务租约（互斥 + CAS 续租/释放 + 轮次占位）、
//! 跨副本 `key_health` 失败计数自动清零、NATS 广播失效 `margin_cache`。

use okapi_engine as engine;
use okapi_kernel::sched_redis::{MaintenanceLease, SchedulerRedis};
use okapi_store::channels::{ChannelCandidate, KeyFailure};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn maintenance_lease_coordinates_multi_replica_workers() {
    okapi_store::test_support::assert_isolated();
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
    let sched = SchedulerRedis::new(redis);

    // 1. 活跃锁 + 轮次占位：副本 A 拿到锁，副本 B 跳过；CAS 校验防误续/误放
    let job = format!("reconcile_{}", Uuid::new_v4().simple());
    let MaintenanceLease::Acquired(token_a) =
        sched.maintenance_claim(&job, "worker-a", 60, 30).await
    else {
        panic!("worker-a must acquire lease");
    };
    assert_eq!(
        sched.maintenance_claim(&job, "worker-b", 60, 30).await,
        MaintenanceLease::Skipped,
        "活跃锁持有期间另一副本必须跳过"
    );
    assert!(sched.maintenance_extend(&job, &token_a, 90).await);
    assert!(!sched.maintenance_extend(&job, "wrong-token", 90).await);

    // 错 token 无法释放；正确 token 释放活跃锁后，轮次占位仍在 30s 内阻止重复跑
    sched.maintenance_release(&job, "wrong-token").await;
    assert!(sched.maintenance_extend(&job, &token_a, 90).await);
    sched.maintenance_release(&job, &token_a).await;
    assert!(!sched.maintenance_extend(&job, &token_a, 90).await);
    assert_eq!(
        sched.maintenance_claim(&job, "worker-b", 60, 30).await,
        MaintenanceLease::Skipped,
        "本周期轮次占位未过期前另一副本不应重复扫描"
    );

    // 2. 无轮次占位（round_ttl = 0）时释放活跃锁后另一副本可立即接管
    let job2 = format!("handoff_{}", Uuid::new_v4().simple());
    let MaintenanceLease::Acquired(t1) = sched.maintenance_claim(&job2, "worker-a", 60, 0).await
    else {
        panic!("worker-a must acquire");
    };
    assert_eq!(
        sched.maintenance_claim(&job2, "worker-b", 60, 0).await,
        MaintenanceLease::Skipped
    );
    sched.maintenance_release(&job2, &t1).await;
    let MaintenanceLease::Acquired(t2) = sched.maintenance_claim(&job2, "worker-b", 60, 0).await
    else {
        panic!("worker-b must take over immediately after release");
    };
    sched.maintenance_release(&job2, &t2).await;
}

#[tokio::test]
async fn cross_replica_key_health_clears_failures_on_other_node() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    // 构造两个进程内缓存完全隔离的副本，共享同一 PG 与 Redis
    let replica_a = engine::build_state(&database_url, &redis_url, "gw-a", None, None)
        .await
        .unwrap();
    let replica_b = engine::build_state(&database_url, &redis_url, "gw-b", None, None)
        .await
        .unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let channel_id: i64 = sqlx::query_scalar(
        "INSERT INTO channels (name, provider, status, priority, weight, api_base, settings)
         VALUES ($1, 'openai', 1, 10, 10, 'https://example.invalid/v1', '{}'::jsonb)
         RETURNING id",
    )
    .bind(format!("ha-ch-{suffix}"))
    .fetch_one(&pg)
    .await
    .unwrap();
    let channel_key_id: i64 = sqlx::query_scalar(
        "INSERT INTO channel_keys (channel_id, credential_ciphertext, credential_kind, status, weight)
         VALUES ($1, $2, 0, 1, 1)
         RETURNING id",
    )
    .bind(channel_id)
    .bind(b"sk-ha-test".as_slice())
    .fetch_one(&pg)
    .await
    .unwrap();

    let cand = ChannelCandidate {
        channel_id,
        channel_key_id,
        channel_name: format!("ha-ch-{suffix}"),
        provider: "openai".to_owned(),
        api_base: Some("https://example.invalid/v1".to_owned()),
        credential: "sk-ha-test".to_owned(),
        priority: 10,
        weight: 10,
        trust_upstream_usage: true,
        max_concurrency: None,
        rpm_limit: None,
        daily_spend_cap_micro: None,
        model_mapping: serde_json::json!({}),
        thinking_to_content: false,
        bill_by_response_model: false,
        strip_request_fields: Vec::new(),
        inject_request_fields: serde_json::Map::new(),
        tool_loss_policy: None,
        responses_native: false,
        api_version: None,
        aws_region: None,
        oauth_token_url: None,
        extensions: serde_json::Value::Null,
        proxy_url: None,
        egress_proxy_id: None,
        egress_max_concurrency: None,
        egress_rotation: None,
        extra_headers: Vec::new(),
        capabilities: serde_json::Value::Null,
        cost_milli: 1000,
        same_key_retries: 1,
        first_output_timeout_secs: 30,
        pool_rank: 0,
        count_in_flight: false,
        warm_affinity: false,
    };

    // 副本 A 登记瞬态失败
    engine::key_health::failure(&replica_a, &cand, "upstream_5xx", KeyFailure::Transient).await;
    let failed_after: i32 =
        sqlx::query_scalar("SELECT failed_count FROM channel_keys WHERE id = $1")
            .bind(channel_key_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(failed_after, 1);
    assert!(
        !replica_b.health.key_failures.contains_key(&channel_key_id),
        "副本 B 本地缓存不应提前包含该 key"
    );

    // 副本 B 跑成功：应通过集群脏集感知并清零 PG failed_count
    engine::key_health::success(&replica_b, &cand).await;
    let failed_cleared: i32 =
        sqlx::query_scalar("SELECT failed_count FROM channel_keys WHERE id = $1")
            .bind(channel_key_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(
        failed_cleared, 0,
        "副本 B 成功响应必须跨副本清零 PG 连续失败计数"
    );
    let dirty = replica_b
        .sched
        .health_dirty_keys()
        .await
        .unwrap_or_default();
    assert!(
        !dirty.contains(&channel_key_id),
        "清零后 Redis 集群脏集也应同步移除"
    );
}

#[tokio::test]
async fn routing_invalidate_broadcast_clears_margin_cache_across_replicas() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let nats_url = std::env::var("OKAPI_NATS_URL").ok();
    if nats_url.is_none() {
        assert!(
            std::env::var("OKAPI_REQUIRE_SERVICES").ok().as_deref() != Some("1"),
            "OKAPI_REQUIRE_SERVICES=1 时必须提供 OKAPI_NATS_URL"
        );
        return;
    }

    let replica_a =
        engine::build_state(&database_url, &redis_url, "gw-a", None, nats_url.as_deref())
            .await
            .unwrap();
    let replica_b =
        engine::build_state(&database_url, &redis_url, "gw-b", None, nats_url.as_deref())
            .await
            .unwrap();
    engine::spawn_routing_invalidate_subscriber(replica_b.clone());
    // 给订阅建立留出短暂窗口
    tokio::time::sleep(Duration::from_millis(150)).await;

    replica_b
        .routing
        .margin_cache
        .insert(0, Arc::new(HashSet::from(["default|99".to_owned()])))
        .await;
    assert!(replica_b.routing.margin_cache.get(&0).await.is_some());

    replica_a.invalidate_routing_caches();

    let mut cleared = false;
    for _ in 0..40 {
        if replica_b.routing.margin_cache.get(&0).await.is_none() {
            cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        cleared,
        "routing.invalidate 广播必须即时失效其它副本的 margin_cache"
    );
}
