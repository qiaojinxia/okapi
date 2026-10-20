-- 渠道池新增两种选路策略（IMPLEMENTATION §11.14）：
--   least_usage       层内按 key 当日消费 ÷ 权重升序（Redis spend:ck:*）
--   least_concurrency 层内按 key 在途请求数 ÷ 权重升序（Redis conc:ck:*）
-- 两者都不推翻 priority 分层与主池 / 降级池的先后；读数相同时按权重随机。
ALTER TABLE channel_pools DROP CONSTRAINT channel_pools_strategy_chk;
ALTER TABLE channel_pools ADD CONSTRAINT channel_pools_strategy_chk
    CHECK (routing_strategy IN ('priority_weighted', 'least_latency', 'least_usage', 'least_concurrency'));
COMMENT ON COLUMN channel_pools.routing_strategy IS 'priority_weighted：priority 分层 + 层内成本修正加权随机（历史行为）；least_latency：层内按 Redis lat:ck:* 的 EWMA 升序，无数据者按中位数处理；least_usage：层内按 spend:ck:* 当日消费 ÷ 权重升序；least_concurrency：层内按 conc:ck:* 在途请求数 ÷ 权重升序；读数相同按权重随机';
