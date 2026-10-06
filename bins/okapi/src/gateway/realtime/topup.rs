//! 会话内追加预扣。连接预扣只够一份 `max_output` 的量，会话却能跑满 8 分钟、
//! 累计任意多个 response；结算按「多退少补」照扣，不追加就能把余额扣成大负数。
//! 每个 `response.done` 后若累计金额超过已覆盖额度，再预扣一份（至少补足差额），
//! 余额不足即断开。追加的预扣只是准入闸：收尾时全部退回，再按累计用量一次 commit，
//! 所以透支最多是最后一个 response 的量。
use super::super::state::AppState;
use super::Prep;
use okapi_domain::{Money, TokenUsage};
use okapi_ledger::{LimitCaps, ReserveOutcome};
use okapi_pricing::calculate;
use uuid::Uuid;

pub(super) struct TopUps {
    covered: Money,
    chunk: Money,
    ids: Vec<Uuid>,
}

impl TopUps {
    pub fn new(reserved: Money) -> Self {
        Self {
            covered: reserved,
            chunk: reserved,
            ids: Vec::new(),
        }
    }

    /// 需要追加的额度；None = 已覆盖（算价失败也交给结算路径处理）。
    fn shortfall(&self, prep: &Prep, usage: TokenUsage) -> Option<Money> {
        let amount = calculate(&prep.book, &prep.calc, usage).ok()?.amount;
        let need = amount.saturating_sub(self.covered);
        (need.as_micros() > 0).then(|| need.max(self.chunk))
    }

    /// Err 带给客户端的错误码，调用方据此断开会话。
    pub async fn cover(
        &mut self,
        state: &AppState,
        prep: &Prep,
        usage: TokenUsage,
    ) -> Result<(), &'static str> {
        let Some(est) = self.shortfall(prep, usage) else {
            return Ok(());
        };
        let id = Uuid::new_v4();
        let outcome = state
            .reserve_for_key(
                prep.key.quota_limited,
                okapi_ledger::ReserveRequest {
                    user_id: prep.key.user_id,
                    api_key_id: prep.key.key_id,
                    request_id: id,
                    est,
                    // 限速与并发已在建连时计过，追加预扣只管钱
                    caps: LimitCaps::default(),
                    est_tokens: 0,
                },
                chrono::Utc::now(),
            )
            .await;
        match outcome {
            Ok(ReserveOutcome::Reserved { .. }) => {
                self.ids.push(id);
                self.covered = self.covered.saturating_add(est);
                Ok(())
            }
            Ok(ReserveOutcome::Insufficient { .. }) => Err(okapi_api::codes::INSUFFICIENT_QUOTA),
            Ok(ReserveOutcome::RateLimited { .. }) => Err(okapi_api::codes::RATE_LIMITED),
            Err(error) => {
                tracing::warn!(request_id = %prep.request_id, %error, "realtime top-up reservation failed");
                Err(okapi_api::codes::OVERLOADED)
            }
        }
    }

    /// 结算前退回全部追加预扣；退款失败的留给 sweep。
    pub async fn release(&mut self, state: &AppState, prep: &Prep) {
        for id in self.ids.drain(..) {
            if let Err(error) = state
                .ledger
                .refund(prep.key.user_id, prep.key.key_id, id)
                .await
            {
                tracing::error!(request_id = %prep.request_id, top_up = %id, %error, "realtime top-up refund failed; left for sweep");
            }
        }
    }
}
