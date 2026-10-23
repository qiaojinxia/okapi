-- 评审 2026-10-23（迁移 / schema 审计）。

-- 1) 保留策略对 DEFAULT 分区逐批清理：`WHERE created_at < $1 ORDER BY created_at, <主键列> LIMIT 1000`。
--    首月的数据全落在 DEFAULT 分区（月分区只提前建下个月的），而父表上的索引都不以 created_at 打头，
--    所以每一批都是整表扫描 + 排序；数据量一大，批批撞 10 秒语句超时，这批数据就永远清不动。
--    只在 DEFAULT 分区上建（不是父表）：月分区的写入路径不为它多维护一个索引，DEFAULT 分区
--    首月之后几乎不再写入。CREATE INDEX 会短暂挡住对该分区的写入，建在现存行上很快；
--    表很大的站点想避开，可先在库里手工 CREATE INDEX CONCURRENTLY 同名索引，这里的 IF NOT EXISTS 会跳过。
CREATE INDEX IF NOT EXISTS billing_records_default_created ON billing_records_default (created_at, id);
CREATE INDEX IF NOT EXISTS billing_events_default_created ON billing_events_default (created_at, event_id);
CREATE INDEX IF NOT EXISTS audit_logs_default_created ON audit_logs_default (created_at, id);

-- 2) 与 0004 同族的值域守卫（NOT VALID：只约束之后的新写入，不因历史脏行让迁移失败）。
--    未知的 override_kind 会让价簿加载器悄悄丢掉这条用户专属价（该用户改按模型价计费）；
--    tier_ratios 不是对象则这个模型的档位表编译失败。
ALTER TABLE user_pricing ADD CONSTRAINT user_pricing_override_kind_known
    CHECK (override_kind IN ('ratio', 'absolute')) NOT VALID;
ALTER TABLE model_pricing ADD CONSTRAINT model_pricing_tier_ratios_object
    CHECK (tier_ratios IS NULL OR jsonb_typeof(tier_ratios) = 'object') NOT VALID;

-- 3) users.aff_code 已有 UNIQUE 约束（users_aff_code_key，NULL 本来就互不冲突），WHERE aff_code IS NOT NULL
--    的部分唯一索引是完全重复的一份，每次写 users 白白多维护一个索引。
DROP INDEX IF EXISTS idx_users_aff_code;
