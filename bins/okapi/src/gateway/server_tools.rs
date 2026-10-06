//! Request authorization and monetary bounds for native Anthropic tools.
use bytes::Bytes;
use okapi_domain::{AnthropicToolUsage, Money, ServerToolUsage};
use okapi_pricing::{
    AnthropicToolScope, CalcContext, PriceBook, PricingError, Quote, ServerToolPrices, ToolPrice,
    calculate, calculate_with_server_tool_scope,
};
use okapi_providers::UpstreamError;
use serde_json::{Value, json};

/// A gateway policy, explicitly forwarded; not a provider default.
const DEFAULT_MAX_USES: u32 = 5;

#[derive(Clone, Default)]
pub(super) struct ToolAdmission {
    tools: Vec<Value>,
    search: Option<u32>,
    fetch: Option<u32>,
    execution: bool,
}

fn kind(tool: &Value) -> Result<Option<&'static str>, &'static str> {
    let Some(t) = tool.get("type").and_then(Value::as_str) else {
        return Ok(None);
    };
    let name = match t {
        "web_search_20250305" | "web_search_20260209" | "web_search_20260318" => "web_search",
        "web_fetch_20250910" | "web_fetch_20260209" | "web_fetch_20260309"
        | "web_fetch_20260318" => "web_fetch",
        "code_execution_20250522"
        | "code_execution_20250825"
        | "code_execution_20260120"
        | "code_execution_20260521" => "code_execution",
        t if ["web_search_", "web_fetch_", "code_execution_"]
            .iter()
            .any(|prefix| {
                t.strip_prefix(prefix)
                    .is_some_and(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()))
            }) =>
        {
            return Err("tools.type");
        }
        _ => return Ok(None),
    };
    if tool.get("name").and_then(Value::as_str) != Some(name) {
        return Err("tools.name");
    }
    Ok(Some(name))
}

fn build_error(param: &str) -> UpstreamError {
    UpstreamError::Build(param.to_owned())
}

