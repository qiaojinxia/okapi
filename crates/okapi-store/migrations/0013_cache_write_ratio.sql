-- 缓存写入倍率（DESIGN §3.2 prompt 四段计费）。
--
-- 背景：Anthropic prompt caching 的缓存**写入**（cache_creation_input_tokens）官方定价为
-- 基础输入价的 1.25×（5m TTL）/ 2.0×（1h TTL），与缓存**读取**（0.1×）方向相反。
-- 此前只有单一 cache_ratio，写入段被混入常规输入按 1.0× 计费 → 每笔缓存写入漏收约 20%。
--
-- 缺省 1.0 = 保持旧行为（按常规输入计），不改变既有站点账单；管理端按模型配置后生效。
-- 对齐生态：new-api 同轴名为 create_cache_ratio（ratio JSON 导入时做键名映射）。

ALTER TABLE model_pricing
    ADD COLUMN IF NOT EXISTS cache_write_ratio NUMERIC(6,4) NOT NULL DEFAULT 1;

ALTER TABLE user_pricing
    ADD COLUMN IF NOT EXISTS custom_cache_write_ratio NUMERIC(6,4);

COMMENT ON COLUMN model_pricing.cache_write_ratio IS
    '缓存写入倍率（Anthropic cache_creation；1.0=按常规输入计，官方 1.25×@5m / 2.0×@1h）';
COMMENT ON COLUMN user_pricing.custom_cache_write_ratio IS
    '用户专属缓存写入倍率覆盖；NULL = 用模型级值';
