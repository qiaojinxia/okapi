//! Frozen admission quotes explain a hold; their estimates are never measured usage.
use okapi_domain::{Money, TokenUsage};
use okapi_pricing::{PricingError, PricingSnapshot, Quote, ServerToolFee};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct Amounts {
    #[serde(rename = "amount_micro")]
    amount: Money,
    #[serde(rename = "original_amount_micro")]
    original: Money,
    #[serde(rename = "discount_micro")]
    discount: Money,
    #[serde(rename = "list_price_micro")]
    list_price: Money,
}

impl Amounts {
    fn from_quote(quote: &Quote) -> Self {
        Self {
            amount: quote.amount,
            original: quote.original,
            discount: quote.discount,
            list_price: quote.list_price,
        }
    }

    fn without_tools(mut self, fees: &[ServerToolFee]) -> Result<Self, PricingError> {
        // Invert checked_add in reverse order, including signed rule discounts.
        // Unknown fees contributed zero to the quote but remain null in their rows.
        for fee in fees.iter().rev() {
            let sub = |value: Money, fee: Option<Money>| {
                value
                    .checked_sub(fee.unwrap_or(Money::ZERO))
                    .ok_or(PricingError::Overflow)
            };
            self.amount = sub(self.amount, fee.amount_micro)?;
            self.original = sub(self.original, fee.original_amount_micro)?;
            self.discount = sub(self.discount, fee.discount_micro)?;
            self.list_price = sub(self.list_price, fee.list_price_micro)?;
        }
        Ok(self)
    }
}

#[derive(Serialize)]
pub(super) struct Candidate {
    role: &'static str,
    routing_model: String,
    priced_model: String,
    completion_cap: u32,
    estimated_usage: TokenUsage,
    quote: Amounts,
    components: Components,
    pricing_snapshot: PricingSnapshot,
}

#[derive(Serialize)]
struct Components {
    /// Ratio/tiered Token or per-call base quote, before independent tool fees.
    base_quote: Amounts,
    server_tool_fees: Vec<ServerToolFee>,
}

impl Candidate {
    pub(super) fn new(
        role: &'static str,
        routing_model: &str,
        priced_model: &str,
        estimated_usage: TokenUsage,
        quote: &Quote,
    ) -> Result<Self, PricingError> {
        let amounts = Amounts::from_quote(quote);
        Ok(Self {
            role,
            routing_model: routing_model.to_owned(),
            priced_model: priced_model.to_owned(),
            completion_cap: estimated_usage.completion_tokens,
            estimated_usage,
            quote: amounts,
            components: Components {
                base_quote: amounts.without_tools(&quote.snapshot.server_tool_fees)?,
                server_tool_fees: quote.snapshot.server_tool_fees.clone(),
            },
            pricing_snapshot: quote.snapshot.clone(),
        })
    }
}

#[derive(Serialize)]
struct Admission<'a> {
    version: u8,
    source: &'static str,
    policy: &'static str,
    requested_model: &'a str,
    reserved_amount_micro: Money,
    max_candidate_index: usize,
    candidates: Vec<Candidate>,
}

pub(super) fn freeze(
    requested_model: &str,
    reserved_amount: Money,
    candidates: Vec<Candidate>,
) -> Result<Value, PricingError> {
    // Strict > gives the first winner on ties, independent of route availability.
    let mut winner = None;
    for (index, candidate) in candidates.iter().enumerate() {
        if winner.is_none_or(|(_, amount)| candidate.quote.amount > amount) {
            winner = Some((index, candidate.quote.amount));
        }
    }
    let (max_candidate_index, amount) =
        winner.ok_or(PricingError::Internal("reservation_empty"))?;
    if amount != reserved_amount {
        return Err(PricingError::Internal("reservation_amount_mismatch"));
    }
    serde_json::to_value(Admission {
        version: 1,
        source: "gateway_admission",
        policy: "max_candidate_amount",
        requested_model,
        reserved_amount_micro: reserved_amount,
        max_candidate_index,
        candidates,
    })
    .map_err(|_| PricingError::Internal("reservation_serialize"))
}

