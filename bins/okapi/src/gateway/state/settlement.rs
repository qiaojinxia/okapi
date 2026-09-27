use super::AppState;
use okapi_ledger::SettlementInput;

impl AppState {
    pub(super) async fn prepare_settlement(&self, input: &mut SettlementInput<'_>) {
        // 来源 IP 记录开关（settings.record_ip_log，缺省 true）。收口在这里而非各端点：
        // 七个计费端点全部经 settle_write，关一处即全站不落 IP（PG 列与 CH 列一起）。
        // docs/database.md 早写着「记录与否走 settings.record_ip_log」，但此前全仓无人读它，
        // 站长关不掉——属隐私合规缺口而非功能缺失。
        if input.client_ip.is_some() && !self.record_ip_log().await {
            input.client_ip = None;
        }
        // 上游成本（§11.18）统一在此折算而非各端点：官方价 × 渠道相对成本系数。
        // 只有成功计费且选中了渠道的记录才有成本；失败 / 退款记 None（CH 侧 0）。
        if input.upstream_cost.is_none()
            && input.log_type == 2
            && input.list_price.as_micros() >= 0
            && let Some(channel_id) = input.channel_id
            && let Some(cost_milli) = self.channel_cost_milli(channel_id).await
        {
            input.upstream_cost = Some(okapi_domain::Money::from_micros(
                i64::try_from(
                    i128::from(input.list_price.as_micros()) * i128::from(cost_milli) / 1000,
                )
                .unwrap_or(i64::MAX),
            ));
        }
    }

    /// Persist the actual usage before closing Redis. Pending completions survive
    /// process loss and are retried by the worker; stats run once per inserted bill.
    pub async fn settle_success(
        &self,
        mut input: SettlementInput<'_>,
    ) -> Result<bool, crate::gateway::error::AppError> {
        let _backlog = super::BacklogGuard::enter(&self.settle_backlog);
        self.prepare_settlement(&mut input).await;
        let _permit = self.settle_gate.acquire().await;
        let mut delay = std::time::Duration::from_millis(200);
        for attempt in 0..3u8 {
            match okapi_ledger::sync::record(&self.pg, &self.ledger, input.clone()).await {
                Ok(inserted) => {
                    if inserted {
                        self.sched
                            .kpi_record(input.usage.total_raw(), input.amount.as_micros(), false)
                            .await;
                    }
                    return Ok(inserted);
                }
                Err(error) => {
                    tracing::error!(request_id=%input.request_id, %error, attempt, "durable settlement persistence failed");
                    if attempt < 2 {
                        tokio::time::sleep(delay).await;
                        delay *= 4;
                    } else {
                        return Err(error.into());
                    }
                }
            }
        }
        Err(crate::gateway::error::AppError::internal())
    }
}
