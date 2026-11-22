-- 渠道池调度策略中心（Phase 1）：
-- 1) channel_pools 新增 policy jsonb（多跳降级、选路/粘性/容灾/护栏/灰度/条件覆写规则）与 policy_version；
-- 2) pool_channels 新增成员角色（primary / backup / canary）、池内启停开关、标签 tags 与池内模型过滤 model_filter。
ALTER TABLE channel_pools
    ADD COLUMN IF NOT EXISTS policy jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS policy_version bigint NOT NULL DEFAULT 1;

COMMENT ON COLUMN channel_pools.policy IS '渠道池调度策略文档（PoolPolicy JSONB：多跳降级、选路、粘性、容灾、护栏、灰度与条件覆写规则）';
COMMENT ON COLUMN channel_pools.policy_version IS '渠道池策略单调版本号（每次更新递增，用于缓存核对与审计回放）';

ALTER TABLE pool_channels
    ADD COLUMN IF NOT EXISTS role varchar(16) NOT NULL DEFAULT 'primary'
        CHECK (role IN ('primary', 'backup', 'canary')),
    ADD COLUMN IF NOT EXISTS enabled boolean NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS tags jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS model_filter jsonb;

COMMENT ON COLUMN pool_channels.role IS '成员在本池内的角色：primary 主力 / backup 备胎 / canary 灰度';
COMMENT ON COLUMN pool_channels.enabled IS '成员在本池内的启停开关（false 仅在本池摘流，不影响该渠道在其他池服务）';
COMMENT ON COLUMN pool_channels.tags IS '成员在本池内的标签数组（供策略规则 required_tags/excluded_tags 分流）';
COMMENT ON COLUMN pool_channels.model_filter IS '成员在本池内的模型通配白名单（null = 继承渠道全部模型）';
