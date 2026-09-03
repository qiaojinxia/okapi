-- 渠道池语义统一（IMPLEMENTATION §11.14）：
--   一条规则——渠道只服务它所在的池；分组必有池；未入任何池的渠道 = 孤儿（不可达）。
--   替代此前"有池只看池内 / 无池只看未入池 / strict 开关"三态：那套规则让 vip 组用户
--   看不到任何公共渠道（比 default 组能用的还少），且 UI 文案与之相反。
-- 迁移保持既有行为：未入池渠道并入 default 池、无池分组指向 default 池，
-- 原来"无池用户能打未入池渠道"在新规则下等价成立。
-- （发布前压平回 0001；当前无生产部署，见 §11.10）

-- 1. 内置默认池
INSERT INTO channel_pools (pool_code, description, routing_strategy)
VALUES ('default', '内置默认池：新渠道缺省加入，未指定池的分组走这里', 'priority_weighted')
ON CONFLICT (pool_code) DO NOTHING;

-- 2. 未入池渠道 → default 池（含软删渠道，保持"恢复即可达"）
INSERT INTO pool_channels (pool_code, channel_id)
SELECT 'default', c.id
FROM channels c
WHERE NOT EXISTS (SELECT 1 FROM pool_channels pc WHERE pc.channel_id = c.id)
ON CONFLICT DO NOTHING;

-- 3. 分组必有池
UPDATE price_groups SET pool_code = 'default' WHERE pool_code IS NULL;
ALTER TABLE price_groups
    ALTER COLUMN pool_code SET DEFAULT 'default',
    ALTER COLUMN pool_code SET NOT NULL;
COMMENT ON COLUMN price_groups.pool_code IS
    '该分组的用户打哪个池；缺省 default。池的 fallback_pool_code 再决定池内无候选时退到哪';

-- 4. 池级降级（同模型换池；模型级降级见 models.fallback_models，两者正交）
ALTER TABLE channel_pools
    ADD COLUMN fallback_pool_code VARCHAR(32) REFERENCES channel_pools(pool_code),
    ADD CONSTRAINT channel_pools_fallback_not_self
        CHECK (fallback_pool_code IS NULL OR fallback_pool_code <> pool_code);
COMMENT ON COLUMN channel_pools.fallback_pool_code IS
    '本池对某模型无可用候选时退到的池（单跳，不递归）；计费仍按请求者的分组倍率';

-- 5. 成员级调度参数覆盖（NULL = 继承 channels.priority / channel_keys.weight）
ALTER TABLE pool_channels
    ADD COLUMN priority_override INT,
    ADD COLUMN weight_override   INT;
COMMENT ON COLUMN pool_channels.priority_override IS
    '同一渠道在不同池里可以是主力也可以是备胎：本池内的优先级覆盖，NULL = 用渠道自身 priority';
COMMENT ON COLUMN pool_channels.weight_override IS
    '本池内的抽样权重覆盖（作用于该渠道全部 key），NULL = 用各 key 自身 weight';

-- 6. 用户可自选的档位（new-api UserUsableGroups 的对应物）
ALTER TABLE price_groups
    ADD COLUMN self_select BOOLEAN NOT NULL DEFAULT false;
COMMENT ON COLUMN price_groups.self_select IS
    '用户可在门户为自己的 key 选择此分组（价随组走）；false = 仅管理员可分配';

-- 7. strict_group_isolation 退役：孤儿渠道天然不可达，两态开关无存在必要
DELETE FROM settings WHERE key = 'strict_group_isolation';
