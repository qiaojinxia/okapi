//! 运维监控运行时指标（`/admin/monitor/metrics` 与 `/admin/monitor/metrics/raw`）端到端测试：
//! 1. 非管理员拒绝访问（`401` / `403`）；
//! 2. Redis 无心跳时自动降级返回本进程指标（`source = "local"`），包含 Phase 2 质量降级/压缩计数与 `pool_policies` 缓存；
//! 3. 写入多节点 Redis 心跳后自动切换为集群聚合（`source = "shared"`，`node_count = 2`），正确合并 RPS/计数/水位；
//! 4. `/admin/monitor/metrics/raw` 以管理员会话返回标准 Prometheus 0.0.4 文本格式。

use fred::interfaces::KeysInterface;
use okapi_console as console;
use okapi_gateway as gateway;
use okapi_kernel::metrics::{NodeMetricsRates, RuntimeSnapshot};
use okapi_kernel::ops::metrics_sync;
use serde_json::Value;
use std::net::SocketAddr;
use std::time::Duration;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn setup() -> (okapi_engine::state::AppState, SocketAddr, String, String) {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let user_id = okapi_store::provision::create_user(&pg, &format!("mon-u-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-mon-user-{suffix}");
    sqlx::query(
        "INSERT INTO api_keys (user_id, name, key_hash, key_prefix) VALUES ($1, 'u', $2, 'sk-m')",
    )
    .bind(user_id)
    .bind(hash(&user_token))
    .execute(&pg)
    .await
    .unwrap();

    let admin_id = okapi_store::provision::create_user(&pg, &format!("mon-a-{suffix}"))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = 100 WHERE id = $1")
        .bind(admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-mon-admin-{suffix}");
    sqlx::query(
        "INSERT INTO api_keys (user_id, name, key_hash, key_prefix) VALUES ($1, 'a', $2, 'sk-a')",
    )
    .bind(admin_id)
    .bind(hash(&admin_token))
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(
        &database_url,
        &redis_url,
        &format!("node-{suffix}"),
        None,
        None,
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = console::router(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    (state, addr, admin_token, user_token)
}

async fn publish_sample_nodes(redis: &fred::clients::Client) {
    let m = okapi_kernel::metrics::global();
    let snap_gw = m.snapshot(
        "gw-pod-1",
        &RuntimeSnapshot {
            role: "gateway",
            pg_pool_size: 16,
            pg_pool_idle: 10,
            pricebook_epoch: 12,
            settle_backlog: 5,
            settle_in_flight: 2,
            settle_shedding: false,
            cache_models: 20,
            cache_candidates: 40,
            cache_channel_cost: 6,
            cache_channel_control: 4,
            cache_pool_policies: 3,
            cache_settings: 11,
        },
        NodeMetricsRates {
            gateway_rps: 12.5,
            failovers_per_min: 2.0,
            recent_gw_p95_ms: Some(140.0),
            recent_ttft_p95_ms: Some(310.0),
            ..Default::default()
        },
    );
    let snap_worker = m.snapshot(
        "worker-pod-1",
        &RuntimeSnapshot {
            role: "worker",
            pg_pool_size: 8,
            pg_pool_idle: 6,
            pricebook_epoch: 12,
            settle_backlog: 0,
            settle_in_flight: 0,
            settle_shedding: false,
            cache_models: 20,
            cache_candidates: 0,
            cache_channel_cost: 0,
            cache_channel_control: 0,
            cache_pool_policies: 1,
            cache_settings: 11,
        },
        NodeMetricsRates {
            console_rps: 1.5,
            ..Default::default()
        },
    );
    metrics_sync::publish_snapshot(redis, "gw-pod-1:gateway", &snap_gw)
        .await
        .unwrap();
    metrics_sync::publish_snapshot(redis, "worker-pod-1:worker", &snap_worker)
        .await
        .unwrap();
}

#[tokio::test]
async fn monitor_metrics_aggregates_shared_nodes_and_exposes_raw_prometheus() {
    let (state, addr, admin_token, user_token) = setup().await;
    let http = reqwest::Client::new();
    let redis = state.sched.client();
    let _: Result<i64, _> = redis.del(metrics_sync::REDIS_KEY).await;

    // 1. 普通用户无 settings.read 权限，拒绝访问
    let forbid = http
        .get(format!("http://{addr}/admin/monitor/metrics"))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbid.status(), 403);

    // 记录几条进程内指标（含调度池质量降级）
    let m = okapi_kernel::metrics::global();
    m.record_upstream_outcome("openai", "ok");
    m.record_upstream_ttft("openai", Duration::from_millis(260));
    m.record_pool_quality_event("mon-pool", "demote_error_rate");

    // 2. Redis 暂无心跳 -> 降级本进程（source = "local"）
    let local: Value = http
        .get(format!("http://{addr}/admin/monitor/metrics"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(local["source"], "local");
    assert_eq!(local["node_count"], 1);
    assert!(
        local["pool_quality_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ev| ev["pool"] == "mon-pool" && ev["action"] == "demote_error_rate")
    );

    // 3. 发布两个活跃节点快照到 Redis -> 集群聚合（source = "shared"）
    publish_sample_nodes(redis).await;

    let shared: Value = http
        .get(format!("http://{addr}/admin/monitor/metrics"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shared["source"], "shared");
    assert_eq!(shared["node_count"], 2);
    assert_eq!(shared["summary"]["gateway_rps"], 12.5);
    assert_eq!(shared["summary"]["settle_backlog"], 5);
    assert_eq!(shared["summary"]["recent_gw_p95_ms"], 140.0);
    assert_eq!(shared["nodes"].as_array().unwrap().len(), 2);

    // 4. `/admin/monitor/metrics/raw` 返回标准 Prometheus 文本
    let raw_resp = http
        .get(format!("http://{addr}/admin/monitor/metrics/raw"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(raw_resp.status(), 200);
    let ct = raw_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/plain"));
    let raw_text = raw_resp.text().await.unwrap();
    assert!(raw_text.contains(
        "okapi_pool_quality_events_total{pool=\"mon-pool\",action=\"demote_error_rate\"}"
    ));
    assert!(raw_text.contains("cache=\"pool_policies\""));

    let _: Result<i64, _> = redis.del(metrics_sync::REDIS_KEY).await;
}
