use super::{
    Hold, Status, UserGuard, db,
    hot::{self, HotHold},
};
use crate::{BalanceLedger, LedgerError, RepairOutcome};
use okapi_domain::Money;
use serde_json::{Value, json};

pub struct Repaired {
    pub wallet: RepairOutcome,
    pub subscription: RepairOutcome,
}

impl UserGuard {
    /// Rebuild missing durable reservations together with both balances in one Redis script.
    /// Caller reads authoritative event totals while holding this same user guard.
    pub async fn repair(
        &mut self,
        ledger: &BalanceLedger,
        wallet: Money,
        subscription: Money,
    ) -> Result<Repaired, LedgerError> {
        let expected = hot::eval(
            ledger,
            concat!(
                include_str!("../lua/balance_state.lua"),
                "\nreturn ledger_balance_state(KEYS[1])"
            ),
            vec![format!("bal:{{{}}}", self.user_id)],
            Vec::new(),
        )
        .await?;
        let active = hot::active(ledger, self.user_id).await?;
        let ids: Vec<_> = active.keys().copied().collect();
        let holds: Vec<Hold> = sqlx::query_as("SELECT * FROM balance_holds WHERE user_id=$1 AND (state<>'closed' OR id=ANY($2)) ORDER BY id")
            .bind(self.user_id).bind(ids).fetch_all(&mut *self.connection).await?;
        let (epoch, until) = db::window(self, chrono::Utc::now())
            .await?
            .unwrap_or_default();
        let mut keys = vec![format!("bal:{{{}}}", self.user_id)];
        let mut manifest = Vec::new();
        for hold in &holds {
            let receipt = match hold.status {
                Status::Pending => None,
                Status::Held => Some(HotHold::from_hold(hold, false)?),
                Status::Closing | Status::Closed => Some(HotHold::from_hold(hold, true)?),
            };
            keys.push(format!("hold:{{{}}}:{}", self.user_id, hold.id));
            manifest.push(json!({"id":hold.id,"state":if hold.status==Status::Pending {"pending"}else{"confirmed"},
                "amount":hold.maximum_micro.to_string(),"key":hold.api_key_id.to_string(),"proof":hold.request_hash,"receipt":receipt,
                "seal":hold.cancel_requested.then(|| HotHold::sealed(hold))}));
        }
        let transfers: Vec<_> = crate::transfers::pending(self, self.user_id)
            .await?
            .into_iter()
            .filter(|row| row.applied_at.is_none())
            .map(|row| row.manifest())
            .collect();
        let fund_sequence = crate::transfers::high_water(self, self.user_id).await?;
        let raw = hot::eval(
            ledger,
            concat!(
                include_str!("../lua/balance_state.lua"),
                "\n",
                include_str!("../lua/hold_repair.lua")
            ),
            keys,
            vec![
                wallet.as_micros().to_string(),
                subscription.as_micros().to_string(),
                serde_json::to_string(&manifest)
                    .map_err(|_| LedgerError::InvalidHold("manifest"))?,
                epoch,
                until.to_string(),
                serde_json::to_string(&transfers)
                    .map_err(|_| LedgerError::InvalidHold("transfers"))?,
                fund_sequence.to_string(),
                expected,
            ],
        )
        .await?;
        let result = hot::response(&raw)?;
        let repaired = Repaired {
            wallet: parse(&result["wallet"])?,
            subscription: parse(&result["sub"])?,
        };
        sqlx::query("UPDATE balance_holds SET state='closed',updated_at=now() WHERE user_id=$1 AND state='closing'")
            .bind(self.user_id).execute(&mut *self.connection).await?;
        Ok(repaired)
    }
}
fn parse(value: &Value) -> Result<RepairOutcome, LedgerError> {
    let money = |key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .and_then(|v| v.parse().ok())
            .map(Money::from_micros)
            .ok_or(LedgerError::InvalidHold("repair_result"))
    };
    Ok(RepairOutcome {
        before: money("before")?,
        after: money("after")?,
        inflight: money("inflight")?,
    })
}
