-- 渠道池选路策略 expiry_urgency：多窗口过期紧迫度调度（综合 5h 短窗口与 7d 周窗口的剩余额度/剩余时间，
-- 优先清仓即将过期作废的大块订阅额度，同时按请求上下文大小保护快见底账号不接大冷启动请求）。
ALTER TABLE channel_pools DROP CONSTRAINT channel_pools_strategy_chk;
ALTER TABLE channel_pools ADD CONSTRAINT channel_pools_strategy_chk
    CHECK (routing_strategy IN (
        'priority_weighted',
        'least_latency',
        'least_usage',
        'least_concurrency',
        'soonest_reset',
        'expiry_urgency'
    ));
COMMENT ON COLUMN channel_pools.routing_strategy IS 'priority_weighted：priority 分层 + 层内成本修正加权随机（订阅账号再按额度余量分三档）；least_latency：层内按 Redis lat:ck:* 的 EWMA 升序，无数据者按中位数处理；least_usage：层内按 spend:ck:* 当日消费 ÷ 权重升序；least_concurrency：层内按 conc:ck:* 在途请求数 ÷ 权重升序；soonest_reset：层内按额度窗口最近一次重置升序；expiry_urgency：多窗口（5h/7d）过期紧迫度综合清仓优先 + 长短任务匹配';
