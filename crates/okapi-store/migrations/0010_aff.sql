-- 邀请返利 aff（IMPLEMENTATION §13 M4）：
-- aff_code 惰性生成（门户首查时）；inviter_id 注册时绑定终身不变；
-- 返利仅在充值核销时触发（比例 settings.aff_percent_bp 基点，缺省 0=关闭），
-- 兑换码核销不返利（防套利）。事件 actor='system:aff'。
ALTER TABLE users
    ADD COLUMN IF NOT EXISTS aff_code VARCHAR(16),
    ADD COLUMN IF NOT EXISTS inviter_id BIGINT REFERENCES users(id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_aff_code ON users (aff_code) WHERE aff_code IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_users_inviter ON users (inviter_id) WHERE inviter_id IS NOT NULL;
