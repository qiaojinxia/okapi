//! Payment-source validation and claiming within the caller's financial transaction.

use crate::{StoreError, admin::PaidOrder};

/// Fields extracted only after verifying the provider's signature and paid status.
pub struct PaymentProof<'a> {
    pub gateway: &'a str,
    pub merchant_id: &'a str,
    pub trade_no: &'a str,
    pub currency: &'a str,
    pub amount_minor: i64,
}

pub enum Acceptance {
    Applied(PaidOrder),
    Duplicate,
    Missing,
    Mismatch(&'static str),
    Conflict(&'static str),
    AwaitingSession,
}

/// The caller must commit the returned order together with its durable credit
/// or subscription grant. Any error must roll back the entire transaction.
#[allow(clippy::too_many_lines)]
pub async fn accept_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    order_no: &str,
    proof: &PaymentProof<'_>,
) -> Result<Acceptance, StoreError> {
    let Some(row) = sqlx::query!(
        r#"SELECT id,user_id,amount_micro,plan_id,subscription_snapshot,status,
                  gateway,currency,(pay_amount*100)::bigint AS amount_minor,
                  merchant_id,checkout_session_id,payment_contract_version,gateway_trade_no
           FROM recharge_orders WHERE order_no=$1 FOR UPDATE"#,
        order_no
    )
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(Acceptance::Missing);
    };
    for (matches, field) in [
        (row.gateway == proof.gateway, "gateway"),
        (
            row.currency.eq_ignore_ascii_case(proof.currency),
            "currency",
        ),
        (
            row.amount_minor == Some(proof.amount_minor) && proof.amount_minor > 0,
            "amount",
        ),
        (
            row.merchant_id.as_deref().map_or(
                row.payment_contract_version == 0 || proof.gateway == "stripe",
                |id| id == proof.merchant_id,
            ),
            "pid",
        ),
    ] {
        if !matches {
            return Ok(Acceptance::Mismatch(field));
        }
    }
    if proof.gateway == "stripe" {
        if let Some(session) = &row.checkout_session_id {
            if session != proof.trade_no {
                return Ok(Acceptance::Mismatch("session_id"));
            }
        } else if row.payment_contract_version > 0 {
            // A webhook may race the Checkout response. Do not acknowledge it
            // until the provider-created session has been durably bound.
            return Ok(Acceptance::AwaitingSession);
        }
    }
    if row.status != 0 {
        return Ok(
            if matches!(row.status, 1 | 3)
                && row.gateway_trade_no.as_deref() == Some(proof.trade_no)
            {
                Acceptance::Duplicate
            } else {
                Acceptance::Conflict("trade_no")
            },
        );
    }
    // Existing paid orders predate receipts. Do not backfill guessed merchant
    // identities or silently allow an old payment to be spent again.
    let legacy_claim = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM recharge_orders
           WHERE gateway=$1 AND gateway_trade_no=$2 AND status IN (1,3) AND id<>$3
             AND (merchant_id IS NULL OR merchant_id=$4)) AS "claimed!""#,
        proof.gateway,
        proof.trade_no,
        row.id,
        proof.merchant_id
    )
    .fetch_one(&mut **tx)
    .await?;
    if legacy_claim {
        return Ok(Acceptance::Conflict("trade_no"));
    }
    let claim = sqlx::query_scalar!(
        r#"INSERT INTO payment_receipts (gateway,merchant_id,trade_no,order_id,currency,amount_minor)
           VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING RETURNING order_id"#,
        proof.gateway,proof.merchant_id,proof.trade_no,row.id,proof.currency,proof.amount_minor
    ).fetch_optional(&mut **tx).await?;
    if claim.is_none() {
        return Ok(Acceptance::Conflict("trade_no"));
    }
    sqlx::query!(
        "UPDATE recharge_orders SET status=1,gateway_trade_no=$2,paid_at=now() WHERE id=$1",
        row.id,
        proof.trade_no
    )
    .execute(&mut **tx)
    .await?;
    Ok(Acceptance::Applied(PaidOrder {
        user_id: row.user_id,
        amount_micro: row.amount_micro,
        plan_id: row.plan_id,
        subscription_snapshot: row.subscription_snapshot,
    }))
}

pub async fn bind_checkout(
    pool: &sqlx::PgPool,
    order_no: &str,
    session: &str,
) -> Result<bool, StoreError> {
    Ok(sqlx::query!(
        "UPDATE recharge_orders SET checkout_session_id=$2 WHERE order_no=$1 AND gateway='stripe' AND status=0 AND checkout_session_id IS NULL AND payment_contract_version=1",
        order_no,session
    ).execute(pool).await?.rows_affected() == 1)
}
