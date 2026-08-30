-- 余额有效期（IMPLEMENTATION §11 #1790-6，M4）：
-- NULL = 永不过期；到期由 worker 清零（事件 event_type='expire'，actor=system:worker）
-- 并重置为 NULL（幂等防重扫）。充值不自动延期（延期策略列 backlog）。
ALTER TABLE users
    ADD COLUMN IF NOT EXISTS balance_expires_at TIMESTAMPTZ;
