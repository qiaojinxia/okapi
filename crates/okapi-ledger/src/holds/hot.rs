use super::{Hold, MAXIMUM_MICROS};
use crate::{BalanceLedger, LedgerError};
use chrono::{DateTime, Utc};
use fred::interfaces::{HashesInterface, KeysInterface, LuaInterface};
use okapi_domain::Money;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const RESERVE: &str = concat!(
    include_str!("../lua/hold_concurrency.lua"),
    "\n",
    include_str!("../lua/hold_reserve.lua")
);
const CLOSE: &str = concat!(
    include_str!("../lua/hold_concurrency.lua"),
    "\n",
    include_str!("../lua/hold_close.lua")
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HotHold {
    pub amount: String,
    pub key: String,
    pub proof: String,
    pub pool: i16,
    pub epoch: String,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
}
impl HotHold {
    pub(super) fn sealed(hold: &Hold) -> Self {
        Self {
            amount: hold.maximum_micro.to_string(),
            key: hold.api_key_id.to_string(),
            proof: hold.request_hash.clone(),
            pool: 0,
            epoch: String::new(),
            phase: "closed".into(),
            actual: Some("0".into()),
            credit: Some("0".into()),
        }
    }
    pub(super) fn from_hold(hold: &Hold, closed: bool) -> Result<Self, LedgerError> {
        Ok(Self {
            amount: hold.maximum_micro.to_string(),
            key: hold.api_key_id.to_string(),
            proof: hold.request_hash.clone(),
            pool: hold.pool()?.as_i16(),
            epoch: hold.source_window.clone().unwrap_or_default(),
            phase: if closed { "closed" } else { "held" }.into(),
            actual: closed
                .then(|| hold.actual_micro.map(|v| v.to_string()))
                .flatten(),
            credit: closed
                .then(|| hold.credit_micro.map(|v| v.to_string()))
                .flatten(),
        })
    }
    pub(crate) fn amount(&self) -> Result<Money, LedgerError> {
        self.amount
            .parse::<i64>()
            .ok()
            .filter(|v| (0..=MAXIMUM_MICROS).contains(v))
            .map(Money::from_micros)
            .ok_or(LedgerError::InvalidHold("receipt_amount"))
    }
    fn validate(&self, hold: &Hold) -> Result<(), LedgerError> {
        if self.amount != hold.maximum_micro.to_string()
            || self.key != hold.api_key_id.to_string()
            || self.proof != hold.request_hash
            || !matches!(self.pool, 0 | 1)
            || (self.pool == 1) == self.epoch.is_empty()
            || self.phase != "held"
            || self.actual.is_some()
            || self.credit.is_some()
        {
            return Err(LedgerError::HoldConflict);
        }
        Ok(())
    }
}
/// None means no debit ever happened; a permanent tombstone blocks late admission.
pub(super) async fn seal(
    ledger: &BalanceLedger,
    hold: &Hold,
) -> Result<Option<HotHold>, LedgerError> {
    let raw = eval(
        ledger,
        include_str!("../lua/hold_seal.lua"),
        keys(hold),
        vec![
            hold.id.to_string(),
            hold.maximum_micro.to_string(),
            hold.api_key_id.to_string(),
            hold.request_hash.clone(),
        ],
    )
    .await?;
    let value = response(&raw)?;
    if value
        == serde_json::to_value(HotHold::sealed(hold))
            .map_err(|_| LedgerError::InvalidHold("receipt"))?
    {
        return Ok(None);
    }
    let receipt: HotHold =
        serde_json::from_value(value).map_err(|_| LedgerError::InvalidHold("receipt"))?;
    receipt.validate(hold)?;
    Ok(Some(receipt))
}
pub(super) enum Reserved {
    Held(HotHold),
    Insufficient(Money),
    ConcurrencyLimited,
}

pub(super) fn keys(hold: &Hold) -> Vec<String> {
    vec![
        format!("bal:{{{}}}", hold.user_id),
        format!("hold:{{{}}}:{}", hold.user_id, hold.id),
    ]
}
pub(super) async fn eval(
    ledger: &BalanceLedger,
    script: &str,
    keys: Vec<String>,
    args: Vec<String>,
) -> Result<String, LedgerError> {
    tokio::time::timeout(
        Duration::from_secs(10),
        ledger.client().eval(script, keys, args),
    )
    .await
    .map_err(|_| LedgerError::HoldRecoveryRequired)?
    .map_err(Into::into)
}
pub(super) fn response(raw: &str) -> Result<serde_json::Value, LedgerError> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| LedgerError::InvalidHold("receipt"))?;
    match value.get("error").and_then(serde_json::Value::as_str) {
        None | Some("insufficient" | "concurrency") => Ok(value),
        Some("conflict") => Err(LedgerError::HoldConflict),
        Some("recovery_required" | "window_required" | "window_conflict") => {
            Err(LedgerError::HoldRecoveryRequired)
        }
        Some(_) => Err(LedgerError::InvalidHold("hot_state")),
    }
}
pub(super) async fn active(
    ledger: &BalanceLedger,
    user_id: i64,
) -> Result<std::collections::BTreeMap<uuid::Uuid, HotHold>, LedgerError> {
    let values: std::collections::HashMap<String, String> = tokio::time::timeout(
        Duration::from_secs(10),
        ledger.client().hgetall(format!("bal:{{{user_id}}}")),
    )
    .await
    .map_err(|_| LedgerError::HoldRecoveryRequired)??;
    let mut out = std::collections::BTreeMap::new();
    for (field, value) in values {
        let Some(id) = field.strip_prefix("h:") else {
            continue;
        };
        let id = uuid::Uuid::parse_str(id).map_err(|_| LedgerError::InvalidHold("identity"))?;
        let hold: HotHold =
            serde_json::from_str(&value).map_err(|_| LedgerError::InvalidHold("receipt"))?;
        if hold.phase != "held"
            || !matches!(hold.pool, 0 | 1)
            || hold.actual.is_some()
            || hold.credit.is_some()
            || (hold.pool == 1) == hold.epoch.is_empty()
        {
            return Err(LedgerError::InvalidHold("receipt"));
        }
        hold.amount()?;
        out.insert(id, hold);
    }
    Ok(out)
}

