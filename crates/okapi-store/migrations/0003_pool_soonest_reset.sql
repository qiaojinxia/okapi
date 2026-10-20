-- 渠道池选路策略 soonest_reset（IMPLEMENTATION §11.14）：订阅账号层内按最近一次额度窗口重置升序，
-- 先用快要重置的额度；没有额度观测的 key 按本层中位数排。
ALTER TABLE channel_pools DROP CONSTRAINT channel_pools_strategy_chk;
ALTER TABLE channel_pools ADD CONSTRAINT channel_pools_strategy_chk
    CHECK (routing_strategy IN ('priority_weighted', 'least_latency', 'least_usage', 'least_concurrency', 'soonest_reset'));
COMMENT ON COLUMN channel_pools.routing_strategy IS 'priority_weighted：priority 分层 + 层内成本修正加权随机（订阅账号再按额度余量分三档）；least_latency：层内按 Redis lat:ck:* 的 EWMA 升序，无数据者按中位数处理；least_usage：层内按 spend:ck:* 当日消费 ÷ 权重升序；least_concurrency：层内按 conc:ck:* 在途请求数 ÷ 权重升序；soonest_reset：层内按额度窗口最近一次重置升序；读数相同按权重随机';
