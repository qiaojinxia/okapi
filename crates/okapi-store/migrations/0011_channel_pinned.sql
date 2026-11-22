-- 渠道列表置顶支持：pinned_at 非空即置顶，按置顶时间倒序排在最前，不影响网关路由调度优先级。
ALTER TABLE channels
    ADD COLUMN IF NOT EXISTS pinned_at timestamptz;

COMMENT ON COLUMN channels.pinned_at IS '管理端列表置顶时间；非空时在渠道列表置顶展示（按置顶时间倒序），不影响路由调度优先级';

CREATE INDEX IF NOT EXISTS idx_channels_pinned_sort
    ON channels ((pinned_at IS NOT NULL) DESC, pinned_at DESC, priority DESC, id)
    WHERE deleted_at IS NULL;
