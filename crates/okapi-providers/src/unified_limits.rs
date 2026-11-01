//! Anthropic 订阅（OAuth）响应头 `anthropic-ratelimit-unified-*`：各额度窗口的已用比例、重置时刻与状态。
//!
//! 只读的传输元数据：解析后交给网关挂上的响应观测钩子（[`observe_responses`]），写进请求诊断、
//! 喂给额度预判；不参与重试 / 冷却的判定（那在 `retry_after`）。成功与失败的响应都上报。
//! 头出现在哪些响应上、小数的口径，以线上诊断里的实际值为准——所以这里只做防御性解析：
//! 读不出来的字段缺席，不猜，也不让一个畸形头影响推理。

use reqwest::header::HeaderMap;
use serde::Serialize;
use std::{future::Future, panic::AssertUnwindSafe, sync::Arc};

/// 头里的窗口别名 → (与额度轮询同名的规范名, 窗口秒数)。
const WINDOWS: [(&str, &str, i64); 2] = [("5h", "five_hour", 18_000), ("7d", "seven_day", 604_800)];
/// 状态 / 声明类字段只收短小的小写标识，免得上游怪头原样进日志。
const MAX_TOKEN_LEN: usize = 32;
/// 已用比例的合理上界（用尽 = 1.0；超过 10 倍当畸形值丢弃）。
const MAX_UTILIZATION: f64 = 10.0;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowLimit {
    /// 与额度轮询同名：`five_hour` / `seven_day`。
    pub name: &'static str,
    pub window_secs: i64,
    /// 已用比例：1.0 = 用尽（头里是小数，不是百分数）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utilization: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// `allowed` / `allowed_warning` / `rejected` 之类。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct UnifiedLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// 当前决定放行与否的是哪个窗口（`five_hour` / `seven_day` …）。
    #[serde(rename = "claim", skip_serializing_if = "Option::is_none")]
    pub representative_claim: Option<String>,
    /// `anthropic-ratelimit-unified-reset`：代表窗口的重置时刻（Unix 秒）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<i64>,
    #[serde(rename = "w", skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<WindowLimit>,
}

impl UnifiedLimits {
    /// 从响应头读出订阅限额信息；一个相关的头都没有（API key 渠道、别家上游）返回 None。
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
        };
        let token = |name: &str| text(name).and_then(sanitize_token);
        let epoch = |name: &str| {
            text(name)
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|at| *at > 0)
        };
        let mut limits = Self {
            status: token("anthropic-ratelimit-unified-status"),
            representative_claim: token("anthropic-ratelimit-unified-representative-claim"),
            reset: epoch("anthropic-ratelimit-unified-reset"),
            windows: Vec::new(),
        };
        for (alias, name, window_secs) in WINDOWS {
            let window = WindowLimit {
                name,
                window_secs,
                utilization: text(&format!("anthropic-ratelimit-unified-{alias}-utilization"))
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && (0.0..=MAX_UTILIZATION).contains(value)),
                resets_at: epoch(&format!("anthropic-ratelimit-unified-{alias}-reset")),
                status: token(&format!("anthropic-ratelimit-unified-{alias}-status")),
            };
            if window.utilization.is_some() || window.resets_at.is_some() || window.status.is_some()
            {
                limits.windows.push(window);
            }
        }
        let present = limits.status.is_some()
            || limits.representative_claim.is_some()
            || limits.reset.is_some()
            || !limits.windows.is_empty();
        present.then_some(limits)
    }

    /// 某个窗口的读数（按窗口秒数找）。
    #[must_use]
    pub fn window(&self, window_secs: i64) -> Option<&WindowLimit> {
        self.windows.iter().find(|w| w.window_secs == window_secs)
    }
}

fn sanitize_token(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_TOKEN_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-')))
    .then(|| value.to_owned())
}

/// 一次上游响应的元数据。
pub struct ResponseMeta<'a> {
    pub status: u16,
    pub limits: &'a UnifiedLimits,
}

