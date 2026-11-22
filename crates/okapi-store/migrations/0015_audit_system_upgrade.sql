-- 审计系统增强：补齐来源上下文列（user_agent / trace_id / actor_key_id）与常用分区索引。
-- 对分区父表 audit_logs 的 ALTER TABLE / CREATE INDEX 会自动传播到全部现存及未来月分区。

ALTER TABLE audit_logs
    ADD COLUMN IF NOT EXISTS user_agent varchar(256),
    ADD COLUMN IF NOT EXISTS trace_id varchar(64),
    ADD COLUMN IF NOT EXISTS actor_key_id bigint;

-- 默认列表倒序与游标翻页
CREATE INDEX IF NOT EXISTS idx_audit_created_id
    ON audit_logs (created_at DESC, id DESC);

-- 按动作前缀 / 动作名检索（text_pattern_ops 支持 LIKE 'prefix%' 走 B-Tree 范围扫描）
CREATE INDEX IF NOT EXISTS idx_audit_action_time
    ON audit_logs (action text_pattern_ops, created_at DESC);

-- 按对象（渠道 ID / 用户 ID / 邮箱 / 设置键）检索
CREATE INDEX IF NOT EXISTS idx_audit_target_time
    ON audit_logs (target, created_at DESC)
    WHERE target IS NOT NULL;

-- 按来源 IP 检索
CREATE INDEX IF NOT EXISTS idx_audit_ip_time
    ON audit_logs (ip, created_at DESC)
    WHERE ip IS NOT NULL;

-- 按链路 trace_id 关联检索
CREATE INDEX IF NOT EXISTS idx_audit_trace_id
    ON audit_logs (trace_id)
    WHERE trace_id IS NOT NULL;

-- 按凭证 API key ID 检索（密钥泄露事后追溯）
CREATE INDEX IF NOT EXISTS idx_audit_actor_key_time
    ON audit_logs (actor_key_id, created_at DESC)
    WHERE actor_key_id IS NOT NULL;
