//! 规则修饰器栈的运行期输入采集（DESIGN §3.4）。
//!
//! 两个输入都由价簿内是否存在该类启用规则门控——站点没配 volume/surge 规则时
//! 热路径不产生任何额外 Redis 往返、不包装响应体，与 service_tier 的 `has_tiers`
//! 门控同构：
//! - volume → Redis `tok:{uid}:<yyyymm>`（docs/database.md §2.1）
//! - surge  → **集群**在途请求数 vs `settings.surge_inflight_threshold`

use super::state::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use futures::Stream;
use okapi_pricing::PriceBook;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::task::{Context, Poll};

/// 一次报价所需的规则触发输入。
#[derive(Debug, Clone, Copy, Default)]
pub struct RuleInputs {
    pub monthly_tokens: u64,
    /// 本月累计消费 micro（volume 规则消费额轴；仅价簿含该类阈值时采集）。
    pub monthly_spend_micro: u64,
    pub surge_active: bool,
}

/// 采集本次请求的规则输入（无对应规则时零 IO）。
pub async fn collect(state: &AppState, book: &PriceBook, user_id: i64) -> RuleInputs {
    let monthly_tokens = if book.has_volume_rules() {
        state.sched.monthly_tokens_get(user_id).await
    } else {
        0
    };
    let monthly_spend_micro = if book.has_spend_rules() {
        state.sched.monthly_spend_get(user_id).await
    } else {
        0
    };
    let surge_active = if book.has_surge_rules() {
        surge_active(state).await
    } else {
        false
    };
    RuleInputs {
        monthly_tokens,
        monthly_spend_micro,
        surge_active,
    }
}

/// 结算后累加本月 token 与消费（各自仅在存在对应规则时写，
/// 避免给未用该能力的站点留垃圾键）。
pub async fn record_tokens(state: &AppState, user_id: i64, tokens: u64, amount_micro: i64) {
    let book = state.pricebook.load();
    if book.has_volume_rules() {
        state.sched.monthly_tokens_add(user_id, tokens).await;
    }
    if book.has_spend_rules() {
        state.sched.monthly_spend_add(user_id, amount_micro).await;
    }
}

/// surge 判定用**集群**在途量，不是本进程的。
///
/// 此前读的是进程内 `AtomicI64`：阈值配 100，单 pod 部署时是"集群 100 并发触发"，
/// 扩到 10 个 pod 就变成"要 1000 并发才触发"，而且只对恰好落在撞线那个 pod 上的请求加价。
/// surge 是**计价**规则不是限流——同一份配置在不同副本数下收不同的钱、同一时刻不同用户
/// 按运气收不同的价，账单没法解释。改读 Redis 量表后阈值语义与副本数无关。
async fn surge_active(state: &AppState) -> bool {
    let threshold = state
        .setting_cached("surge_inflight_threshold")
        .await
        .as_ref()
        .as_ref()
        .and_then(serde_json::Value::as_i64)
        .filter(|v| *v > 0);
    let Some(threshold) = threshold else {
        return false;
    };
    state.sched.inflight_total().await >= threshold
}

/// 在途计数中间件：价簿无 surge 规则时直接放行（不计数、不包装响应体）。
pub async fn track_in_flight(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !state.pricebook.load().has_surge_rules() {
        return next.run(req).await;
    }
    let guard = InFlightGuard::enter(Arc::clone(&state.in_flight));
    report_inflight(&state).await;
    let resp = next.run(req).await;
    // 流式响应在 handler 返回后才真正占用资源，计数必须活到响应体读完
    let (parts, body) = resp.into_parts();
    let guarded = GuardedBody {
        inner: Box::pin(body.into_data_stream()),
        _guard: guard,
        state,
        done: false,
    };
    Response::from_parts(parts, Body::from_stream(guarded))
}

/// 上报节流：每秒至多一次，但"降到 0"一定立即上报——否则实例空下来之后，
/// 量表里还挂着它最后那个非零读数直到过期，surge 会凭空多加价好几秒。
async fn report_inflight(state: &AppState) {
    let count = state.in_flight.load(Ordering::Relaxed);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let last = state.surge_reported_at.load(Ordering::Relaxed);
    if count > 0 && now_ms.saturating_sub(last) < 1000 {
        return;
    }
    state.surge_reported_at.store(now_ms, Ordering::Relaxed);
    state.sched.inflight_report(&state.node, count).await;
}

struct InFlightGuard(Arc<AtomicI64>);

impl InFlightGuard {
    fn enter(counter: Arc<AtomicI64>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 持有计数守卫直到响应体流结束（含客户端中断——Drop 一样触发）。
struct GuardedBody {
    inner: Pin<Box<axum::body::BodyDataStream>>,
    _guard: InFlightGuard,
    state: AppState,
    done: bool,
}

impl Stream for GuardedBody {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let polled = self.inner.as_mut().poll_next(cx);
        // 流结束（正常读完或客户端中断）时补一次上报：Drop 里 await 不了，
        // 只能在这最后一次 poll 里把"我这边空下来了"告诉量表
        if matches!(polled, Poll::Ready(None)) && !self.done {
            self.done = true;
            let state = self.state.clone();
            tokio::spawn(async move { report_inflight(&state).await });
        }
        polled
    }
}
