//! 缩尺压测器（IMPLEMENTATION §12.1 对标用）：
//! 自包含：seed（用户/key/模型/渠道）→ 内置极速 mock 上游 → 并发打 gateway
//! /v1/chat/completions（非流式与流式）→ RPS + 延迟分位。
//!
//! 用法：
//!   cargo build --release --bin okapi && ./target/release/okapi gateway &
//!   cargo run --release --example loadgen -- [并发] [秒] [stream]
//! 结果一行 JSON（docs/perf-report.md 收录）。

use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use uuid::Uuid;

const GATEWAY: &str = "http://127.0.0.1:8080";

#[tokio::main(flavor = "multi_thread")]
// 压测脚本：线性场景一体（展示统计层允许浮点/精度损失，非计费路径）
#[allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::cast_precision_loss
)]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().collect();
    let concurrency: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(64);
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
    let stream = args.get(3).map(String::as_str) == Some("stream");
    let hold = args.get(3).map(String::as_str) == Some("hold");
    let baseline = args.get(3).map(String::as_str) == Some("baseline");

    // ---- seed ----
    let database_url = std::env::var("DATABASE_URL")?;
    let pg = okapi_store::connect_pg(&database_url).await?;
    okapi_store::run_migrations(&pg).await?;
    let suffix = Uuid::new_v4().simple().to_string();
    let model = format!("bench-{}", &suffix[..10]);
    let user_id = okapi_store::provision::create_user(&pg, &format!("bench-{suffix}")).await?;
    let token = format!("sk-okapi-bench-{suffix}");
    let key_hash = hex::encode(Sha256::digest(token.as_bytes()));
    okapi_store::provision::create_api_key(&pg, user_id, &key_hash, "sk-okapi-bench").await?;
    okapi_store::provision::create_model_ratio(&pg, &model, "1", "1", "1").await?;

    // ---- 内置 mock 上游（零逻辑，衡量网关自身开销） ----
    let mock_app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|body: axum::body::Bytes| async move {
            let is_stream = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("stream").and_then(serde_json::Value::as_bool))
                .unwrap_or(false);
            let want_hold = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("x_hold").and_then(serde_json::Value::as_bool))
                .unwrap_or(false);
            if want_hold {
                // SSE 持有专项：首字立即给（过 gateway 首字窗口），随后无限心跳慢流
                let stream = futures::stream::unfold(0u64, |i| async move {
                    if i == 0 {
                        return Some((
                            Ok::<_, std::convert::Infallible>(
                                "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hold\"}}]}\n\n".to_owned(),
                            ),
                            i + 1,
                        ));
                    }
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    Some((
                        Ok(
                            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\".\"}}]}\n\n".to_owned(),
                        ),
                        i + 1,
                    ))
                });
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    axum::body::Body::from_stream(stream),
                )
                    .into_response()
            } else if is_stream {
                let sse = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n\
                           data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n\
                           data: {\"choices\":[],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":2}}\n\n\
                           data: [DONE]\n\n";
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse,
                )
                    .into_response()
            } else {
                axum::Json(json!({
                    "id":"b","object":"chat.completion",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"ok"}}],
                    "usage":{"prompt_tokens":20,"completion_tokens":2}
                }))
                .into_response()
            }
        }),
    );
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let mock = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(listener, mock_app).await.ok();
    });

    okapi_store::provision::create_channel(
        &pg,
        &format!("bench-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "bench-credential",
        &[model.as_str()],
        true, // trust_upstream_usage：结算直接采信
    )
    .await?;
    // 引导余额（走 SQL 直写 Redis 不可——用 ledger）
    let redis_url = std::env::var("OKAPI_REDIS_URL")?;
    let redis = okapi_store::connect_redis(&redis_url).await?;
    let ledger = okapi_ledger::BalanceLedger::new(redis);
    ledger
        .credit(user_id, okapi_domain::Money::from_micros(1_000_000_000_000))
        .await?;

    // 新模型要进 PriceBook：发布 epoch + NATS 广播（gateway 秒级热更；无 NATS 退 30s 轮询）
    let snapshot = json!({"reason": "bench"});
    okapi_store::admin::publish_epoch(&pg, user_id, &snapshot).await?;
    if let Ok(nats_url) = std::env::var("OKAPI_NATS_URL")
        && let Ok(nc) = async_nats::connect(&nats_url).await
    {
        let epoch = sqlx::query_scalar!(r#"SELECT MAX(epoch) AS "e!" FROM pricing_epochs"#)
            .fetch_one(&pg)
            .await?;
        let _ = nc.publish("pricing.epoch", epoch.to_string().into()).await;
    }

    // gateway 就绪 + 预热（等 PriceBook 拾取 bench 模型）
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(concurrency * 2)
        .build()?;
    let warm_body = json!({
        "model": model, "max_tokens": 8,
        "messages": [{"role":"user","content":"warmup"}]
    });
    let mut ready = false;
    for _ in 0..200 {
        let resp = client
            .post(format!("{GATEWAY}/v1/chat/completions"))
            .bearer_auth(&token)
            .json(&warm_body)
            .send()
            .await;
        if let Ok(r) = resp
            && r.status().is_success()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        ready,
        "gateway 预热失败（确认已起 ./target/release/okapi gateway）"
    );

    // ---- SSE 持有专项（hold 模式）：分批建 N 条 SSE 长连接持有 T 秒，观测稳定性 ----
    if hold {
        return run_hold(&client, &token, &model, concurrency, secs).await;
    }

    // ---- 压测 ----
    // baseline 档：直打 mock（绕过 gateway），差值即网关自身开销口径
    let target = if baseline {
        format!("http://{mock}/v1")
    } else {
        format!("{GATEWAY}/v1")
    };
    let deadline = Instant::now() + Duration::from_secs(secs);
    let ok = Arc::new(AtomicU64::new(0));
    let err = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..concurrency {
        let client = client.clone();
        let token = token.clone();
        let model = model.clone();
        let target = target.clone();
        let ok = Arc::clone(&ok);
        let err = Arc::clone(&err);
        handles.push(tokio::spawn(async move {
            let mut lat_us: Vec<u64> = Vec::with_capacity(4096);
            let body = json!({
                "model": model,
                "stream": stream,
                "max_tokens": 16,
                "messages": [{"role":"user","content":"benchmark prompt with some tokens"}]
            });
            while Instant::now() < deadline {
                let t0 = Instant::now();
                let resp = client
                    .post(format!("{target}/chat/completions"))
                    .bearer_auth(&token)
                    .json(&body)
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        // 流式需读完 body 才算完成
                        let _ = r.bytes().await;
                        lat_us.push(u64::try_from(t0.elapsed().as_micros()).unwrap_or(u64::MAX));
                        ok.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {
                        err.fetch_add(1, Ordering::Relaxed);
                        // 错误退避：防 429/失败忙循环打满 CPU
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
            lat_us
        }));
    }
    let started = Instant::now();
    let mut all: Vec<u64> = Vec::new();
    for h in handles {
        all.extend(h.await?);
    }
    let elapsed = started.elapsed().as_secs_f64();
    all.sort_unstable();
    let pct = |p: f64| -> u64 {
        if all.is_empty() {
            return 0;
        }
        // 展示统计允许浮点（非计费路径）
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let idx = ((all.len() as f64 - 1.0) * p) as usize;
        all[idx]
    };
    let total_ok = ok.load(Ordering::Relaxed);
    println!(
        "{}",
        json!({
            "mode": if baseline { "baseline" } else if stream { "stream" } else { "json" },
            "concurrency": concurrency,
            "duration_secs": secs,
            "requests_ok": total_ok,
            "requests_err": err.load(Ordering::Relaxed),
            // 展示统计（非计费路径）
            "rps": format!("{:.0}", f64::from(u32::try_from(total_ok).unwrap_or(u32::MAX)) / elapsed),
            "p50_ms": format!("{:.2}", pct(0.50) as f64 / 1000.0),
            "p95_ms": format!("{:.2}", pct(0.95) as f64 / 1000.0),
            "p99_ms": format!("{:.2}", pct(0.99) as f64 / 1000.0),
        })
    );
    Ok(())
}

/// SSE 持有专项（§12.1"10 万并发 SSE 稳定持有"缩尺）：
/// 分批建立 conns 条慢流 SSE 并持有 hold_secs 秒；每 10s 报告活跃数与自身 RSS。
/// 端口约束：同机单源 IP 约 6 万 ephemeral 上限（client→gateway 与 gateway→mock 各占一份）。
#[allow(clippy::cast_precision_loss)] // 展示统计
async fn run_hold(
    client: &reqwest::Client,
    token: &str,
    model: &str,
    conns: usize,
    hold_secs: u64,
) -> anyhow::Result<()> {
    use futures::StreamExt as _;
    let active = Arc::new(AtomicU64::new(0));
    let peak = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut handles = Vec::with_capacity(conns);

    let build_started = Instant::now();
    for i in 0..conns {
        // 分批：每 1000 条歇 200ms，防 accept/预扣风暴
        if i > 0 && i % 1000 == 0 {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let client = client.clone();
        let token = token.to_owned();
        let model = model.to_owned();
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        let failed = Arc::clone(&failed);
        let stop = Arc::clone(&stop);
        handles.push(tokio::spawn(async move {
            let body = json!({
                "model": model, "stream": true, "x_hold": true, "max_tokens": 16,
                "messages": [{"role":"user","content":"hold"}]
            });
            let resp = client
                .post(format!("{GATEWAY}/v1/chat/completions"))
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await;
            let Ok(resp) = resp else {
                failed.fetch_add(1, Ordering::Relaxed);
                return;
            };
            if !resp.status().is_success() {
                failed.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let mut stream = resp.bytes_stream();
            // 首帧到达即计活跃
            if stream.next().await.is_none() {
                failed.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let now = active.fetch_add(1, Ordering::Relaxed) + 1;
            peak.fetch_max(now, Ordering::Relaxed);
            // 持有：继续消费心跳帧直到 stop
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // 断流/读错误终结；帧或 1s 空转继续
                if let Ok(None | Some(Err(_))) =
                    tokio::time::timeout(Duration::from_secs(1), stream.next()).await
                {
                    break;
                }
            }
            active.fetch_sub(1, Ordering::Relaxed);
        }));
    }

    // 报告循环：建立期 + 持有期
    let deadline = Instant::now() + Duration::from_secs(hold_secs);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let rss_kb = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned))
            })
            .unwrap_or_else(|| "n/a".to_owned());
        println!(
            "{}",
            json!({
                "phase": "holding",
                "elapsed_s": build_started.elapsed().as_secs(),
                "active": active.load(Ordering::Relaxed),
                "failed": failed.load(Ordering::Relaxed),
                "loadgen_rss_kb": rss_kb,
            })
        );
    }
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        let _ = h.await;
    }
    println!(
        "{}",
        json!({
            "mode": "hold",
            "conns_requested": conns,
            "peak_active": peak.load(Ordering::Relaxed),
            "failed": failed.load(Ordering::Relaxed),
            "hold_secs": hold_secs,
        })
    );
    Ok(())
}
