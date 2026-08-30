-- 管理操作审计（docs/database.md §1.7）：console 写路径全量留痕

CREATE TABLE audit_logs (
    id         BIGINT GENERATED ALWAYS AS IDENTITY,
    actor      VARCHAR(64) NOT NULL,                 -- admin:{id} / mcp:{key_id} / system
    action     VARCHAR(64) NOT NULL,                 -- channel.create / pricing.publish / user.credit ...
    target     VARCHAR(128),
    detail     JSONB,
    ip         INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
CREATE TABLE audit_logs_default PARTITION OF audit_logs DEFAULT;
CREATE INDEX idx_audit_actor_time ON audit_logs (actor, created_at DESC);
