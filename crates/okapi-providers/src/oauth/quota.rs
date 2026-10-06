//! Compatibility API; provider hooks own quota formats and endpoints.
pub use crate::account::quota::{Snapshot, Window};
use crate::{Outbound, UpstreamError, http::HttpPool};
use serde_json::Value;

pub fn parse(provider: &str, data: &Value, now: i64) -> Option<Snapshot> {
    crate::registry::lookup(provider)?
        .account?
        .parse_quota(data, now)
}

pub async fn probe(
    http: &HttpPool,
    provider: &str,
    base: &str,
    token: &str,
    account: Option<&str>,
    outbound: &Outbound,
) -> Result<Snapshot, UpstreamError> {
    let hook = crate::registry::lookup(provider)
        .and_then(|descriptor| descriptor.account)
        .filter(|hook| hook.capabilities().quota)
        .ok_or_else(|| UpstreamError::Build("quota_unsupported".into()))?;
    hook.quota(crate::account::QuotaContext {
        http,
        api_base: base,
        access_token: token,
        account_id: account,
        outbound,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn quota_distinguishes_zero_unknown_expired_and_single_long_window() {
        let value = parse("codex", &json!({"rate_limit":{"primary_window":{"used_percent":90.1,"limit_window_seconds":2_628_000,"reset_at":2000},"secondary_window":null}}),1000).unwrap();
        assert_eq!(value.headroom(1000), Some(9));
        assert_eq!(value.windows[0].window_secs, Some(2_628_000));
        assert_eq!(value.headroom(1121), None);
        let value = parse(
            "anthropic_max",
            &json!({"five_hour":{"utilization":0,"resets_at":"1970-01-01T00:20:00Z"}}),
            1000,
        )
        .unwrap();
        assert_eq!(value.headroom(1000), Some(100));
        assert_eq!(value.headroom(1200), None);
        assert!(
            parse(
                "codex",
                &json!({"rate_limit":{"primary_window":null}}),
                1000
            )
            .is_none()
        );
        assert!(
            parse(
                "anthropic_max",
                &json!({"five_hour":{"utilization":"unknown"}}),
                1000
            )
            .is_none()
        );
    }

    #[test]
    fn claude_percentage_cap_uses_five_hours_while_weekly_exhaustion_still_blocks() {
        let mut data = json!({
            "five_hour": {"utilization":20,"resets_at":"1970-01-01T00:30:00Z"},
            "seven_day": {"utilization":95,"resets_at":"1970-01-01T00:40:00Z"}
        });
        let value = parse("anthropic_max", &data, 1000).unwrap();
        assert_eq!(value.threshold_window.as_deref(), Some("five_hour"));
        assert_eq!(value.headroom(1000), Some(80));
        data["seven_day"]["utilization"] = json!(100);
        assert_eq!(
            parse("anthropic_max", &data, 1000).unwrap().headroom(1000),
            Some(0)
        );
        data["five_hour"] = Value::Null;
        data["seven_day"]["utilization"] = json!(95);
        assert_eq!(
            parse("anthropic_max", &data, 1000).unwrap().headroom(1000),
            None
        );
    }

    #[test]
    fn codex_total_cap_uses_longest_actual_window_and_preserves_short_window_exhaustion() {
        let mut data = json!({"rate_limit": {
            "primary_window": {"used_percent":95,"limit_window_seconds":18000,"reset_at":1800},
            "secondary_window": {"used_percent":40,"limit_window_seconds":604_800,"reset_at":2400}
        }});
        let value = parse("codex", &data, 1000).unwrap();
        assert_eq!(value.threshold_window.as_deref(), Some("secondary_window"));
        assert_eq!(value.headroom(1000), Some(60));
        data["rate_limit"]["primary_window"]["used_percent"] = json!(100);
        assert_eq!(parse("codex", &data, 1000).unwrap().headroom(1000), Some(0));
        data["rate_limit"]["primary_window"]["reset_at"] = json!(900);
        assert_eq!(
            parse("codex", &data, 1000).unwrap().headroom(1000),
            Some(60)
        );
    }

    #[test]
    fn old_cached_snapshots_are_scoped_by_the_hook_and_expired_data_is_unknown() {
        let data = json!({"rate_limit": {
            "primary_window": {"used_percent":95,"limit_window_seconds":18000,"reset_at":1800},
            "secondary_window": {"used_percent":40,"limit_window_seconds":604_800,"reset_at":2400}
        }});
        let snapshot = parse("codex", &data, 1000).unwrap();
        let mut old = serde_json::to_value(snapshot).unwrap();
        old.as_object_mut().unwrap().remove("threshold_window");
        let mut cached: Snapshot = serde_json::from_value(old).unwrap();
        crate::registry::lookup("codex")
            .unwrap()
            .account
            .unwrap()
            .scope_quota(&mut cached);
        assert_eq!(cached.headroom(1000), Some(60));
        assert_eq!(cached.headroom(1121), None);
    }
}