pub(super) async fn reserve(
    ledger: &BalanceLedger,
    hold: &Hold,
    now: DateTime<Utc>,
    window: Option<(String, i64)>,
    concurrency: i32,
) -> Result<Reserved, LedgerError> {
    let (epoch, until) = window.unwrap_or_default();
    let mut keys = keys(hold);
    keys.push(format!("conc:{{{}}}:k:{}", hold.user_id, hold.api_key_id));
    let raw = eval(
        ledger,
        RESERVE,
        keys,
        vec![
            hold.id.to_string(),
            hold.maximum_micro.to_string(),
            hold.api_key_id.to_string(),
            hold.request_hash.clone(),
            now.timestamp().to_string(),
            epoch,
            until.to_string(),
            concurrency.to_string(),
        ],
    )
    .await?;
    let value = response(&raw)?;
    if value.get("error").and_then(serde_json::Value::as_str) == Some("concurrency") {
        return Ok(Reserved::ConcurrencyLimited);
    }
    if value.get("error").is_some() {
        let balance = value
            .get("balance")
            .and_then(serde_json::Value::as_str)
            .and_then(|v| v.parse::<i64>().ok())
            .ok_or(LedgerError::InvalidHold("balance"))?;
        return Ok(Reserved::Insufficient(Money::from_micros(balance)));
    }
    let receipt: HotHold =
        serde_json::from_value(value).map_err(|_| LedgerError::InvalidHold("receipt"))?;
    receipt.validate(hold)?;
    Ok(Reserved::Held(receipt))
}
pub(super) async fn verify(ledger: &BalanceLedger, hold: &Hold) -> Result<(), LedgerError> {
    let mut key_names = keys(hold).into_iter();
    let balance = key_names.next().ok_or(LedgerError::InvalidHold("keys"))?;
    let key = key_names.next().ok_or(LedgerError::InvalidHold("keys"))?;
    let raw: Option<String> =
        tokio::time::timeout(Duration::from_secs(10), ledger.client().get(key))
            .await
            .map_err(|_| LedgerError::HoldRecoveryRequired)??;
    let raw = raw.ok_or(LedgerError::HoldRecoveryRequired)?;
    let receipt: HotHold =
        serde_json::from_str(&raw).map_err(|_| LedgerError::InvalidHold("receipt"))?;
    receipt.validate(hold)?;
    if receipt.pool != hold.pool()?.as_i16()
        || receipt.epoch != hold.source_window.clone().unwrap_or_default()
    {
        return Err(LedgerError::HoldConflict);
    }
    let field: Option<String> = tokio::time::timeout(
        Duration::from_secs(10),
        ledger.client().hget(balance, format!("h:{}", hold.id)),
    )
    .await
    .map_err(|_| LedgerError::HoldRecoveryRequired)??;
    if field.as_deref() != Some(raw.as_str()) {
        return Err(LedgerError::HoldRecoveryRequired);
    }
    Ok(())
}
pub(super) async fn close(ledger: &BalanceLedger, hold: &Hold) -> Result<(), LedgerError> {
    let wanted = HotHold::from_hold(hold, true)?;
    let encoded =
        serde_json::to_string(&wanted).map_err(|_| LedgerError::InvalidHold("receipt"))?;
    let raw = eval(
        ledger,
        CLOSE,
        keys(hold),
        vec![hold.id.to_string(), encoded],
    )
    .await?;
    let value = response(&raw)?;
    let expected = serde_json::to_value(wanted).map_err(|_| LedgerError::InvalidHold("receipt"))?;
    if value != expected {
        return Err(LedgerError::HoldConflict);
    }
    Ok(())
}
pub(super) async fn current_window(
    ledger: &BalanceLedger,
    user_id: i64,
) -> Result<(String, i64), LedgerError> {
    let (epoch, until): (Option<String>, Option<String>) = tokio::time::timeout(
        Duration::from_secs(10),
        ledger
            .client()
            .hmget(format!("bal:{{{user_id}}}"), vec!["sub_epoch", "sub_until"]),
    )
    .await
    .map_err(|_| LedgerError::HoldRecoveryRequired)??;
    Ok((
        epoch.unwrap_or_default(),
        until
            .map(|v| v.parse::<i64>())
            .transpose()
            .map_err(|_| LedgerError::InvalidHold("window"))?
            .unwrap_or(0),
    ))
}
