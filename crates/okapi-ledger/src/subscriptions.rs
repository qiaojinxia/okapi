//! 订阅池编排（IMPLEMENTATION §11.28）：PG 状态翻转 → Redis 第二池置额 → pool=1 事件。
//!
//! 三条来路（支付回调 / 兑换码核销 / 管理员发放）与 worker（滚窗 / 到期）都走这里，
//! 保证顺序一致：**先 PG 状态，再热账本，再事件**——PG 翻转失败什么都不动；热账本
//! 与事件之间崩溃由两池对账检出（`sub + Σ在途₁ == Σ events(pool=1)`）。
//! 不碰鉴权缓存：分组是否变了由返回值告知，调用方自行 `auth:ver` 失效（gateway 侧概念）。

use crate::error::LedgerError;
use crate::redis::BalanceLedger;
use okapi_domain::Money;
use okapi_store::subscriptions::{self as store, ActivateOutcome, SubPlan, Subscription};
use sqlx::PgPool;

/// 授予结果。
#[derive(Debug)]
pub enum Granted {
    /// 新订阅：池已注资、`sub_grant` 已记。
    Activated(Subscription),
    /// 同套餐续期：只延了可用截止，窗口与池余额不动。
    Renewed(Subscription),
}

impl Granted {
    #[must_use]
    pub fn subscription(&self) -> &Subscription {
        match self {
            Self::Activated(s) | Self::Renewed(s) => s,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Activated(_) => "activated",
            Self::Renewed(_) => "renewed",
        }
    }

    /// 用户分组集合是否变了（调用方据此失效鉴权缓存）。
    #[must_use]
    pub const fn group_changed(&self) -> bool {
        matches!(self, Self::Activated(s) if s.granted_group)
    }
}

/// 激活 / 续期并给池注资。`source` 形如 `purchase:<order_no>` / `redeem:<code_id>` / `admin:<uid>`。
/// 激活期内换别的套餐 → `LedgerError::SubscriptionActive(当前套餐码)`。
pub async fn grant(
    pg: &PgPool,
    ledger: &BalanceLedger,
    user_id: i64,
    plan: &SubPlan,
    source: &str,
    actor: &str,
) -> Result<Granted, LedgerError> {
    let now = chrono::Utc::now();
    match store::activate(pg, user_id, plan, now, source).await? {
        ActivateOutcome::Conflict { active_plan_code } => {
            Err(LedgerError::SubscriptionActive(active_plan_code))
        }
        ActivateOutcome::Renewed(sub) => {
            // 只延可用截止；窗口与池余额不动（续期不是加额）
            ledger
                .sub_touch_until(user_id, sub.sub_until().timestamp())
                .await?;
            Ok(Granted::Renewed(sub))
        }
        ActivateOutcome::Activated(sub) => {
            fund_window(
                pg,
                ledger,
                &sub,
                Money::from_micros(sub.quota_micro),
                "sub_grant",
                actor,
                serde_json::json!({
                    "plan_code": plan.plan_code,
                    "source": source,
                    "subscription_id": sub.id,
                }),
            )
            .await?;
            Ok(Granted::Activated(sub))
        }
    }
}

/// 池置额 + 记事件（激活 / 滚窗 / 到期共用）。`quota` 为新窗额度（到期传 0 → `sub_until` 也清 0）。
/// 事件 delta = `sub_set` 前后差，所以跨窗累计仍满足 pool=1 不变式。
pub async fn fund_window(
    pg: &PgPool,
    ledger: &BalanceLedger,
    sub: &Subscription,
    quota: Money,
    event_type: &str,
    actor: &str,
    payload: serde_json::Value,
) -> Result<Money, LedgerError> {
    let until = if quota.is_zero() {
        0
    } else {
        sub.sub_until().timestamp()
    };
    let outcome = ledger.sub_set(sub.user_id, quota, until).await?;
    let delta = Money::from_micros(
        outcome
            .after
            .as_micros()
            .saturating_sub(outcome.before.as_micros()),
    );
    crate::pg::record_sub_event(
        pg,
        sub.user_id,
        delta,
        outcome.after,
        event_type,
        actor,
        payload,
    )
    .await?;
    Ok(outcome.after)
}

/// 结束一条订阅（2 到期 / 3 取消）：PG 翻转（含收组）→ 池清零 → `sub_expire` 事件。
/// 已非激活 → `Ok(None)`（幂等）。返回的 `Subscription.granted_group` 为真表示分组被收回。
pub async fn end(
    pg: &PgPool,
    ledger: &BalanceLedger,
    subscription_id: i64,
    status: i16,
    actor: &str,
) -> Result<Option<Subscription>, LedgerError> {
    let Some(sub) = store::finish(pg, subscription_id, status).await? else {
        return Ok(None);
    };
    fund_window(
        pg,
        ledger,
        &sub,
        Money::ZERO,
        "sub_expire",
        actor,
        serde_json::json!({
            "plan_code": sub.plan_code,
            "subscription_id": sub.id,
            "status": status,
            "group_revoked": sub.granted_group.then(|| sub.group_code.clone()).flatten(),
        }),
    )
    .await?;
    Ok(Some(sub))
}

/// 滚窗：窗口前滚到覆盖 `now` 的那一格（连跳不补发）→ 池重置到 quota（在途保留）→ `sub_reset`。
pub async fn roll(
    pg: &PgPool,
    ledger: &BalanceLedger,
    sub: &Subscription,
    now: chrono::DateTime<chrono::Utc>,
    actor: &str,
) -> Result<Subscription, LedgerError> {
    let (ws, we) = store::advance_window(sub.window_end, sub.period(), now);
    store::roll_window(pg, sub.id, ws, we).await?;
    let rolled = Subscription {
        window_start: ws,
        window_end: we,
        ..sub.clone()
    };
    fund_window(
        pg,
        ledger,
        &rolled,
        Money::from_micros(rolled.quota_micro),
        "sub_reset",
        actor,
        serde_json::json!({
            "plan_code": rolled.plan_code,
            "subscription_id": rolled.id,
            "window_start": ws,
            "window_end": we,
        }),
    )
    .await?;
    Ok(rolled)
}

/// worker 一轮：到期的收尾、到窗的滚窗。返回 `(rolled, expired)` 计数。
pub async fn tick(
    pg: &PgPool,
    ledger: &BalanceLedger,
    now: chrono::DateTime<chrono::Utc>,
    limit: i64,
) -> Result<TickReport, LedgerError> {
    let mut report = TickReport::default();
    for sub in store::due(pg, now, limit).await? {
        if sub.expires_at <= now {
            if let Some(ended) = end(pg, ledger, sub.id, 2, "system:worker").await? {
                report.expired += 1;
                report.group_changed |= ended.granted_group;
            }
        } else {
            roll(pg, ledger, &sub, now, "system:worker").await?;
            report.rolled += 1;
        }
    }
    Ok(report)
}

/// 一轮滚窗 / 到期的结果。
#[derive(Debug, Default, Clone, Copy)]
pub struct TickReport {
    pub rolled: u32,
    pub expired: u32,
    /// 有订阅收回了分组：调用方需失效鉴权缓存。
    pub group_changed: bool,
}
