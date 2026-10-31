//! Retry timing is transport metadata; only exhausted subscription windows block a key.
use reqwest::header::HeaderMap;

pub(crate) fn seconds(headers: &HeaderMap) -> Option<i64> {
    at(headers, chrono::Utc::now().timestamp())
}

fn at(headers: &HeaderMap, now: i64) -> Option<i64> {
    let text = |name: &str| headers.get(name)?.to_str().ok().map(str::trim);
    let positive = |value: i64| (value > 0).then_some(value);
    let remaining = |reset: i64| reset.checked_sub(now).and_then(positive);
    let explicit = text("retry-after")
        .and_then(|value| {
            value.parse::<i64>().ok().filter(|n| *n >= 0).or_else(|| {
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()
                    .and_then(|date| remaining(date.timestamp()))
            })
        })
        // OpenAI / Azure OpenAI 自带的毫秒版；标准头缺席或读不出来时才看它
        .or_else(|| text("retry-after-ms").and_then(millis_to_seconds));
    // OpenAI 系：用完的是哪个维度（请求数 / token 数），就等那个维度重置。`x-ratelimit-reset-*`
    // 是 "1s" / "6m0s" 这类时长；余量没用完的维度只是在报状态，不算限流
    let usage_reset = ["requests", "tokens"]
        .into_iter()
        .filter_map(|dimension| {
            let drained = text(&format!("x-ratelimit-remaining-{dimension}"))?
                .parse::<i64>()
                .is_ok_and(|left| left == 0);
            drained
                .then(|| text(&format!("x-ratelimit-reset-{dimension}")).and_then(duration_seconds))
                .flatten()
        })
        .max();
    let mut windows = Vec::new();
    for window in ["5h", "7d"] {
        if text(&format!("anthropic-ratelimit-unified-{window}-utilization"))
            .is_some_and(|value| exhausted(value, 1))
            && let Some(delay) = text(&format!("anthropic-ratelimit-unified-{window}-reset"))
                .and_then(|value| value.parse().ok())
                .and_then(remaining)
        {
            windows.push(delay);
        }
    }
    // Codex CLI 0.160 只读 `-reset-at`（Unix 秒）；旧的 `-reset-after-seconds` 留作兜底
    for window in ["primary", "secondary"] {
        if text(&format!("x-codex-{window}-used-percent"))
            .is_some_and(|value| exhausted(value, 100))
            && let Some(delay) = text(&format!("x-codex-{window}-reset-at"))
                .and_then(|value| value.parse().ok())
                .and_then(remaining)
                .or_else(|| {
                    text(&format!("x-codex-{window}-reset-after-seconds"))
                        .and_then(|value| value.parse().ok())
                        .and_then(positive)
                })
        {
            windows.push(delay);
        }
    }
    // When both windows are exhausted the later reset is the first usable instant.
    windows
        .into_iter()
        .max()
        .or(explicit)
        .or(usage_reset)
        .or_else(|| {
            text("anthropic-ratelimit-unified-reset")
                .and_then(|value| value.parse().ok())
                .and_then(remaining)
        })
}

/// Google API 的 429 把等待时间放在错误正文的 `google.rpc.RetryInfo.retryDelay`（"34s"），响应头里没有。
/// 只在头里没给时才由 [`crate::UpstreamError::retry_after_secs`] 采用。
pub(crate) fn from_body(body: &[u8]) -> Option<i64> {
    // 与 `account_fault` 同理：更大的正文多半回显了请求，不读
    if body.is_empty() || body.len() > 16 * 1024 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value
        .pointer("/error/details")?
        .as_array()?
        .iter()
        .find_map(|detail| {
            detail
                .get("@type")?
                .as_str()?
                .ends_with("google.rpc.RetryInfo")
                .then(|| duration_seconds(detail.get("retryDelay")?.as_str()?))
                .flatten()
        })
}

