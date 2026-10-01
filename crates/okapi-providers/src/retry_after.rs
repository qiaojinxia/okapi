//! Retry timing is transport metadata; only exhausted subscription windows block a key.
use reqwest::header::HeaderMap;

pub(crate) fn seconds(headers: &HeaderMap) -> Option<i64> {
    at(headers, chrono::Utc::now().timestamp())
}

fn at(headers: &HeaderMap, now: i64) -> Option<i64> {
    let text = |name: &str| headers.get(name)?.to_str().ok().map(str::trim);
    let positive = |value: i64| (value > 0).then_some(value);
    let remaining = |reset: i64| reset.checked_sub(now).and_then(positive);
    let explicit = text("retry-after").and_then(|value| {
        value.parse::<i64>().ok().filter(|n| *n >= 0).or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value)
                .ok()
                .and_then(|date| remaining(date.timestamp()))
        })
    });
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
    for window in ["primary", "secondary"] {
        if text(&format!("x-codex-{window}-used-percent"))
            .is_some_and(|value| exhausted(value, 100))
            && let Some(delay) = text(&format!("x-codex-{window}-reset-after-seconds"))
                .and_then(|value| value.parse().ok())
                .and_then(positive)
        {
            windows.push(delay);
        }
    }
    // When both windows are exhausted the later reset is the first usable instant.
    windows.into_iter().max().or(explicit).or_else(|| {
        text("anthropic-ratelimit-unified-reset")
            .and_then(|value| value.parse().ok())
            .and_then(remaining)
    })
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
    fn subscription_resets_only_apply_to_exhausted_windows() {
        let mut h = headers(&[
            ("retry-after", "5"),
            ("x-codex-primary-used-percent", "37"),
            ("x-codex-primary-reset-after-seconds", "604800"),
        ]);
        assert_eq!(at(&h, 1000), Some(5));
        h.insert("x-codex-primary-used-percent", "100.0".parse().unwrap());
        assert_eq!(at(&h, 1000), Some(604_800));
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
