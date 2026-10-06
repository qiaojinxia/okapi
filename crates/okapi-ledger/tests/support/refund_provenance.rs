//! Original monetary authority survives refund; adjustments add no measured usage.
use super::{Bed, Money, Uuid, Value, admin_refund, bed, json, record_credit, record_settlement};

async fn archive(bed: &Bed, id: Uuid) {
    // Use the retention receipt's exact columns, atomically removing this detail only.
    sqlx::query(
        "WITH removed AS (DELETE FROM billing_records WHERE request_id=$1 RETURNING *)
         INSERT INTO billing_record_receipts(request_id,user_id,api_key_id,group_code,model_name,
         channel_id,channel_key_id,status,amount_micro,original_amount_micro,discount_micro,
         upstream_cost_micro,is_stream,node,pool,pricing_snapshot,usage_details,created_at,source_window)
         SELECT request_id,user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,status,
         amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,node,pool,
         pricing_snapshot,usage_details,created_at,source_window FROM removed",
    )
    .bind(id)
    .execute(&bed.pg)
    .await
    .unwrap();
}

async fn funded() -> Bed {
    let bed = bed().await;
    record_credit(
        &bed.pg,
        bed.user_id,
        Money::from_micros(10_000),
        "recharge",
        "test",
        json!({}),
    )
    .await
    .unwrap();
    bed
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn live_and_archived_refunds_preserve_original_cost_coverage_snapshot_and_money() {
    for archived in [false, true] {
        for (cost, amount, discount) in [
            (Some(150), 240, 60),
            (Some(0), 240, 60),
            (None, 240, 60),
            (Some(150), 360, -60),
        ] {
            let bed = funded().await;
            let id = Uuid::new_v4();
            let coefficient = if cost == Some(0) { 0 } else { 500 };
            let snapshot = json!({"epoch":7,"mode":"ratio","upstream_cost_basis":{"version":1,"source":"selected_channel","channel_id":1,"relative_cost_milli":coefficient,"list_price_micro":300},"server_tool_fees":[{"quantity":2,"amount_micro":100}]});
            let mut input = bed.committed(id);
            input.amount = Money::from_micros(amount);
            input.discount = Money::from_micros(discount);
            input.delta_micro = -amount;
            input.upstream_cost = cost.map(Money::from_micros);
            input.pricing_snapshot = Some(snapshot.clone());
            record_settlement(&bed.pg, input).await.unwrap();
            let usage: Value =
                sqlx::query_scalar("SELECT usage_details FROM billing_records WHERE request_id=$1")
                    .bind(id)
                    .fetch_one(&bed.pg)
                    .await
                    .unwrap();
            if archived {
                archive(&bed, id).await;
            }
            let first = admin_refund(&bed.pg, id, "cost provenance", "test")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(first.amount.as_micros(), amount);
            assert_eq!(bed.wallet_snapshot().await, 10_000);
            assert_eq!(bed.key_used().await.0, 0);
            let outbox = bed.outbox(id).await;
            assert_eq!(outbox.len(), 2);
            let reverse = &outbox[1].1;
            assert_eq!(outbox[1].0, "billing.refunded");
            for (field, expected) in [
                ("amount_micro", -amount),
                ("original_amount_micro", -300),
                ("discount_micro", -discount),
                ("upstream_cost_micro", -cost.unwrap_or(0)),
            ] {
                assert_eq!(reverse[field], expected, "{archived}/{cost:?}: {reverse}");
            }
            assert_eq!(reverse["upstream_cost_known"], cost.is_some());
            assert_eq!(reverse["pricing_epoch"], 7);
            assert_eq!(
                serde_json::from_str::<Value>(reverse["ratio_snapshot"].as_str().unwrap()).unwrap(),
                snapshot
            );
            for field in [
                "prompt_tokens",
                "cached_tokens",
                "completion_tokens",
                "reasoning_tokens",
            ] {
                assert_eq!(reverse[field], 0);
            }
            assert!(reverse["server_tool_usage"].is_null());
            let original: (i16,i64,i64,i64,Option<i64>,Value,Value) = sqlx::query_as(
                "SELECT status,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,usage_details FROM billing_records WHERE request_id=$1
                 UNION ALL SELECT status,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,usage_details FROM billing_record_receipts WHERE request_id=$1")
                .bind(id).fetch_one(&bed.pg).await.unwrap();
            assert_eq!(original, (30, amount, 300, discount, cost, snapshot, usage));
            assert!(
                admin_refund(&bed.pg, id, "repeat", "test")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(bed.outbox(id).await.len(), 2);
            assert_eq!(bed.events(id).await.len(), 2);
            assert_eq!(bed.wallet_snapshot().await, 10_000);
            assert_eq!(bed.key_used().await.0, 0);
        }
    }
}

#[tokio::test]
async fn legacy_refunds_do_not_invent_pricing_epoch_or_snapshot() {
    for archived in [false, true] {
        for snapshot in [
            None,
            Some(json!({"epoch":"7"})),
            Some(json!({"epoch":true})),
        ] {
            let bed = funded().await;
            let id = Uuid::new_v4();
            let mut input = bed.committed(id);
            input.pricing_epoch = None;
            input.pricing_snapshot = snapshot.clone();
            input.upstream_cost = None;
            record_settlement(&bed.pg, input).await.unwrap();
            if archived {
                archive(&bed, id).await;
            }
            admin_refund(&bed.pg, id, "legacy", "test")
                .await
                .unwrap()
                .unwrap();
            let outbox = bed.outbox(id).await;
            assert!(outbox[1].1["pricing_epoch"].is_null());
            assert_eq!(
                outbox[1].1["ratio_snapshot"],
                snapshot.as_ref().map(Value::to_string).unwrap_or_default()
            );
            assert_eq!(outbox[1].1["upstream_cost_known"], false);
        }
    }
}

#[tokio::test]
async fn unrepresentable_reversal_rolls_back_live_and_archived_refunds_atomically() {
    for archived in [false, true] {
        for column in [
            "amount_micro",
            "original_amount_micro",
            "discount_micro",
            "upstream_cost_micro",
        ] {
            let bed = funded().await;
            let id = Uuid::new_v4();
            record_settlement(&bed.pg, bed.committed(id)).await.unwrap();
            // Corrupt one saved amount to exercise fail-closed transaction rollback.
            sqlx::query("UPDATE billing_records SET amount_micro=CASE WHEN $2='amount_micro' THEN $3 ELSE amount_micro END,
                original_amount_micro=CASE WHEN $2='original_amount_micro' THEN $3 ELSE original_amount_micro END,
                discount_micro=CASE WHEN $2='discount_micro' THEN $3 ELSE discount_micro END,
                upstream_cost_micro=CASE WHEN $2='upstream_cost_micro' THEN $3 ELSE upstream_cost_micro END WHERE request_id=$1")
                .bind(id).bind(column).bind(i64::MIN).execute(&bed.pg).await.unwrap();
            if archived {
                archive(&bed, id).await;
            }
            assert!(
                matches!(
                    admin_refund(&bed.pg, id, "overflow", "test").await,
                    Err(okapi_ledger::LedgerError::InvalidSettlement)
                ),
                "{archived}/{column}"
            );
            let status: i16 = sqlx::query_scalar(
                "SELECT status FROM billing_financial_records WHERE request_id=$1",
            )
            .bind(id)
            .fetch_one(&bed.pg)
            .await
            .unwrap();
            assert_eq!(status, 20);
            assert_eq!(bed.wallet_snapshot().await, 9760);
            assert_eq!(bed.key_used().await.0, 240);
            assert_eq!(bed.events(id).await.len(), 1);
            assert_eq!(bed.outbox(id).await.len(), 1);
        }
    }
}