/// `retry-after-ms` 的值（毫秒，允许小数）→ 向上取整的秒数；0、负数、读不出来返回 None。
fn millis_to_seconds(value: &str) -> Option<i64> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut millis: i64 = whole.parse().ok()?;
    if fraction.bytes().any(|b| b != b'0') {
        millis = millis.checked_add(1)?;
    }
    if millis <= 0 {
        return None;
    }
    millis.checked_add(999).map(|m| m / 1000)
}

/// Go 风格 / protobuf JSON 的时长（"1s"、"6m0s"、"20ms"、"1.5s"、"1h2m3s"）→ 向上取整的秒数；
/// 读不出来或为 0 返回 None。整数运算，不经浮点。
fn duration_seconds(text: &str) -> Option<i64> {
    let mut rest = text.trim();
    if rest.is_empty() {
        return None;
    }
    let mut millis: u128 = 0;
    while !rest.is_empty() {
        let number_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let (number, tail) = rest.split_at(number_end);
        let unit_end = tail
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(tail.len());
        let (unit, next) = tail.split_at(unit_end);
        let scale: u128 = match unit {
            "ms" => 1,
            "s" => 1_000,
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            _ => return None,
        };
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let whole: u128 = if whole.is_empty() {
            0
        } else {
            whole.parse().ok()?
        };
        millis = millis.checked_add(whole.checked_mul(scale)?)?;
        if !fraction.is_empty() {
            // 九位小数足够精确；多出来的丢掉（向上取整的误差不超过 1 毫秒）
            let fraction = &fraction[..fraction.len().min(9)];
            let numerator: u128 = fraction.parse().ok()?;
            let denominator = 10_u128.pow(u32::try_from(fraction.len()).ok()?);
            millis = millis.checked_add(numerator.checked_mul(scale)?.div_ceil(denominator))?;
        }
        rest = next;
    }
    i64::try_from(millis.div_ceil(1000))
        .ok()
        .filter(|seconds| *seconds > 0)
}

