use super::AppState;
use okapi_ledger::SettlementInput;
use sqlx::Connection as _;

impl AppState {
    pub(in crate::gateway) async fn prepare_settlement(&self, input: &mut SettlementInput<'_>) {
        if input.dimensions.diagnostics.is_none() {
            input.dimensions.diagnostics = super::super::diagnostics::snapshot();
        }
        if let Some(code) = input.error_code {
            let diagnostics = input
                .dimensions
                .diagnostics
                .get_or_insert_with(|| serde_json::json!({}));
            if diagnostics.get("error_phase").is_none() {
                diagnostics["error_phase"] =
                    serde_json::json!(super::super::diagnostics::phase(code));
            }
        } else if let Some(diagnostics) = input
            .dimensions
            .diagnostics
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
            && diagnostics
                .get("request_failed")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            diagnostics.remove("error_phase");
            diagnostics.remove("error_message");
        }
        // 来源 IP 记录开关（settings.record_ip_log，缺省 true）。收口在这里而非各端点：
        // 七个计费端点全部经 settle_write，关一处即全站不落 IP（PG 列与 CH 列一起）。
        // docs/database.md 早写着「记录与否走 settings.record_ip_log」，但此前全仓无人读它，
        // 站长关不掉——属隐私合规缺口而非功能缺失。
        if input.client_ip.is_some() && !self.record_ip_log().await {
            input.client_ip = None;
        }
        self.prepare_upstream_cost(input).await;
    }

    async fn prepare_upstream_cost(&self, input: &mut SettlementInput<'_>) {
        if input.log_type == 2 {
            if input
                .pricing_snapshot
                .as_ref()
                .is_none_or(|s| s.get("server_tool_cost_coverage").is_none())
                && let Some(coverage) = super::super::server_tools::ToolAdmission::default()
                    .cost_coverage(input.usage.server_tool_usage)
            {
                input
                    .pricing_snapshot
                    .get_or_insert_with(|| serde_json::json!({}))["server_tool_cost_coverage"] =
                    coverage;
            }
            if has_unpriced_tool_usage(input.pricing_snapshot.as_ref()) {
                input.upstream_cost = None;
                return;
            }
        }
        if input.upstream_cost.is_some() || input.log_type != 2 || input.list_price.is_negative() {
            return;
        }
        let Some(channel) = input.channel_id else {
            return;
        };
        // Only old payloads without provenance use the compatibility lookup.
        // A malformed existing basis must never be replaced with newer configuration.
        let has_basis = input
            .pricing_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.get("upstream_cost_basis").is_some());
        if !has_basis && let Some(cost_milli) = self.channel_cost_milli(channel).await {
            super::super::upstream_cost::pin_legacy(
                input
                    .pricing_snapshot
                    .get_or_insert_with(|| serde_json::json!({})),
                channel,
                cost_milli,
                input.list_price,
            );
        }
        input.upstream_cost = input.pricing_snapshot.as_ref().and_then(|snapshot| {
            super::super::upstream_cost::estimate(snapshot, channel, input.list_price)
        });
        if input.upstream_cost.is_none() {
            tracing::warn!(request_id = %input.request_id, "upstream cost basis invalid, unavailable or overflowing; estimate remains unknown");
        }
    }

    pub(crate) async fn persist_success(
        &self,
        input: SettlementInput<'_>,
    ) -> Result<bool, okapi_ledger::LedgerError> {
        // 先轮到该用户（结算队列）再占全局结算闸：排队的结算既不占闸也不占连接，
        // 一个大客户的结算堆积不会挡住其他用户落账；结算队列与准入队列分开，
        // 闸满时等待中的结算也不会挡住该用户的新请求预扣。
        let _turn = self
            .settlement_turns
            .wait(input.user_id, super::super::user_turns::SETTLEMENT_WAIT)
            .await?;
        let _permit = self.settle_gate.acquire().await;
        if input.dimensions.endpoint != "/v1/videos" {
            return okapi_ledger::sync::record(&self.pg, &self.ledger, input).await;
        }
        let mut guard = okapi_ledger::holds::UserGuard::acquire(&self.pg, input.user_id).await?;
        let mut tx = guard.connection()?.begin().await?;
        let inserted = okapi_ledger::sync::record_in_tx(&mut tx, input.clone()).await?;
        if let (Some(task_id), Some(channel_key_id)) =
            (input.upstream_request_id, input.channel_key_id)
        {
            sqlx::query!("INSERT INTO video_tasks(user_id,task_id,request_id,channel_key_id) VALUES ($1,$2,$3,$4) ON CONFLICT (request_id) DO NOTHING",input.user_id,task_id,input.request_id,channel_key_id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        if let Err(error) = guard.synchronize(&self.ledger).await {
            tracing::error!(request_id=%input.request_id,%error,"video charge awaiting hot ledger recovery");
        }
        Ok(inserted)
    }

    /// Persist the actual usage before closing Redis. Pending completions survive
    /// process loss and are retried by the worker; stats run once per inserted bill.
    ///
    /// 落账在受跟踪的后台任务里跑：客户端断开时 handler future 被丢弃，若结算随之中断，
    /// 调用方的失败守卫会先写 0 元失败账，按 request_id 先到先得，补记的扣费就被当重放跳过。
    /// 调用方须在调用前解除失败守卫。
    pub async fn settle_success(
        &self,
        mut input: SettlementInput<'_>,
    ) -> Result<bool, crate::gateway::error::AppError> {
        // 诊断挂在调用方任务的 task-local 上，离开前取走
        if input.dimensions.diagnostics.is_none() {
            input.dimensions.diagnostics = super::super::diagnostics::snapshot();
        }
        let owned = okapi_ledger::pg::OwnedSettlementInput::from(input);
        let (done, result) = tokio::sync::oneshot::channel();
        let state = self.clone();
        self.settlements.spawn(async move {
            let _ = done.send(state.settle_success_attached(owned.as_input()).await);
        });
        result
            .await
            .map_err(|_| crate::gateway::error::AppError::internal())?
    }

    async fn settle_success_attached(
        &self,
        mut input: SettlementInput<'_>,
    ) -> Result<bool, crate::gateway::error::AppError> {
        let _backlog = super::BacklogGuard::enter(&self.settle_backlog);
        self.prepare_settlement(&mut input).await;
        let journaled = super::super::settlement_retry::save_in_flight(self, &input)
            .await
            .is_ok();
        let mut delay = std::time::Duration::from_millis(200);
        for attempt in 0..3u8 {
            match Box::pin(self.persist_success(input.clone())).await {
                Ok(inserted) => {
                    if inserted {
                        self.sched
                            .kpi_record(input.usage.total_raw(), input.amount.as_micros(), false)
                            .await;
                    }
                    let _ = super::super::settlement_retry::remove(self, input.request_id).await;
                    return Ok(inserted);
                }
                Err(error) => {
                    tracing::error!(request_id=%input.request_id, %error, attempt, "durable settlement persistence failed");
                    if attempt < 2 {
                        tokio::time::sleep(delay).await;
                        delay *= 4;
                    } else {
                        if journaled {
                            tracing::warn!(request_id=%input.request_id, "settlement retained in retry journal");
                            let _ = super::super::settlement_retry::due_now(self, input.request_id)
                                .await;
                            return Ok(false);
                        }
                        let _ = self
                            .ledger
                            .refund(input.user_id, input.api_key_id, input.request_id)
                            .await;
                        return Err(error.into());
                    }
                }
            }
        }
        Err(crate::gateway::error::AppError::internal())
    }
}

/// A Token-only list price cannot represent complete cost for unpriced tool usage.
fn has_unpriced_tool_usage(snapshot: Option<&serde_json::Value>) -> bool {
    if snapshot
        .and_then(|s| s.get("server_tool_cost_coverage"))
        .is_some_and(|coverage| {
            coverage
                .get("complete")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        })
    {
        return true;
    }
    snapshot
        .and_then(|s| s.get("server_tool_fees"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                let no_observed_use = row.get("quantity").is_none_or(serde_json::Value::is_null)
                    || row.get("quantity").and_then(serde_json::Value::as_u64) == Some(0);
                !(row.get("requested").and_then(serde_json::Value::as_bool) == Some(false)
                    && no_observed_use)
                    && row.get("pricing").is_none_or(serde_json::Value::is_null)
                    && row.get("quantity").and_then(serde_json::Value::as_u64) != Some(0)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_or_malformed_native_coverage_cannot_be_replaced_with_token_only_cost() {
        for coverage in [
            serde_json::json!({"complete":false}),
            serde_json::json!({}),
            serde_json::json!(null),
            serde_json::json!({"complete":"true"}),
        ] {
            assert!(has_unpriced_tool_usage(Some(
                &serde_json::json!({"server_tool_cost_coverage":coverage})
            )));
        }
        assert!(!has_unpriced_tool_usage(Some(
            &serde_json::json!({"server_tool_cost_coverage":{"complete":true}})
        )));
    }

    #[test]
    fn only_unrequested_absent_or_zero_usage_is_exempt_from_unknown_cost() {
        for quantity in [serde_json::Value::Null, serde_json::json!(0)] {
            assert!(!has_unpriced_tool_usage(Some(
                &serde_json::json!({"server_tool_fees":[{"requested":false,"quantity":quantity,"pricing":null}]})
            )));
        }
        for (requested, quantity) in [
            (serde_json::json!(true), serde_json::Value::Null),
            (serde_json::Value::Null, serde_json::Value::Null),
            (serde_json::json!(false), serde_json::json!(1)),
            (serde_json::json!(false), serde_json::json!("bad")),
        ] {
            assert!(has_unpriced_tool_usage(Some(
                &serde_json::json!({"server_tool_fees":[{"requested":requested,"quantity":quantity,"pricing":null}]})
            )));
        }
    }

    #[test]
    fn missing_prices_do_not_turn_tool_costs_into_known_token_only_costs() {
        assert!(!has_unpriced_tool_usage(None));
        for quantity in [serde_json::Value::Null, serde_json::json!(1)] {
            assert!(has_unpriced_tool_usage(Some(
                &serde_json::json!({"server_tool_fees":[{"quantity":quantity,"pricing":null}]})
            )));
        }
        assert!(!has_unpriced_tool_usage(Some(
            &serde_json::json!({"server_tool_fees":[{"quantity":0,"pricing":null}]})
        )));
        assert!(!has_unpriced_tool_usage(Some(
            &serde_json::json!({"server_tool_fees":[{"quantity":null,"pricing":{"billing":"included"}}]})
        )));
    }
}
