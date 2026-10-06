//! Independent request fees. The contract identifies native usage, not function names.
use crate::{CompileError, PriceBook, PricingError, Quote};
use okapi_domain::{AnthropicToolUsage, ModelCode, Money, ServerToolUsage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "billing", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolPrice {
    Additional { price_per_request_micro: Money },
    Included {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicToolPrices {
    pub web_search: ToolPrice,
    pub web_fetch: ToolPrice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "usage_contract", rename_all = "snake_case")]
pub enum ServerToolPrices {
    AnthropicServerToolUseV1(AnthropicToolPrices),
}

impl ServerToolPrices {
    /// An empty object explicitly clears a draft; omission is handled by the caller.
    pub fn parse(value: &serde_json::Value) -> Result<Option<Self>, &'static str> {
        let object = value.as_object().ok_or("server_tool_prices")?;
        if object.is_empty() {
            return Ok(None);
        }
        // Serde's empty struct variant can ignore fields even with deny_unknown_fields.
        // Do not silently treat an explicitly supplied price as included/free.
        for name in ["web_search", "web_fetch"] {
            if object
                .get(name)
                .and_then(serde_json::Value::as_object)
                .is_some_and(|tool| {
                    tool.get("billing").and_then(serde_json::Value::as_str) == Some("included")
                        && tool.len() != 1
                })
            {
                return Err("server_tool_prices");
            }
        }
        let prices: Self =
            serde_json::from_value(value.clone()).map_err(|_| "server_tool_prices")?;
        prices.validate().map_err(|_| "server_tool_prices")?;
        Ok(Some(prices))
    }

    pub(crate) fn validate(self) -> Result<(), &'static str> {
        let Self::AnthropicServerToolUseV1(prices) = self;
        for price in [prices.web_search, prices.web_fetch] {
            if let ToolPrice::Additional {
                price_per_request_micro,
            } = price
                && price_per_request_micro.is_negative()
            {
                return Err("negative server tool price");
            }
        }
        Ok(())
    }
}

/// Request authority is separate from whether a native counter was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicToolScope {
    pub web_search: bool,
    pub web_fetch: bool,
}

/// Null components mean unpriced/unobserved. They must not be presented as free.
#[derive(Debug, Clone, Serialize)]
pub struct ServerToolFee {
    pub usage_contract: &'static str,
    pub tool: &'static str,
    pub unit: &'static str,
    pub quantity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested: Option<bool>,
    pub pricing: Option<ToolPrice>,
    pub list_price_micro: Option<Money>,
    pub original_amount_micro: Option<Money>,
    pub amount_micro: Option<Money>,
    pub discount_micro: Option<Money>,
}

impl PriceBook {
    /// Read the explicitly published profile without importing a current default price.
    #[must_use]
    pub fn server_tool_prices(&self, model: &ModelCode) -> Option<ServerToolPrices> {
        self.server_tool_prices.get(model).copied()
    }

    /// Bind only explicitly configured fees to this immutable book's epoch.
    pub fn with_server_tool_prices(
        mut self,
        entries: impl IntoIterator<Item = (ModelCode, ServerToolPrices)>,
    ) -> Result<Self, CompileError> {
        for (model, prices) in entries {
            prices
                .validate()
                .map_err(|reason| CompileError::InvalidServerToolPrices {
                    model: model.to_string(),
                    reason,
                })?;
            if self
                .server_tool_prices
                .insert(model.clone(), prices)
                .is_some()
            {
                return Err(CompileError::InvalidServerToolPrices {
                    model: model.to_string(),
                    reason: "duplicate server tool profile",
                });
            }
        }
        Ok(self)
    }
}