fn exhausted(value: &str, threshold: u64) -> bool {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    !fraction.is_empty()
        && fraction.bytes().all(|b| b.is_ascii_digit())
        && whole.parse::<u64>().is_ok_and(|n| n >= threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn accepts_seconds_and_http_dates_but_rejects_invalid_or_past_values() {
        assert_eq!(at(&headers(&[("retry-after", "30")]), 0), Some(30));
        assert_eq!(
            at(
                &headers(&[("retry-after", "Thu, 01 Jan 1970 00:01:00 GMT")]),
                0
            ),
            Some(60)
        );
        for value in [
            "-1",
            "bad",
            "9223372036854775808",
            "Thu, 01 Jan 1970 00:00:00 GMT",
        ] {
            assert_eq!(at(&headers(&[("retry-after", value)]), 1), None);
        }
    }

    #[test]
    fn millisecond_header_is_the_fallback_for_the_standard_one() {
        assert_eq!(at(&headers(&[("retry-after-ms", "1500")]), 0), Some(2));
        assert_eq!(at(&headers(&[("retry-after-ms", "20")]), 0), Some(1));
        assert_eq!(at(&headers(&[("retry-after-ms", "2000.5")]), 0), Some(3));
        assert_eq!(
            at(
                &headers(&[("retry-after", "5"), ("retry-after-ms", "9000")]),
                0
            ),
            Some(5),
            "标准头优先"
        );
        // 标准头读不出来（已过去的日期）时退到毫秒版
        assert_eq!(
            at(
                &headers(&[
                    ("retry-after", "Thu, 01 Jan 1970 00:00:00 GMT"),
                    ("retry-after-ms", "3000")
                ]),
                1
            ),
            Some(3)
        );
        for bad in ["0", "-1", "abc", "", ".5", "1e3"] {
            assert_eq!(at(&headers(&[("retry-after-ms", bad)]), 0), None, "{bad}");
        }
    }

    #[test]
    fn usage_reset_headers_apply_to_the_exhausted_dimension() {
        let drained_requests = [
            ("x-ratelimit-remaining-requests", "0"),
            ("x-ratelimit-reset-requests", "1s"),
            ("x-ratelimit-remaining-tokens", "1200"),
            ("x-ratelimit-reset-tokens", "6m0s"),
        ];
        assert_eq!(
            at(&headers(&drained_requests), 0),
            Some(1),
            "只有请求数用完，不等 token 维度"
        );
        let mut both = drained_requests;
        both[2].1 = "0";
        assert_eq!(
            at(&headers(&both), 0),
            Some(360),
            "两个维度都用完，取较晚的"
        );
        let mut healthy = drained_requests;
        healthy[0].1 = "5";
        assert_eq!(
            at(&headers(&healthy), 0),
            None,
            "余量没用完只是在报状态，不算限流"
        );
        let mut standard = both.to_vec();
        standard.push(("retry-after", "3"));
        assert_eq!(at(&headers(&standard), 0), Some(3), "标准头优先");
    }

    #[test]
    fn go_style_and_protobuf_durations() {
        for (text, expected) in [
            ("1s", Some(1)),
            ("6m0s", Some(360)),
            ("20ms", Some(1)),
            ("1.5s", Some(2)),
            ("1h2m3s", Some(3723)),
            ("34s", Some(34)),
            ("0.5s", Some(1)),
            ("3.000000001s", Some(4)),
            ("0s", None),
            ("0ms", None),
            ("", None),
            ("30", None),
            ("s", None),
            ("1.2.3s", None),
            ("5x", None),
            ("-5s", None),
        ] {
            assert_eq!(duration_seconds(text), expected, "{text}");
        }
    }

    const RETRY_INFO_BODY: &[u8] = br#"{"error":{"code":429,"message":"You exceeded your current quota","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[]},{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"34s"}]}}"#;

    #[test]
    fn google_retry_info_lives_in_the_body() {
        assert_eq!(from_body(RETRY_INFO_BODY), Some(34));
        for body in [
            &br#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"X"}]}}"#[..],
            br#"{"error":{"details":[{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"soon"}]}}"#,
            b"not json",
            b"",
        ] {
            assert_eq!(from_body(body), None);
        }
    }

    /// 头里给了就用头；头没给才退到正文（Google 系）。
    #[test]
    fn upstream_error_reads_the_body_only_without_a_header() {
        use crate::UpstreamError;
        let error = |retry_after_secs| UpstreamError::Status {
            status: 429,
            body: bytes::Bytes::from_static(RETRY_INFO_BODY),
            retry_after_secs,
        };
        assert_eq!(error(None).retry_after_secs(), Some(34));
        assert_eq!(error(Some(7)).retry_after_secs(), Some(7));
    }

    #[test]
    fn subscription_resets_only_apply_to_exhausted_windows() {
        let mut h = headers(&[
            ("retry-after", "5"),
            ("x-codex-primary-used-percent", "37"),
            ("x-codex-primary-reset-after-seconds", "604800"),
        ]);
        assert_eq!(at(&h, 1000), Some(5));
        h.insert("x-codex-primary-used-percent", "100.0".parse().unwrap());
        assert_eq!(at(&h, 1000), Some(604_800));
        // 绝对时间优先；已过去的绝对时间不算数，退回相对秒数
        h.insert("x-codex-primary-reset-at", "19000".parse().unwrap());
        assert_eq!(at(&h, 1000), Some(18_000));
        h.insert("x-codex-primary-reset-at", "900".parse().unwrap());
        assert_eq!(at(&h, 1000), Some(604_800));
        let h = headers(&[
            ("x-codex-secondary-used-percent", "100"),
            ("x-codex-secondary-reset-at", "4600"),
        ]);
        assert_eq!(at(&h, 1000), Some(3600), "新版只发 reset-at 也能算出冷却");
        let h = headers(&[
            ("anthropic-ratelimit-unified-5h-utilization", "1.0"),
            ("anthropic-ratelimit-unified-5h-reset", "2000"),
            ("anthropic-ratelimit-unified-7d-utilization", "1.1"),
            ("anthropic-ratelimit-unified-7d-reset", "7000"),
        ]);
        assert_eq!(at(&h, 1000), Some(6000));
        assert!(!exhausted("1.bad", 1));
    }
}
