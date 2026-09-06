//! 订阅实例（`plans.kind = 1` → `user_subscriptions`；IMPLEMENTATION §11.28）。
//!
//! PG 只管状态与窗口边界；池余额的热值在 Redis（`okapi_ledger::BalanceLedger::sub_set`），
//! 权威值是 `billing_events WHERE pool = 1` 的和——两者都由调用方（console / worker）在
//! 这里的状态翻转成功后处理。这里不碰 Redis，也不记事件。

use crate::error::StoreError;
use chrono::{DateTime, Duration, Months, Utc};
use sqlx::PgPool;

/// 周期：1 日 2 周 3 月（自激活时刻起算，非自然日历）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
    Month,
}

impl Period {
    #[must_use]
    pub const fn from_i16(v: i16) -> Option<Self> {
        match v {
            1 => Some(Self::Day),
            2 => Some(Self::Week),
            3 => Some(Self::Month),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::Day => 1,
            Self::Week => 2,
            Self::Month => 3,
        }
    }

    /// 窗口终点。月周期用日历月（1/31 起算 → 2/28|29 → 3/28|29，chrono `Months` 语义）。
    #[must_use]
    pub fn next(self, start: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::Day => start + Duration::days(1),
            Self::Week => start + Duration::weeks(1),
            Self::Month => start
                .checked_add_months(Months::new(1))
                .unwrap_or(start + Duration::days(30)),
        }
    }
}

/// 订阅套餐（`plans.kind = 1`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SubPlan {
    pub id: i64,
    pub plan_code: String,
    pub display_name: String,
    /// 每窗额度（micro）。
    pub quota_micro: i64,
    pub group_code: Option<String>,
    /// 售价（0 = 不可自助购买）。
    pub price_micro: i64,
    pub period: i16,
    pub duration_days: i32,
    pub sort_order: i32,
    pub description: Option<String>,
}

impl SubPlan {
    #[must_use]
    pub fn period(&self) -> Period {
        // 表级 CHECK 保证 kind=1 时 period ∈ {1,2,3}；防御性缺省按月
        Period::from_i16(self.period).unwrap_or(Period::Month)
    }
}