fn fee(
    quote: &Quote,
    tool: &'static str,
    quantity: Option<u32>,
    pricing: Option<ToolPrice>,
    requested: Option<bool>,
) -> Result<ServerToolFee, PricingError> {
    let mut row = ServerToolFee {
        usage_contract: "anthropic_server_tool_use_v1",
        tool,
        unit: "request",
        quantity,
        requested,
        pricing,
        list_price_micro: None,
        original_amount_micro: None,
        amount_micro: None,
        discount_micro: None,
    };
    if requested == Some(false) {
        if quantity.is_some_and(|n| n > 0) {
            return Err(PricingError::InvalidServerToolAdmission(
                "undeclared_tool_usage",
            ));
        }
        row.list_price_micro = Some(Money::ZERO);
        row.original_amount_micro = Some(Money::ZERO);
        row.amount_micro = Some(Money::ZERO);
        row.discount_micro = Some(Money::ZERO);
        return Ok(row);
    }
    let Some(pricing) = pricing else {
        return Ok(row);
    };
    let unit_price = match pricing {
        ToolPrice::Included {} => Money::ZERO,
        ToolPrice::Additional {
            price_per_request_micro,
        } => price_per_request_micro,
    };
    let Some(quantity) = quantity else {
        if !unit_price.is_zero() {
            return Err(PricingError::MissingServerToolUsage);
        }
        return Ok(row);
    };
    let list = i128::from(unit_price.as_micros())
        .checked_mul(i128::from(quantity))
        .ok_or(PricingError::Overflow)?;
    let list_price = Money::from_micros(i64::try_from(list).map_err(|_| PricingError::Overflow)?);
    let scaled = list
        .checked_mul(i128::from(crate::ratio::RATIO_SCALE))
        .ok_or(PricingError::Overflow)?;
    let mut scaled = crate::engine::step(scaled, quote.snapshot.group_ratio)?;
    let original = crate::engine::micro_from_money_scaled(scaled)?;
    scaled = crate::engine::step(scaled, quote.snapshot.user_multiplier)?;
    for rule in &quote.snapshot.rules {
        scaled = crate::engine::step(scaled, rule.multiplier)?;
    }
    let amount = crate::engine::micro_from_money_scaled(scaled)?;
    let discount = original.checked_sub(amount).ok_or(PricingError::Overflow)?;
    row.list_price_micro = Some(list_price);
    row.original_amount_micro = Some(original);
    row.amount_micro = Some(amount);
    row.discount_micro = Some(discount);
    Ok(row)
}

pub(crate) fn add_to_quote(
    mut quote: Quote,
    book: &PriceBook,
    model: &ModelCode,
    usage: Option<ServerToolUsage>,
    scope: Option<AnthropicToolScope>,
) -> Result<Quote, PricingError> {
    let usage = match usage {
        Some(ServerToolUsage::Anthropic(u)) => u,
        None if scope.is_some() => AnthropicToolUsage::default(),
        None => return Ok(quote),
    };
    let prices = book
        .server_tool_prices
        .get(model)
        .map(|prices| match prices {
            ServerToolPrices::AnthropicServerToolUseV1(prices) => *prices,
        });
    let rows = [
        fee(
            &quote,
            "web_search",
            usage.web_search_requests,
            prices.map(|p| p.web_search),
            scope.map(|s| s.web_search),
        )?,
        fee(
            &quote,
            "web_fetch",
            usage.web_fetch_requests,
            prices.map(|p| p.web_fetch),
            scope.map(|s| s.web_fetch),
        )?,
    ];
    for row in rows {
        let add = |value: Money, fee: Option<Money>| {
            value
                .checked_add(fee.unwrap_or(Money::ZERO))
                .ok_or(PricingError::Overflow)
        };
        quote.list_price = add(quote.list_price, row.list_price_micro)?;
        quote.original = add(quote.original, row.original_amount_micro)?;
        quote.amount = add(quote.amount, row.amount_micro)?;
        quote.discount = add(quote.discount, row.discount_micro)?;
        quote.snapshot.server_tool_fees.push(row);
    }
    Ok(quote)
}