pub(super) fn settled_snapshot(
    pricing: &PricingSnapshot,
    reservation: &Value,
) -> Result<Value, PricingError> {
    let mut snapshot = serde_json::to_value(pricing)
        .map_err(|_| PricingError::Internal("pricing_snapshot_serialize"))?;
    let object = snapshot
        .as_object_mut()
        .ok_or(PricingError::Internal("pricing_snapshot_shape"))?;
    object.insert("reservation".into(), reservation.clone());
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use okapi_domain::{AnthropicToolUsage, ServerToolUsage, UserId};
    use okapi_pricing::{
        AnthropicToolPrices, AnthropicToolScope, CalcContext, GroupEntry, ModelEntry,
        PriceBookSource, PricingMode, RatioFp, ServerToolPrices, ToolPrice, book,
        calculate_with_server_tool_scope,
    };

    fn quote(user: i64, tool_price: Option<i64>) -> (TokenUsage, Quote) {
        let fp = |n| RatioFp::from_scaled(n).unwrap();
        let mut book = book::compile(PriceBookSource {
            epoch: 77,
            models: vec![ModelEntry {
                model: "m".into(),
                pricing: PricingMode::PerCall {
                    price: Money::from_micros(180),
                },
                tier_ratios: vec![],
            }],
            groups: vec![GroupEntry {
                group: "g".into(),
                ratio: fp(2_000_000),
            }],
            overrides: vec![],
            rules: vec![],
        })
        .unwrap();
        if let Some(price) = tool_price {
            book = book
                .with_server_tool_prices([(
                    "m".into(),
                    ServerToolPrices::AnthropicServerToolUseV1(AnthropicToolPrices {
                        web_search: ToolPrice::Additional {
                            price_per_request_micro: Money::from_micros(price),
                        },
                        web_fetch: ToolPrice::Included {},
                    }),
                )])
                .unwrap();
        }
        let usage = TokenUsage {
            prompt_tokens: 50,
            completion_tokens: 20,
            server_tool_usage: Some(ServerToolUsage::Anthropic(AnthropicToolUsage {
                web_search_requests: Some(2),
                web_fetch_requests: None,
                code_execution_requests: None,
            })),
            ..TokenUsage::default()
        };
        let context = CalcContext {
            user: UserId::new(1),
            model: "m".into(),
            group: "g".into(),
            user_multiplier: fp(user),
            monthly_tokens: 0,
            monthly_spend_micro: 0,
            local_minute_of_day: 0,
            now_unix: 0,
            utc_offset_seconds: 0,
            surge_active: false,
            service_tier: None,
        };
        let quote = calculate_with_server_tool_scope(
            &book,
            &context,
            usage,
            AnthropicToolScope {
                web_search: true,
                web_fetch: false,
            },
        )
        .unwrap();
        (usage, quote)
    }

    #[test]
    fn base_components_keep_signed_discounts_and_unknown_fees_distinct() {
        for user in [500_000, 2_000_000] {
            for price in [None, Some(0), Some(10000)] {
                let (usage, quote) = quote(user, price);
                let candidate =
                    Candidate::new("primary", "alias-base", "m", usage, &quote).unwrap();
                let base = candidate.components.base_quote;
                assert_eq!(base.list_price.as_micros(), 180);
                assert_eq!(base.original.as_micros(), 360);
                assert_eq!(base.amount.as_micros(), 360 * user / 1_000_000);
                assert_eq!(base.discount.as_micros(), 360 - 360 * user / 1_000_000);
                let value = freeze("alias", quote.amount, vec![candidate]).unwrap();
                let c = &value["candidates"][0];
                assert_eq!(c["routing_model"], "alias-base");
                assert_eq!(c["priced_model"], "m");
                assert_eq!(c["pricing_snapshot"]["mode"], "per_call");
                assert_eq!(c["pricing_snapshot"]["epoch"], 77);
                assert_eq!(
                    c["components"]["server_tool_fees"][0]["pricing"].is_null(),
                    price.is_none()
                );
                assert_eq!(
                    c["components"]["server_tool_fees"][0]["amount_micro"].is_null(),
                    price.is_none()
                );
                assert!(c["components"]["server_tool_fees"][1]["quantity"].is_null());
                assert_eq!(c["components"]["server_tool_fees"][1]["amount_micro"], 0);
            }
        }
    }

    #[test]
    fn winner_is_first_on_ties_and_malformed_hold_metadata_is_rejected() {
        let (usage, quote) = quote(1_000_000, Some(10000));
        let candidates = || {
            vec![
                Candidate::new("primary", "a", "m", usage, &quote).unwrap(),
                Candidate::new("fallback", "b", "m", usage, &quote).unwrap(),
            ]
        };
        let value = freeze("alias", quote.amount, candidates()).unwrap();
        assert_eq!(value["max_candidate_index"], 0);
        assert!(freeze("alias", Money::ZERO, candidates()).is_err());
        assert!(freeze("alias", Money::ZERO, vec![]).is_err());
    }

    #[test]
    fn components_invert_original_add_order_and_reject_overflow() {
        let (_, quote) = quote(1_000_000, Some(0));
        let mut first = quote.snapshot.server_tool_fees[0].clone();
        first.discount_micro = Some(Money::from_micros(-i64::MAX));
        let mut second = first.clone();
        second.discount_micro = Some(Money::from_micros(i64::MAX));
        let mut total = Amounts::from_quote(&quote);
        total.discount = Money::from_micros(i64::MAX);
        assert_eq!(total.without_tools(&[first, second]).unwrap(), total);
        let mut malformed = quote.snapshot.server_tool_fees[0].clone();
        malformed.discount_micro = Some(Money::from_micros(i64::MIN));
        total.discount = Money::ZERO;
        assert_eq!(
            total.without_tools(&[malformed]),
            Err(PricingError::Overflow)
        );
    }
}
