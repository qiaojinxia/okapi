//! Configured estimates follow the selected candidate, not mutable settlement-time config.
use okapi_domain::Money;
use serde_json::{Value, json};

pub(super) fn snapshot(
    mut snapshot: Option<Value>,
    channel: i64,
    cost_milli: i64,
    list_price: Money,
) -> Option<Value> {
    if let Some(value) = snapshot.as_mut() {
        pin(value, channel, cost_milli, list_price);
    }
    snapshot
}

pub(super) fn pin(value: &mut Value, channel: i64, cost_milli: i64, list_price: Money) {
    pin_source(value, channel, cost_milli, list_price, "selected_channel");
}

pub(super) fn pin_legacy(value: &mut Value, channel: i64, cost_milli: i64, list_price: Money) {
    pin_source(
        value,
        channel,
        cost_milli,
        list_price,
        "settlement_lookup_legacy",
    );
}

fn pin_source(value: &mut Value, channel: i64, cost_milli: i64, price: Money, source: &str) {
    if let Some(map) = value.as_object_mut() {
        map.insert(
            "upstream_cost_basis".into(),
            json!({"version":1,"source":source,"channel_id":channel,
                "relative_cost_milli":cost_milli,"list_price_micro":price.as_micros()}),
        );
    }
}

/// Existing but invalid provenance never authorizes a lookup of newer configuration.
pub(super) fn estimate(snapshot: &Value, channel: i64, price: Money) -> Option<Money> {
    let basis = snapshot.get("upstream_cost_basis")?;
    if basis.get("version").and_then(Value::as_u64) != Some(1)
        || !matches!(
            basis.get("source").and_then(Value::as_str),
            Some("selected_channel" | "settlement_lookup_legacy")
        )
        || basis.get("channel_id").and_then(Value::as_i64) != Some(channel)
        || basis.get("list_price_micro").and_then(Value::as_i64) != Some(price.as_micros())
        || basis.get("batch_cost_ratio_milli").is_some()
    {
        return None;
    }
    estimated_cost(price, basis.get("relative_cost_milli")?.as_i64()?)
}

pub(super) fn estimated_cost(list_price: Money, cost_milli: i64) -> Option<Money> {
    if list_price.is_negative() || cost_milli < 0 {
        return None;
    }
    let value = i128::from(list_price.as_micros()).checked_mul(i128::from(cost_milli))? / 1000;
    i64::try_from(value).ok().map(Money::from_micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_estimate_never_saturates_or_overflows_into_a_known_amount() {
        for (list, ratio, expected) in [
            (21600, 1250, Some(27000)),
            (21600, 0, Some(0)),
            (3, 333, Some(0)),
            (i64::MAX, 1000, Some(i64::MAX)),
            (i64::MAX, 1001, None),
            (-1, 1000, None),
            (1, -1, None),
        ] {
            assert_eq!(
                estimated_cost(Money::from_micros(list), ratio).map(Money::as_micros),
                expected
            );
        }
    }

    #[test]
    fn invalid_or_mismatched_basis_cannot_become_known_cost() {
        let mut snapshot = json!({});
        let price = Money::from_micros(21600);
        pin(&mut snapshot, 9, 1250, price);
        assert_eq!(
            estimate(&snapshot, 9, price).map(Money::as_micros),
            Some(27000)
        );
        assert_eq!(estimate(&snapshot, 10, price), None);
        assert_eq!(estimate(&snapshot, 9, Money::from_micros(1)), None);
        for (key, value) in [
            ("version", json!(2)),
            ("source", json!("unverified")),
            ("relative_cost_milli", json!(-1)),
            ("relative_cost_milli", json!("1250")),
            ("batch_cost_ratio_milli", json!(500)),
        ] {
            let mut bad = snapshot.clone();
            bad["upstream_cost_basis"][key] = value;
            assert_eq!(estimate(&bad, 9, price), None, "{bad}");
        }
        assert_eq!(
            estimate(&json!({"upstream_cost_basis":null}), 9, price),
            None
        );
        assert_eq!(estimate(&json!({}), 9, price), None);
    }
}
