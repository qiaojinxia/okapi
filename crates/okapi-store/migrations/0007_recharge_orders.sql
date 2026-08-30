-- 支付闭环（IMPLEMENTATION §11.2/§13-M4）：recharge_orders 按 docs/database.md §1.6 落地；
-- 同时把 redemption_codes 对齐文档定案（code_hash 明文不落库）。
CREATE TABLE recharge_orders (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_no         VARCHAR(64) NOT NULL UNIQUE,
    user_id          BIGINT NOT NULL REFERENCES users(id),
    amount_micro     BIGINT NOT NULL CHECK (amount_micro > 0),
    currency         VARCHAR(8) NOT NULL DEFAULT 'USD',
    pay_amount       NUMERIC(12,2),                   -- 支付金额（原币种，仅展示）
    gateway          VARCHAR(32) NOT NULL,            -- stripe / epay / manual ...
    gateway_trade_no VARCHAR(128),
    status           SMALLINT NOT NULL DEFAULT 0,     -- 0 created 1 paid 2 failed 3 refunded
    paid_at          TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_recharge_user ON recharge_orders (user_id, created_at DESC);

-- 兑换码明文不落库（docs §1.6 定案）：列改存 SHA-256 hex
ALTER TABLE redemption_codes RENAME COLUMN code TO code_hash;
