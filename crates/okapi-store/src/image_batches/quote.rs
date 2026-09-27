use super::Error;
use serde::{Deserialize, Serialize};

/// Integer microdollars for one requested image, captured before accepting a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitQuote {
    pub amount: i64,
    pub original: i64,
    pub discount: i64,
    pub list_price: i64,
    pub upstream_cost: Option<i64>,
}
impl UnitQuote {
    pub fn total(&self, units: u32) -> Result<Self, Error> {
        let invalid = || Error::Invalid("batch_quote");
        let bound = 9_007_199_254_740_991_i64;
        if !(0..=bound).contains(&self.amount)
            || !(0..=bound).contains(&self.original)
            || !(0..=bound).contains(&self.list_price)
            || self
                .upstream_cost
                .is_some_and(|n| !(0..=bound).contains(&n))
            || self.original.checked_sub(self.discount) != Some(self.amount)
            || units > 200
        {
            return Err(invalid());
        }
        let scale = |n: i64| {
            n.checked_mul(i64::from(units))
                .filter(|n| (-bound..=bound).contains(n))
                .ok_or_else(invalid)
        };
        Ok(Self {
            amount: scale(self.amount)?,
            original: scale(self.original)?,
            discount: scale(self.discount)?,
            list_price: scale(self.list_price)?,
            upstream_cost: self.upstream_cost.map(scale).transpose()?,
        })
    }
    pub fn read(value: &serde_json::Value) -> Result<Self, Error> {
        serde_json::from_value(value.clone()).map_err(|_| Error::Invalid("batch_quote"))
    }
}