/// 启用中的订阅套餐（门户套餐页 / 购买校验）。
pub async fn list_sub_plans(pool: &PgPool) -> Result<Vec<SubPlan>, StoreError> {
    let rows = sqlx::query_as!(
        SubPlan,
        r#"
        SELECT id, plan_code, display_name, grant_micro AS quota_micro, group_code, price_micro,
               period AS "period!", duration_days AS "duration_days!", sort_order, description
        FROM plans
        WHERE kind = 1 AND status = 1
        ORDER BY sort_order, id
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 按 plan_code 找启用中的订阅套餐。
pub async fn find_sub_plan(pool: &PgPool, plan_code: &str) -> Result<Option<SubPlan>, StoreError> {
    let row = sqlx::query_as!(
        SubPlan,
        r#"
        SELECT id, plan_code, display_name, grant_micro AS quota_micro, group_code, price_micro,
               period AS "period!", duration_days AS "duration_days!", sort_order, description
        FROM plans
        WHERE plan_code = $1 AND kind = 1 AND status = 1
        "#,
        plan_code
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 按 id 找订阅套餐（支付回调 / 兑换码核销时套餐可能已停用，故不限 status：
/// 用户已付钱 / 已核销，套餐下架不该让他拿不到东西）。
pub async fn sub_plan_by_id(pool: &PgPool, id: i64) -> Result<Option<SubPlan>, StoreError> {
    let row = sqlx::query_as!(
        SubPlan,
        r#"
        SELECT id, plan_code, display_name, grant_micro AS quota_micro, group_code, price_micro,
               period AS "period!", duration_days AS "duration_days!", sort_order, description
        FROM plans
        WHERE id = $1 AND kind = 1
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 一条订阅实例（含套餐展示字段）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Subscription {
    pub id: i64,
    pub user_id: i64,
    pub plan_id: i64,
    pub plan_code: String,
    pub display_name: String,
    pub period: i16,
    pub status: i16,
    pub quota_micro: i64,
    pub group_code: Option<String>,
    pub granted_group: bool,
    pub starts_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub source: String,
}

impl Subscription {
    /// Redis `sub_until`：池可用截止 = min(window_end, expires_at)。
    #[must_use]
    pub fn sub_until(&self) -> DateTime<Utc> {
        self.window_end.min(self.expires_at)
    }

    #[must_use]
    pub fn period(&self) -> Period {
        Period::from_i16(self.period).unwrap_or(Period::Month)
    }
}

/// 当前激活订阅（至多一条，partial unique index 保证）。
pub async fn active_for_user(
    pool: &PgPool,
    user_id: i64,
) -> Result<Option<Subscription>, StoreError> {
    let row = sqlx::query_as!(
        Subscription,
        r#"
        SELECT s.id, s.user_id, s.plan_id, p.plan_code, p.display_name, p.period AS "period!", s.status,
               s.quota_micro, p.group_code, s.granted_group, s.starts_at, s.expires_at,
               s.window_start, s.window_end, s.source
        FROM user_subscriptions s JOIN plans p ON p.id = s.plan_id
        WHERE s.user_id = $1 AND s.status = 1
        "#,
        user_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 用户订阅历史（门户 / 管理端）。
pub async fn history_for_user(
    pool: &PgPool,
    user_id: i64,
    limit: i64,
) -> Result<Vec<Subscription>, StoreError> {
    let rows = sqlx::query_as!(
        Subscription,
        r#"
        SELECT s.id, s.user_id, s.plan_id, p.plan_code, p.display_name, p.period AS "period!", s.status,
               s.quota_micro, p.group_code, s.granted_group, s.starts_at, s.expires_at,
               s.window_start, s.window_end, s.source
        FROM user_subscriptions s JOIN plans p ON p.id = s.plan_id
        WHERE s.user_id = $1
        ORDER BY s.id DESC
        LIMIT $2
        "#,
        user_id,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 激活结果。
#[derive(Debug)]
pub enum ActivateOutcome {
    /// 新订阅：调用方 `sub_set(quota, sub_until)` + 记 `sub_grant`。
    Activated(Subscription),
    /// 同套餐续期：只延 `expires_at`（窗口与池余额不动）；调用方刷新 `sub_until`。
    Renewed(Subscription),
    /// 激活期内换别的套餐：409 `subscription_active`（升降级 backlog）。
    Conflict { active_plan_code: String },
}

/// 激活 / 续期（购买回调、兑换码核销、管理员发放共用）。
///
/// 单事务：锁当前激活行 → 同套餐续期 / 异套餐冲突 / 无则插入。分组只在用户**不在组里**时
/// 才加并标 `granted_group = true`，到期只收回订阅新加的那份。
pub async fn activate(
    pool: &PgPool,
    user_id: i64,
    plan: &SubPlan,
    now: DateTime<Utc>,
    source: &str,
) -> Result<ActivateOutcome, StoreError> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query!(
        r#"SELECT id, plan_id, expires_at FROM user_subscriptions
           WHERE user_id = $1 AND status = 1 FOR UPDATE"#,
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let duration = Duration::days(i64::from(plan.duration_days));

    if let Some(cur) = current {
        if cur.plan_id != plan.id {
            let active_plan_code =
                sqlx::query_scalar!(r#"SELECT plan_code FROM plans WHERE id = $1"#, cur.plan_id)
                    .fetch_one(&mut *tx)
                    .await?;
            tx.rollback().await?;
            return Ok(ActivateOutcome::Conflict { active_plan_code });
        }
        // 续期：从"当前到期时刻"或"现在"中较晚者起算，worker 滞后不该吃掉用户的天数
        let base = cur.expires_at.max(now);
        sqlx::query!(
            r#"UPDATE user_subscriptions
               SET expires_at = $2, source = $3, updated_at = now() WHERE id = $1"#,
            cur.id,
            base + duration,
            source
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let renewed = active_for_user(pool, user_id)
            .await?
            .ok_or(StoreError::InvalidData("subscription_vanished"))?;
        return Ok(ActivateOutcome::Renewed(renewed));
    }

    let mut granted_group = false;
    if let Some(group) = plan.group_code.as_deref() {
        let inserted = sqlx::query!(
            r#"INSERT INTO user_groups (user_id, group_code, priority) VALUES ($1, $2, 0)
               ON CONFLICT (user_id, group_code) DO NOTHING"#,
            user_id,
            group
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        granted_group = inserted > 0;
    }
    let window_end = plan.period().next(now);
    sqlx::query!(
        r#"
        INSERT INTO user_subscriptions
            (user_id, plan_id, status, starts_at, expires_at, window_start, window_end,
             quota_micro, granted_group, source)
        VALUES ($1, $2, 1, $3, $4, $3, $5, $6, $7, $8)
        "#,
        user_id,
        plan.id,
        now,
        now + duration,
        window_end,
        plan.quota_micro,
        granted_group,
        source
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let created = active_for_user(pool, user_id)
        .await?
        .ok_or(StoreError::InvalidData("subscription_vanished"))?;
    Ok(ActivateOutcome::Activated(created))
}

/// 结束一条激活订阅（2 到期 / 3 取消）；返回被结束的那条供调用方清池、收组。
/// 已非激活 → None（幂等）。
pub async fn finish(
    pool: &PgPool,
    subscription_id: i64,
    status: i16,
) -> Result<Option<Subscription>, StoreError> {
    let before = sqlx::query_as!(
        Subscription,
        r#"
        SELECT s.id, s.user_id, s.plan_id, p.plan_code, p.display_name, p.period AS "period!", s.status,
               s.quota_micro, p.group_code, s.granted_group, s.starts_at, s.expires_at,
               s.window_start, s.window_end, s.source
        FROM user_subscriptions s JOIN plans p ON p.id = s.plan_id
        WHERE s.id = $1 AND s.status = 1
        "#,
        subscription_id
    )
    .fetch_optional(pool)
    .await?;
    let Some(sub) = before else {
        return Ok(None);
    };
    let flipped = sqlx::query!(
        r#"UPDATE user_subscriptions SET status = $2, updated_at = now()
           WHERE id = $1 AND status = 1"#,
        subscription_id,
        status
    )
    .execute(pool)
    .await?
    .rows_affected();
    if flipped == 0 {
        return Ok(None); // 竞争：别处已结束
    }
    if sub.granted_group
        && let Some(group) = sub.group_code.as_deref()
    {
        sqlx::query!(
            r#"DELETE FROM user_groups WHERE user_id = $1 AND group_code = $2"#,
            sub.user_id,
            group
        )
        .execute(pool)
        .await?;
    }
    Ok(Some(sub))
}

/// worker 扫描：窗口已到点（含已过 `expires_at` 的）激活订阅。
pub async fn due(
    pool: &PgPool,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Subscription>, StoreError> {
    let rows = sqlx::query_as!(
        Subscription,
        r#"
        SELECT s.id, s.user_id, s.plan_id, p.plan_code, p.display_name, p.period AS "period!", s.status,
               s.quota_micro, p.group_code, s.granted_group, s.starts_at, s.expires_at,
               s.window_start, s.window_end, s.source
        FROM user_subscriptions s JOIN plans p ON p.id = s.plan_id
        WHERE s.status = 1 AND (s.window_end <= $1 OR s.expires_at <= $1)
        ORDER BY s.window_end
        LIMIT $2
        "#,
        now,
        limit
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// 窗口前滚到覆盖 `now` 的那一格（worker 停过就连跳，不补发错过的窗）。
#[must_use]
pub fn advance_window(
    window_end: DateTime<Utc>,
    period: Period,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let mut start = window_end;
    let mut end = period.next(start);
    // 上限防御：一次最多跳 10 年的日窗，避免坏数据造成长循环
    let mut guard = 0;
    while end <= now && guard < 4000 {
        start = end;
        end = period.next(start);
        guard += 1;
    }
    (start, end)
}

/// 写回新窗口。
pub async fn roll_window(
    pool: &PgPool,
    subscription_id: i64,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<(), StoreError> {
    sqlx::query!(
        r#"UPDATE user_subscriptions SET window_start = $2, window_end = $3, updated_at = now()
           WHERE id = $1 AND status = 1"#,
        subscription_id,
        window_start,
        window_end
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn month_period_uses_calendar_months() {
        let jan31 = Utc
            .with_ymd_and_hms(2026, 1, 31, 10, 0, 0)
            .single()
            .unwrap_or_default();
        let next = Period::Month.next(jan31);
        assert_eq!(
            next,
            Utc.with_ymd_and_hms(2026, 2, 28, 10, 0, 0)
                .single()
                .unwrap_or_default()
        );
    }

    #[test]
    fn advance_skips_missed_windows() {
        let start = Utc
            .with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
            .single()
            .unwrap_or_default();
        let end = Period::Day.next(start);
        // worker 停了三天半：一次跳到覆盖 now 的那一格
        let now = start + Duration::hours(24 * 3 + 12);
        let (ws, we) = advance_window(end, Period::Day, now);
        assert_eq!(ws, start + Duration::days(3));
        assert_eq!(we, start + Duration::days(4));
    }

    #[test]
    fn advance_one_step_when_on_time() {
        let start = Utc
            .with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
            .single()
            .unwrap_or_default();
        let end = Period::Week.next(start);
        let (ws, we) = advance_window(end, Period::Week, end + Duration::seconds(30));
        assert_eq!(ws, end);
        assert_eq!(we, end + Duration::weeks(1));
    }
}
