-- repair：按账本权威值重建热余额（对账修复；docs/database.md §2.2）
-- KEYS[1] bal:{uid}
-- ARGV[1] target_micro —— billing_events 求和，事件流是权威源
-- 返回 {旧 avail, 新 avail, 在途合计}（一律字符串，避免 Lua double 截断大额）
--
-- 在途预扣**不动**：它们各自会按结算/退款路径终结，现在抹掉会让那些请求
-- 结算时凭空多扣或少扣。所以设 avail = target - Σ在途，保证对账不变式
-- `avail + 在途 == 账本` 成立；扫描与写入同一脚本内完成，与并发 reserve 原子互斥。
--
-- 不夹逼到 0：账本为负说明这个用户确实欠着（退款冲销多于充值），
-- 夹成 0 等于凭空送钱；负 avail 会让 reserve 一直判 INSUFFICIENT，正是想要的语义。

local target = tonumber(ARGV[1])
local inflight = 0
local all = redis.call('HGETALL', KEYS[1])
for i = 1, #all, 2 do
    if string.sub(all[i], 1, 2) == 'r:' then
        inflight = inflight + tonumber(string.match(all[i + 1], '^(%d+)') or '0')
    end
end

local prev = tonumber(redis.call('HGET', KEYS[1], 'avail') or '0')
local next_avail = target - inflight
redis.call('HSET', KEYS[1], 'avail', next_avail)
return {tostring(prev), tostring(next_avail), tostring(inflight)}
