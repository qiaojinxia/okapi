//! 多副本（pod 横向扩展）语义验收（IMPLEMENTATION §11.23）。
//!
//! 决定正确性的状态本来就都在 Redis（预扣/限额/并发槽/粘性/鉴权/WS 租约），gateway 与
//! console 本身是无状态的。但有两处**进程内假设**会随副本数变味：
//!
//! 1. surge 加价的负载输入曾是进程内 `AtomicI64`——阈值配 100，单 pod 是"集群 100 并发
//!    触发"，10 个 pod 就变成"要 1000 并发才触发"，且只对撞线那个 pod 上的请求加价。
//!    这是**计价**规则，同一份配置在不同副本数下收不同的钱。
//! 2. 管理面改渠道/池/模型/设置后只失效**本进程**缓存，其余 pod 得等各自 TTL
//!    （候选集 5s、模型 60s、settings 60s）——"禁用一条出问题的渠道"要等近一分钟才全网生效。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::gateway;
use okapi_store::channels::ResolvedModel;
use uuid::Uuid;

async fn build() -> gateway::state::AppState {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let nats = std::env::var("OKAPI_NATS_URL").ok();
    gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        None,
        nats.as_deref(),
    )
    .await
    .unwrap()
}

/// 集群在途量表：各实例只写自己那格，读侧求和。
///
/// 断言用**增量**而非绝对值——量表是全局共享的一张 hash，并行跑的别的用例也可能在写；
/// 拿绝对值断言就是又一个"测试互相污染"的坑。
#[tokio::test]
async fn inflight_gauge_sums_across_instances() {
    let state = build().await;
    let sched = &state.sched;
    let a = format!("pod-a-{}", Uuid::new_v4().simple());
    let b = format!("pod-b-{}", Uuid::new_v4().simple());

    let base = sched.inflight_total().await;

    // 两个"实例"各自上报 → 合计要把两边都算进去（此前只看得见自己那份）
    sched.inflight_report(&a, 5).await;
    sched.inflight_report(&b, 7).await;
    let both = sched.inflight_total().await;
    assert_eq!(both - base, 12, "集群在途 = 各实例之和");

    // 一个实例空下来 → 立刻从合计里退出（否则 surge 会凭空多加价）
    sched.inflight_report(&a, 0).await;
    let after = sched.inflight_total().await;
    assert_eq!(after - base, 7);

    // 负数不该把别人的量吃掉（防御脏值）
    sched.inflight_report(&a, -100).await;
    assert_eq!(sched.inflight_total().await - base, 7);

    // 收尾：把本用例的格子清零，不给别的用例留底数
    sched.inflight_report(&a, 0).await;
    sched.inflight_report(&b, 0).await;
    assert_eq!(sched.inflight_total().await, base);
}

/// 路由缓存失效广播：一个实例改配置，另一个实例立刻弃用旧缓存（不等 TTL）。
#[tokio::test]
async fn routing_invalidate_broadcasts_across_instances() {
    let publisher = build().await;
    let subscriber = build().await;
    if publisher.nats.is_none() {
        eprintln!("跳过：未配置 OKAPI_NATS_URL（无广播通道，退化为 TTL 兜底）");
        return;
    }
    gateway::spawn_routing_invalidate_subscriber(subscriber.clone());
    // 订阅建立需要一个来回
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // 给"另一个 pod"的缓存塞一条，模拟它正拿着旧配置服务
    let model = format!("mp-{}", Uuid::new_v4().simple());
    subscriber
        .model_cache
        .insert(
            model.clone(),
            std::sync::Arc::new(Some(ResolvedModel {
                canonical: model.clone(),
                max_output: None,
                fallback_models: Vec::new(),
            })),
        )
        .await;
    assert!(
        subscriber.model_cache.get(&model).await.is_some(),
        "先确认缓存里确实有"
    );

    // 管理员在"这个 pod"上改了配置
    publisher.invalidate_routing_caches();

    // 另一个 pod 应在广播到达后立刻清掉（而不是等 60s TTL）
    let mut cleared = false;
    for _ in 0..40 {
        if subscriber.model_cache.get(&model).await.is_none() {
            cleared = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(cleared, "改配置应经广播让其它实例立即弃用旧缓存");
}
