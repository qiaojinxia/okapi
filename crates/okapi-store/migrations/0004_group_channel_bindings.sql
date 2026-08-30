-- 渠道可见性矩阵（docs/database.md §1.2，#6977）：
-- settings.strict_group_isolation=true 时未绑定即不可见；false 时绑定为空 = 全可见

CREATE TABLE group_channel_bindings (
    group_code VARCHAR(32) NOT NULL REFERENCES price_groups(group_code),
    channel_id BIGINT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    PRIMARY KEY (group_code, channel_id)
);
CREATE INDEX idx_gcb_channel ON group_channel_bindings (channel_id);
