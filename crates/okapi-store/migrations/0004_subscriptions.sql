-- 订阅套餐：周期配额池（IMPLEMENTATION §11.28，docs/database.md §1.6 / §2.2）。
--
-- 不新增 subscription_plans：plans 已经是"套餐"，加 kind 区分充值模板与订阅；
-- user_subscriptions 兑现 §1.8 预留。池余额不落 PG（热值 Redis bal:{uid}.sub，
-- 权威值 = Σ billing_events WHERE pool = 1），故 billing_events / billing_records 加 pool 列。

ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS kind          SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS price_micro   BIGINT   NOT NULL DEFAULT 0 CHECK (price_micro >= 0),
    ADD COLUMN IF NOT EXISTS period        SMALLINT,
    ADD COLUMN IF NOT EXISTS duration_days INT      CHECK (duration_days > 0),
    ADD COLUMN IF NOT EXISTS sort_order    INT      NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS description   TEXT;
ALTER TABLE plans
    ADD CONSTRAINT plans_subscription_shape
    CHECK (kind = 0 OR (period IN (1, 2, 3) AND duration_days IS NOT NULL));

CREATE TABLE IF NOT EXISTS user_subscriptions (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id        BIGINT NOT NULL REFERENCES users(id),
    plan_id        BIGINT NOT NULL REFERENCES plans(id),
    status         SMALLINT NOT NULL DEFAULT 1,       -- 1 active 2 expired 3 cancelled
    starts_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at     TIMESTAMPTZ NOT NULL,
    window_start   TIMESTAMPTZ NOT NULL,
    window_end     TIMESTAMPTZ NOT NULL,
    quota_micro    BIGINT NOT NULL,
    granted_group  BOOLEAN NOT NULL DEFAULT false,
    source         VARCHAR(96) NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_user_sub_active ON user_subscriptions (user_id) WHERE status = 1;
CREATE INDEX IF NOT EXISTS idx_user_sub_window ON user_subscriptions (window_end) WHERE status = 1;

ALTER TABLE recharge_orders ADD COLUMN IF NOT EXISTS plan_id BIGINT REFERENCES plans(id);

-- 分区表加列：父表 ALTER 自动传播到全部分区
ALTER TABLE billing_events  ADD COLUMN IF NOT EXISTS pool SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE billing_records ADD COLUMN IF NOT EXISTS pool SMALLINT NOT NULL DEFAULT 0;
