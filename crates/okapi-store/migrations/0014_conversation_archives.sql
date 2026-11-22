-- 对话存档表（ClickHouse 未启用时作为主存储；启用 ClickHouse 时仅作写入失败兜底与历史兼容查询）。
-- 保留策略由 settings.conversation_archive（retention_days=0 永久，>0 定时批量清理）控制。
CREATE TABLE IF NOT EXISTS conversation_archives (
    request_id      UUID        PRIMARY KEY,
    user_id         BIGINT      NOT NULL,
    api_key_id      BIGINT      NOT NULL,
    channel_id      BIGINT      NOT NULL DEFAULT 0,
    model           TEXT        NOT NULL DEFAULT '',
    requested_model TEXT        NOT NULL DEFAULT '',
    endpoint        TEXT        NOT NULL DEFAULT '',
    is_stream       BOOLEAN     NOT NULL DEFAULT FALSE,
    status_code     SMALLINT    NOT NULL DEFAULT 200,
    error_code      TEXT        NOT NULL DEFAULT '',
    request_body    TEXT        NOT NULL DEFAULT '',
    response_body   TEXT        NOT NULL DEFAULT '',
    response_text   TEXT        NOT NULL DEFAULT '',
    reasoning_text  TEXT        NOT NULL DEFAULT '',
    tool_calls      TEXT        NOT NULL DEFAULT '',
    truncated       BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_conversation_archives_created
    ON conversation_archives (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_conversation_archives_user_created
    ON conversation_archives (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_conversation_archives_model_created
    ON conversation_archives (model, created_at DESC);