/// 响应观测钩子：同步、要轻（写诊断、把活丢进后台任务）。
pub type Sink = Arc<dyn Fn(&ResponseMeta<'_>) + Send + Sync>;

tokio::task_local! { static SINK: Sink; }

/// 在 `future` 期间挂上观测钩子：其中所有 Anthropic 订阅响应（成功与失败）都会报给 `sink`。
/// 没有挂钩子的调用方不付任何代价。
pub async fn observe_responses<T>(sink: Sink, future: impl Future<Output = T>) -> T {
    SINK.scope(sink, future).await
}

/// 传输层在拿到响应头后调用。钩子里的 panic 不许影响推理，当场吞掉。
pub(crate) fn report(status: u16, headers: &HeaderMap) {
    let _ = SINK.try_with(|sink| {
        if let Some(limits) = UnifiedLimits::from_headers(headers) {
            let meta = ResponseMeta {
                status,
                limits: &limits,
            };
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| sink(&meta)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Seen = Arc<Mutex<Vec<(u16, Option<f64>)>>>;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    fn full() -> HeaderMap {
        headers(&[
            ("anthropic-ratelimit-unified-status", "allowed"),
            (
                "anthropic-ratelimit-unified-representative-claim",
                "five_hour",
            ),
            ("anthropic-ratelimit-unified-reset", "1793512800"),
            ("anthropic-ratelimit-unified-5h-utilization", "0.37"),
            ("anthropic-ratelimit-unified-5h-reset", "1793512800"),
            ("anthropic-ratelimit-unified-5h-status", "allowed"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.07"),
            ("anthropic-ratelimit-unified-7d-reset", "1793937599"),
            ("anthropic-ratelimit-unified-7d-status", "allowed_warning"),
        ])
    }

    #[test]
    fn reads_every_window_and_the_representative_claim() {
        let limits = UnifiedLimits::from_headers(&full()).unwrap();
        assert_eq!(limits.status.as_deref(), Some("allowed"));
        assert_eq!(limits.representative_claim.as_deref(), Some("five_hour"));
        assert_eq!(limits.reset, Some(1_793_512_800));
        let five = limits.window(18_000).unwrap();
        assert_eq!(five.name, "five_hour");
        assert_eq!(five.utilization, Some(0.37));
        assert_eq!(five.resets_at, Some(1_793_512_800));
        let week = limits.window(604_800).unwrap();
        assert_eq!(week.name, "seven_day");
        assert_eq!(week.status.as_deref(), Some("allowed_warning"));
        assert!(limits.window(3600).is_none());
    }

    #[test]
    fn headers_of_other_upstreams_are_not_a_subscription() {
        assert!(UnifiedLimits::from_headers(&HeaderMap::new()).is_none());
        // API key 渠道的常规限流头不是订阅窗口
        let api_key = headers(&[
            ("anthropic-ratelimit-requests-remaining", "99"),
            ("anthropic-ratelimit-tokens-reset", "2026-01-01T00:00:00Z"),
            ("retry-after", "3"),
        ]);
        assert!(UnifiedLimits::from_headers(&api_key).is_none());
    }

    #[test]
    fn malformed_values_are_dropped_instead_of_guessed() {
        let limits = UnifiedLimits::from_headers(&headers(&[
            ("anthropic-ratelimit-unified-status", "ALLOWED; drop table"),
            ("anthropic-ratelimit-unified-5h-utilization", "NaN"),
            ("anthropic-ratelimit-unified-5h-reset", "-5"),
            ("anthropic-ratelimit-unified-7d-utilization", "1e9"),
            ("anthropic-ratelimit-unified-7d-status", "rejected"),
        ]))
        .unwrap();
        assert_eq!(limits.status, None, "含空格 / 大写 / 分号的状态不收");
        assert!(
            limits.window(18_000).is_none(),
            "一个有效字段都没有的窗口整个不出现"
        );
        let week = limits.window(604_800).unwrap();
        assert_eq!(week.utilization, None, "超出合理上界的比例丢弃");
        assert_eq!(week.status.as_deref(), Some("rejected"));
        for bad in ["-0.1", "inf", "", "0.3.4", "1,5"] {
            let parsed = UnifiedLimits::from_headers(&headers(&[
                ("anthropic-ratelimit-unified-status", "allowed"),
                ("anthropic-ratelimit-unified-5h-utilization", bad),
            ]))
            .unwrap();
            assert!(parsed.window(18_000).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn serialization_is_compact_and_omits_unknowns() {
        let limits = UnifiedLimits::from_headers(&full()).unwrap();
        let value = serde_json::to_value(&limits).unwrap();
        assert_eq!(value["claim"], "five_hour");
        assert_eq!(value["w"][0]["name"], "five_hour");
        assert_eq!(value["w"][0]["utilization"], 0.37);
        let sparse = UnifiedLimits::from_headers(&headers(&[(
            "anthropic-ratelimit-unified-status",
            "rejected",
        )]))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&sparse).unwrap(),
            serde_json::json!({"status": "rejected"})
        );
    }

    #[tokio::test]
    async fn the_sink_sees_only_subscription_responses_inside_its_scope() {
        let seen: Seen = Arc::default();
        let recorder = Arc::clone(&seen);
        let sink: Sink = Arc::new(move |meta: &ResponseMeta<'_>| {
            recorder.lock().unwrap().push((
                meta.status,
                meta.limits.window(18_000).and_then(|w| w.utilization),
            ));
        });
        // 没有挂钩子：什么也不发生
        report(200, &full());
        observe_responses(sink, async {
            report(200, &full());
            report(429, &full());
            report(200, &HeaderMap::new());
        })
        .await;
        report(200, &full());
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(200, Some(0.37)), (429, Some(0.37))]
        );
    }

    #[tokio::test]
    async fn a_panicking_sink_cannot_break_the_request() {
        let sink: Sink = Arc::new(|_| panic!("observer bug"));
        let value = observe_responses(sink, async {
            report(200, &full());
            7
        })
        .await;
        assert_eq!(value, 7);
    }
}
