-- 兑换码（IMPLEMENTATION §13 M4「套餐×兑换码」基础）：
-- 面值一次性核销；核销 = 行级原子状态翻转 + credit 事件（actor system:redeem）。
CREATE TABLE redemption_codes (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    code         VARCHAR(64) NOT NULL UNIQUE,
    amount_micro BIGINT NOT NULL CHECK (amount_micro > 0),
    status       SMALLINT NOT NULL DEFAULT 1,          -- 1=未用 2=已用 3=停用
    batch_id     UUID NOT NULL,                        -- 同批生成溯源
    created_by   BIGINT REFERENCES users(id),
    redeemed_by  BIGINT REFERENCES users(id),
    redeemed_at  TIMESTAMPTZ,
    expires_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_redemption_batch ON redemption_codes(batch_id);