impl ToolAdmission {
    pub(super) fn parse(body: &[u8]) -> Result<Self, &'static str> {
        let value: Value = serde_json::from_slice(body).map_err(|_| "tools")?;
        let mut plan = Self::default();
        for tool in value
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = kind(tool)? else {
                continue;
            };
            if name == "code_execution" {
                if plan.execution {
                    return Err("tools.duplicate");
                }
                plan.execution = true;
                plan.tools.push(tool.clone());
                continue;
            }
            let cap = match tool.get("max_uses") {
                None => DEFAULT_MAX_USES,
                Some(n) => n
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0 && i32::try_from(*n).is_ok())
                    .ok_or("tools.max_uses")?,
            };
            let axis = if name == "web_search" {
                &mut plan.search
            } else {
                &mut plan.fetch
            };
            if axis.replace(cap).is_some() {
                return Err("tools.duplicate");
            }
            let mut tool = tool.clone();
            tool["max_uses"] = json!(cap);
            plan.tools.push(tool);
        }
        // A Chat tool_choice=none disables native tools as well as ordinary functions.
        if value.get("tool_choice").and_then(Value::as_str) == Some("none") {
            return Ok(Self::default());
        }
        Ok(plan)
    }

    pub(super) fn has_tools(&self) -> bool {
        !self.tools.is_empty()
    }

    pub(super) fn estimated_usage(&self) -> Option<ServerToolUsage> {
        (self.search.is_some() || self.fetch.is_some()).then_some(ServerToolUsage::Anthropic(
            AnthropicToolUsage {
                web_search_requests: Some(self.search.unwrap_or(0)),
                web_fetch_requests: Some(self.fetch.unwrap_or(0)),
                code_execution_requests: None,
            },
        ))
    }

    /// Directional converters handle ordinary tools. Restore the authorized native
    /// definitions afterwards, without changing them into client function tools.
    pub(super) fn restore(&self, body: Bytes, dialect: &str) -> Result<Bytes, UpstreamError> {
        if self.has_tools() && dialect != "anthropic" {
            return Err(build_error("tools.provider"));
        }
        let mut value: Value = serde_json::from_slice(&body).map_err(|_| build_error("tools"))?;
        let mut tools = value
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut ordinary = Vec::new();
        for tool in tools.drain(..) {
            if kind(&tool).map_err(build_error)?.is_none() {
                ordinary.push(tool);
            }
        }
        if ordinary.len()
            == value
                .get("tools")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
            && !self.has_tools()
        {
            return Ok(body);
        }
        ordinary.extend(self.tools.iter().cloned());
        if ordinary.is_empty() {
            if let Some(map) = value.as_object_mut() {
                map.remove("tools");
            }
        } else {
            value["tools"] = Value::Array(ordinary);
        }
        serde_json::to_vec(&value)
            .map(Bytes::from)
            .map_err(|_| build_error("tools"))
    }

    /// Check the final body after channel injection/stripping. Missing max_uses
    /// must not be normalized back to our default here: it is no longer bounded.
    pub(super) fn verify_outbound(&self, body: &[u8]) -> Result<(), UpstreamError> {
        let value: Value = serde_json::from_slice(body).map_err(|_| build_error("tools"))?;
        let mut native = Vec::new();
        for tool in value
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if kind(tool).map_err(build_error)?.is_some() {
                native.push(tool);
            }
        }
        if native != self.tools.iter().collect::<Vec<_>>() {
            return Err(build_error("tools.admission_changed"));
        }
        Ok(())
    }

    pub(super) fn validate_usage(
        &self,
        prices: Option<ServerToolPrices>,
        observed: Option<ServerToolUsage>,
    ) -> Result<(), PricingError> {
        let counts = match observed {
            Some(ServerToolUsage::Anthropic(u)) => u,
            None => AnthropicToolUsage::default(),
        };
        let prices = prices.map(|ServerToolPrices::AnthropicServerToolUseV1(p)| p);
        for (cap, count, price) in [
            (
                self.search,
                counts.web_search_requests,
                prices.map(|p| p.web_search),
            ),
            (
                self.fetch,
                counts.web_fetch_requests,
                prices.map(|p| p.web_fetch),
            ),
        ] {
            if let Some(cap) = cap {
                if count.is_some_and(|n| n > cap) {
                    return Err(PricingError::InvalidServerToolAdmission(
                        "max_uses_exceeded",
                    ));
                }
                if count.is_none()
                    && matches!(price, Some(ToolPrice::Additional { price_per_request_micro }) if !price_per_request_micro.is_zero())
                {
                    return Err(PricingError::MissingServerToolUsage);
                }
            } else if prices.is_some() && count.is_some_and(|n| n > 0) {
                return Err(PricingError::InvalidServerToolAdmission(
                    "undeclared_tool_usage",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn quote(
        &self,
        book: &PriceBook,
        calc: &CalcContext,
        usage: okapi_domain::TokenUsage,
    ) -> Result<Quote, PricingError> {
        if self.has_tools() || book.server_tool_prices(&calc.model).is_some() {
            calculate_with_server_tool_scope(
                book,
                calc,
                usage,
                AnthropicToolScope {
                    web_search: self.search.is_some(),
                    web_fetch: self.fetch.is_some(),
                },
            )
        } else {
            calculate(book, calc, usage)
        }
    }

    pub(super) fn snapshot(&self, reserved_amount: Money) -> Value {
        json!({"tools":self.tools,"reserved_amount_micro":reserved_amount})
    }

    /// Counts do not provide billed duration, organization allowance or inclusion.
    /// Keep known search/fetch fees without claiming they cover the whole cost.
    pub(super) fn cost_coverage(&self, observed: Option<ServerToolUsage>) -> Option<Value> {
        let count = observed.and_then(|ServerToolUsage::Anthropic(u)| u.code_execution_requests);
        (self.execution || count.is_some_and(|n| n > 0)).then(|| {
            json!({
                "version":1,"source":"anthropic_native_tool_usage","complete":false,
                "reason":"container_duration_price_contract_unavailable","provider":"anthropic",
                "tool":"code_execution","billing_unit":"container_duration",
                "requested":self.execution,"observed_requests":count
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use okapi_pricing::AnthropicToolPrices;

    fn plan(tool: &Value) -> ToolAdmission {
        ToolAdmission::parse(&serde_json::to_vec(&json!({"tools":[tool]})).unwrap()).unwrap()
    }
    fn paid() -> ServerToolPrices {
        ServerToolPrices::AnthropicServerToolUseV1(AnthropicToolPrices {
            web_search: ToolPrice::Additional {
                price_per_request_micro: Money::from_micros(10000),
            },
            web_fetch: ToolPrice::Included {},
        })
    }

    #[test]
    fn execution_native_definitions_are_protected_without_fabricated_caps_or_counts() {
        for version in ["20250522", "20250825", "20260120", "20260521"] {
            let tool = json!({"type":format!("code_execution_{version}"),"name":"code_execution"});
            let p = plan(&tool);
            assert!(p.has_tools());
            assert!(p.estimated_usage().is_none());
            let restored = p.restore(Bytes::from_static(b"{}"), "anthropic").unwrap();
            p.verify_outbound(&restored).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&restored).unwrap()["tools"],
                json!([tool])
            );
            assert!(p.restore(Bytes::from_static(b"{}"), "openai").is_err());
            assert!(p.verify_outbound(b"{}").is_err());
            for count in [None, Some(0), Some(2)] {
                let coverage = p
                    .cost_coverage(Some(ServerToolUsage::Anthropic(AnthropicToolUsage {
                        code_execution_requests: count,
                        ..AnthropicToolUsage::default()
                    })))
                    .unwrap();
                assert_eq!(coverage["observed_requests"], json!(count));
                assert_eq!(coverage["complete"], false);
                assert_eq!(coverage["requested"], true);
            }
            let body = json!({"tools":[tool,tool]});
            assert!(ToolAdmission::parse(&serde_json::to_vec(&body).unwrap()).is_err());
        }
        for tool in [
            json!({"name":"code_execution","input_schema":{}}),
            json!({"type":"function","function":{"name":"code_execution"}}),
        ] {
            let p = plan(&tool);
            assert!(!p.has_tools());
            assert!(p.cost_coverage(None).is_none());
        }
        for count in [None, Some(0), Some(1)] {
            let coverage = ToolAdmission::default().cost_coverage(Some(
                ServerToolUsage::Anthropic(AnthropicToolUsage {
                    code_execution_requests: count,
                    ..AnthropicToolUsage::default()
                }),
            ));
            assert_eq!(coverage.is_some(), count == Some(1));
        }
    }

    #[test]
    fn native_declarations_are_bounded_without_inventing_function_authorization() {
        let p = plan(
            &json!({"type":"web_search_20250305","name":"web_search","allowed_domains":["example.org"]}),
        );
        assert_eq!(p.search, Some(5));
        let b = p
            .restore(
                Bytes::from_static(
                    br#"{"messages":[],"tools":[{"name":"local","input_schema":{}}]}"#,
                ),
                "anthropic",
            )
            .unwrap();
        p.verify_outbound(&b).unwrap();
        let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["tools"][1]["max_uses"], 5);
        assert_eq!(v["tools"][1]["allowed_domains"], json!(["example.org"]));
        assert!(p.restore(Bytes::from_static(b"{}"), "openai").is_err());
        assert!(!plan(&json!({"type":"function","function":{"name":"web_search"}})).has_tools());
        assert!(!plan(&json!({"name":"web_search","input_schema":{"type":"object"}})).has_tools());
    }

    #[test]
    fn other_provider_builtin_tools_are_not_misclassified_as_anthropic() {
        for tool in [
            json!({"type":"web_search"}),
            json!({"type":"web_search_preview"}),
            json!({"type":"web_search_preview_2025_03_11"}),
            json!({"type":"file_search"}),
            json!({"googleSearch":{}}),
        ] {
            let p = plan(&tool);
            assert!(!p.has_tools());
            let body = Bytes::from(serde_json::to_vec(&json!({"tools":[tool]})).unwrap());
            assert_eq!(p.restore(body.clone(), "openai").unwrap(), body);
        }
    }

    #[test]
    fn malformed_or_repeated_native_definitions_are_rejected() {
        for n in [
            json!(0),
            json!(-1),
            json!("2"),
            json!(null),
            json!(2_147_483_648_u64),
        ] {
            let v =
                json!({"tools":[{"type":"web_search_20250305","name":"web_search","max_uses":n}]});
            assert_eq!(
                ToolAdmission::parse(&serde_json::to_vec(&v).unwrap()).err(),
                Some("tools.max_uses")
            );
        }
        for tool in [
            json!({"type":"web_search_20990101","name":"web_search"}),
            json!({"type":"web_search_20250305","name":"local"}),
        ] {
            assert!(
                ToolAdmission::parse(&serde_json::to_vec(&json!({"tools":[tool]})).unwrap())
                    .is_err()
            );
        }
        let t = json!({"type":"web_search_20250305","name":"web_search"});
        assert_eq!(
            ToolAdmission::parse(&serde_json::to_vec(&json!({"tools":[t,t]})).unwrap()).err(),
            Some("tools.duplicate")
        );
    }

    #[test]
    fn channel_mutation_cannot_remove_or_raise_the_forwarded_cap() {
        let p = plan(&json!({"type":"web_search_20250305","name":"web_search","max_uses":2}));
        for tools in [
            json!([]),
            json!([{"type":"web_search_20250305","name":"web_search"}]),
            json!([{"type":"web_search_20250305","name":"web_search","max_uses":3}]),
        ] {
            assert!(
                p.verify_outbound(&serde_json::to_vec(&json!({"tools":tools})).unwrap())
                    .is_err()
            );
        }
    }

    #[test]
    fn missing_whole_usage_or_over_cap_is_not_an_observed_zero() {
        let p = plan(&json!({"type":"web_search_20250305","name":"web_search","max_uses":2}));
        assert!(p.validate_usage(Some(paid()), None).is_err());
        for count in [0, 2, 3] {
            let u = Some(ServerToolUsage::Anthropic(AnthropicToolUsage {
                web_search_requests: Some(count),
                web_fetch_requests: None,
                code_execution_requests: None,
            }));
            assert_eq!(p.validate_usage(Some(paid()), u).is_ok(), count <= 2);
            assert_eq!(
                ToolAdmission::default()
                    .validate_usage(Some(paid()), u)
                    .is_ok(),
                count == 0
            );
        }
        // Legacy unpriced quantities stay captured as unknown monetary components.
        assert!(
            ToolAdmission::default()
                .validate_usage(
                    None,
                    Some(ServerToolUsage::Anthropic(AnthropicToolUsage {
                        web_search_requests: Some(9),
                        web_fetch_requests: None,
                        code_execution_requests: None
                    }))
                )
                .is_ok()
        );
    }
}
