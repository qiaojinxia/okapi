-- 倍率列统一到 1e-6 定点精度，与 model_pricing 各倍率、RatioFp 解析一致。
-- 此前这几列只有 4 位小数：管理端按 6 位小数解析后写入会被 PG 静默四舍五入，
-- 小于 0.00005 的值直接存成 0（等于该分组 / 用户 / 缓存轴免费），
-- 分组倍率与缓存倍率覆盖写到 100 以上还会溢出报错。
ALTER TABLE users ALTER COLUMN price_multiplier TYPE numeric(12,6);
ALTER TABLE user_pricing
    ALTER COLUMN custom_cache_ratio TYPE numeric(12,6),
    ALTER COLUMN custom_cache_write_ratio TYPE numeric(12,6);
ALTER TABLE price_groups ALTER COLUMN group_ratio TYPE numeric(12,6);
