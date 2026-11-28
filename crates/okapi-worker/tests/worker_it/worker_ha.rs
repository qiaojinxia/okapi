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

    // 3. 跨副本共享的维护时间戳、对账检查点与扫描游标
    let ts_job = format!("archive_test_{}", Uuid::new_v4().simple());
    assert_eq!(sched.maint_timestamp_get(&ts_job).await, None);
    sched.maint_timestamp_set(&ts_job, 1_700_001_000, 300).await;
    assert_eq!(
        sched.maint_timestamp_get(&ts_job).await,
        Some(1_700_001_000)
    );
    // 较旧时钟不能回退较新时间戳
    sched.maint_timestamp_set(&ts_job, 1_700_000_500, 300).await;
    assert_eq!(
        sched.maint_timestamp_get(&ts_job).await,
        Some(1_700_001_000)
    );

    let cursor_job = format!("oauth_cursor_{}", Uuid::new_v4().simple());
    sched.maint_cursor_set(&cursor_job, 42, 300).await;
    assert_eq!(sched.maint_cursor_get(&cursor_job).await, Some(42));
    // 扫描游标允许回绕归零
    sched.maint_cursor_set(&cursor_job, 0, 300).await;
    assert_eq!(sched.maint_cursor_get(&cursor_job).await, Some(0));

    // 对账检查点：全量推进 last_full + last_pass；后续增量只推进 last_pass 保留 last_full
    let now_ts = chrono::Utc::now().timestamp();
    sched.maint_reconcile_clock_set(now_ts, true).await;
    sched.maint_reconcile_clock_set(now_ts + 300, false).await;
    let cp = sched
        .maint_reconcile_clock_get()
        .await
        .expect("reconcile checkpoint written");
    assert!(cp.last_full_unix.is_some_and(|v| v >= now_ts));
    assert!(cp.last_pass_unix.is_some_and(|v| v >= now_ts + 300));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
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
        request_overrides: serde_json::Value::Null,
        status_code_mapping: serde_json::Value::Null,
        advanced_routes: serde_json::Value::Null,
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
        pool_role: "primary".to_owned(),
        pool_tags: Vec::new(),
        pool_model_filter: None,
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
    replica_b
        .routing
        .cand_cache
        .insert("gpt-5|default".to_owned(), Arc::new(Vec::new()))
        .await;
    replica_b
        .routing
        .model_cache
        .insert("gpt-5".to_owned(), Arc::new(None))
        .await;
    replica_b
        .settings_cache
        .insert("site_notices".to_owned(), Arc::new(None))
        .await;

    // 1. Margin 作用域广播仅失效 margin_cache，保留 cand_cache / model_cache / settings_cache
    replica_a.invalidate_cache(engine::state::CacheInvalidation::Margin);
    let mut margin_cleared = false;
    for _ in 0..40 {
        if replica_b.routing.margin_cache.get(&0).await.is_none() {
            margin_cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        margin_cleared,
        "Margin 作用域广播必须即时失效其它副本的 margin_cache"
    );
    assert!(replica_b.routing.cand_cache.get("gpt-5|default").await.is_some());
    assert!(replica_b.routing.model_cache.get("gpt-5").await.is_some());
    assert!(replica_b.settings_cache.get("site_notices").await.is_some());

    // 2. Setting 作用域广播仅失效目标 settings 键，保留路由缓存
    replica_a.invalidate_setting_cache("site_notices").await;
    let mut setting_cleared = false;
    for _ in 0..40 {
        if replica_b.settings_cache.get("site_notices").await.is_none() {
            setting_cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        setting_cleared,
        "Setting 作用域广播必须即时失效其它副本的对应设置键"
    );
    assert!(replica_b.routing.cand_cache.get("gpt-5|default").await.is_some());
    assert!(replica_b.routing.model_cache.get("gpt-5").await.is_some());

    // 3. 全量广播（All）仍清除全部路由缓存
    replica_a.invalidate_routing_caches();
    let mut all_cleared = false;
    for _ in 0..40 {
        if replica_b.routing.cand_cache.get("gpt-5|default").await.is_none()
            && replica_b.routing.model_cache.get("gpt-5").await.is_none()
        {
            all_cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        all_cleared,
        "全量 routing.invalidate 广播必须失效其它副本的路由缓存"
    );
}

#[tokio::test]
async fn revision_sync_invalidates_scoped_caches_without_nats() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");

    // 显式关闭 NATS：仅靠 Redis 单调 revision 游标实现跨副本按作用域收敛
    let replica_a = engine::build_state(&database_url, &redis_url, "gw-rev-a", None, None)
        .await
        .unwrap();
    let replica_b = engine::build_state(&database_url, &redis_url, "gw-rev-b", None, None)
        .await
        .unwrap();
    engine::spawn_revision_sync_with_interval(replica_b.clone(), Duration::from_millis(80));
    // 等待启动基线采完
    for _ in 0..40 {
        if replica_b
            .routing
            .sync_cursor
            .lock()
            .expect("lock")
            .is_seeded()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    replica_b
        .routing
        .margin_cache
        .insert(0, Arc::new(HashSet::from(["default|77".to_owned()])))
        .await;
    replica_b
        .routing
        .cand_cache
        .insert("gpt-5|default".to_owned(), Arc::new(Vec::new()))
        .await;
    replica_b
        .routing
        .model_cache
        .insert("gpt-5".to_owned(), Arc::new(None))
        .await;
    replica_b
        .settings_cache
        .insert("site_notices".to_owned(), Arc::new(None))
        .await;

    // 1. Margin 作用域仅清除 margin_cache
    replica_a.invalidate_cache(engine::state::CacheInvalidation::Margin);
    let mut margin_cleared = false;
    for _ in 0..40 {
        if replica_b.routing.margin_cache.get(&0).await.is_none() {
            margin_cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        margin_cleared,
        "无 NATS 时 Margin revision 必须同步失效其它副本的 margin_cache"
    );
    assert!(replica_b.routing.cand_cache.get("gpt-5|default").await.is_some());
    assert!(replica_b.routing.model_cache.get("gpt-5").await.is_some());
    assert!(replica_b.settings_cache.get("site_notices").await.is_some());

    // 2. Setting 作用域仅清除目标设置键
    replica_a.invalidate_setting_cache("site_notices").await;
    let mut setting_cleared = false;
    for _ in 0..40 {
        if replica_b.settings_cache.get("site_notices").await.is_none() {
            setting_cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        setting_cleared,
        "无 NATS 时 Setting revision 必须同步失效目标设置键"
    );
    assert!(replica_b.routing.cand_cache.get("gpt-5|default").await.is_some());
    assert!(replica_b.routing.model_cache.get("gpt-5").await.is_some());

    // 3. All 作用域全量失效路由缓存
    replica_a.invalidate_routing_caches();
    let mut all_cleared = false;
    for _ in 0..40 {
        if replica_b.routing.cand_cache.get("gpt-5|default").await.is_none()
            && replica_b.routing.model_cache.get("gpt-5").await.is_none()
        {
            all_cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        all_cleared,
        "无 NATS 时 All revision 必须全量失效其它副本路由缓存"
    );
}

#[tokio::test]
async fn gateway_exposes_prometheus_metrics_and_propagates_w3c_traceparent() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let state = okapi_gateway::build_state(&database_url, &redis_url, "gw-obs", None, None)
        .await
        .unwrap();
    let app = okapi_gateway::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let client = reqwest::Client::new();
    let incoming_trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    let incoming_parent = "00f067aa0ba902b7";
    let health = client
        .get(format!("http://{addr}/healthz"))
        .header(
            "traceparent",
            format!("00-{incoming_trace}-{incoming_parent}-01"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    let echoed = health
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .expect("response must include traceparent");
    let (trace_id, span_id, flags) =
        okapi_kernel::trace_context::parse_traceparent(echoed).expect("valid W3C traceparent");
    assert_eq!(trace_id, incoming_trace);
    assert_ne!(span_id, incoming_parent);
    assert_eq!(flags, 1);

    let metrics = client
        .get(format!("http://{addr}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(metrics.status(), 200);
    assert_eq!(
        metrics
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(okapi_kernel::metrics::PROMETHEUS_CONTENT_TYPE)
    );
    let body = metrics.text().await.unwrap();
    assert!(body.contains(
        "okapi_http_requests_total{role=\"gateway\",method=\"GET\",route=\"/healthz\",status_class=\"2xx\"}"
    ));
    assert!(body.contains("okapi_pg_pool_connections{role=\"gateway\",state=\"total\"}"));
    assert!(body.contains("okapi_pricebook_epoch "));
    assert!(body.contains("okapi_settlement_backlog{role=\"gateway\"}"));
}

#[tokio::test]
async fn http_data_plane_bridge_relays_to_remote_gateway_with_traceparent() {
    use okapi_engine::bridge::{DataPlaneBridge as _, HttpDataPlaneBridge};
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let gw_state = okapi_gateway::build_state(&database_url, &redis_url, "gw-remote", None, None)
        .await
        .unwrap();
    let app = okapi_gateway::router(gw_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // 独立 console 状态（不经 gateway::build_state，挂 HttpDataPlaneBridge）
    let console_state = engine::build_state(&database_url, &redis_url, "console-only", None, None)
        .await
        .unwrap();
    let bridge = HttpDataPlaneBridge::new(&format!("http://{addr}")).unwrap();

    let incoming_trace = "0af7651916cd43dd8448eb211c80319c";
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "traceparent",
        axum::http::HeaderValue::from_str(&format!("00-{incoming_trace}-b7ad6b7169203331-01"))
            .unwrap(),
    );
    let resp = bridge
        .chat_completions(
            console_state,
            headers,
            axum::body::Bytes::from_static(br#"{"model":"gpt-5","messages":[]}"#),
        )
        .await;
    // 无 API key 时由远端 gateway 返回标准 401，并透传 traceparent 与 x-okapi-request-id
    assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    let echoed = resp
        .headers()
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .expect("bridge must forward gateway traceparent");
    let (trace_id, _, _) =
        okapi_kernel::trace_context::parse_traceparent(echoed).expect("valid traceparent");
    assert_eq!(trace_id, incoming_trace);
    assert!(
        resp.headers().contains_key("x-okapi-request-id"),
        "bridge must forward x-okapi-request-id"
    );
}
