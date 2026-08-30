-- 套餐×兑换码增强（IMPLEMENTATION §11 #1790-5 / #2845 / #3388，M4）：
-- plans = 可复用套餐模板；兑换码可绑套餐（核销金额取套餐 grant 覆盖面值，
-- 并可附带加组 / 设置余额有效期）；bind_user_id 限定核销人；
-- max_per_ip 同批次单 IP 核销上限（Redis 计数，依赖 CDN 头取 IP，直连无头不限）。
CREATE TABLE IF NOT EXISTS plans (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plan_code          VARCHAR(64) NOT NULL UNIQUE,
    display_name       VARCHAR(128) NOT NULL,
    grant_micro        BIGINT NOT NULL CHECK (grant_micro > 0),
    group_code         VARCHAR(64),                       -- 兑换后追加分组（NULL=不改组）
    balance_valid_days INT CHECK (balance_valid_days > 0),-- 兑换后设置余额有效期（NULL=不设置）
    status             SMALLINT NOT NULL DEFAULT 1,       -- 1=启用 2=停用
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE redemption_codes
    ADD COLUMN IF NOT EXISTS plan_id BIGINT REFERENCES plans(id),
    ADD COLUMN IF NOT EXISTS bind_user_id BIGINT REFERENCES users(id),
    ADD COLUMN IF NOT EXISTS max_per_ip INT CHECK (max_per_ip > 0);
