-- sub_set：订阅池置额（激活 / 滚窗 / 到期同一脚本；IMPLEMENTATION §11.28）
-- KEYS[1] bal:{uid}
-- ARGV[1] quota_micro    —— 新窗额度（到期传 0）
-- ARGV[2] sub_until_unix —— 池可用截止（= min(window_end, expires_at)；到期传 0）
-- ARGV[3] sub_epoch —— subscription id/window-start；续期不改变它。
-- 返回 {prev_sub, new_sub}（字符串；调用方据 new - prev 记 pool=1 事件）
--
-- new_sub = quota + Σ在途(pool=1)：老窗口尚未结算的请求在 commit 时会按各自预扣冲回
-- 同一个池（reserved - actual），把它们的预扣额加回来，新窗才不会被老请求的结算吃掉；
-- 不变式 `sub + Σ在途₁ == Σ events(pool=1)` 在事件按 new - prev 记录后仍然成立。
-- 钱包字段与钱包在途一律不碰。

local quota = ledger_integer(ARGV[1], false)
local until_at = ledger_integer(ARGV[2], false)
if not quota or not until_at then return redis.error_reply('invalid_subscription_amount') end
local inflight = 0
local all = redis.call('HGETALL', KEYS[1])
for i = 1, #all, 2 do
    if string.sub(all[i], 1, 2) == 'r:' then
        local receipt = ledger_reservation(all[i + 1])
        if not receipt then return redis.error_reply('invalid_subscription_reservation') end
        if receipt.pool == 1 then
            inflight = ledger_add(inflight, receipt.amount)
            if not inflight then return redis.error_reply('invalid_subscription_amount') end
        end
    end
end

local prev = ledger_integer(redis.call('HGET', KEYS[1], 'sub') or '0', true)
local next_sub = ledger_add(quota, inflight)
if not prev or not next_sub then return redis.error_reply('invalid_subscription_amount') end
redis.call('HSET', KEYS[1], 'sub', ledger_decimal(next_sub))
redis.call('HSET', KEYS[1], 'sub_until', ARGV[2])
redis.call('HSET', KEYS[1], 'sub_epoch', ARGV[3] or '')
return {ledger_decimal(prev), ledger_decimal(next_sub)}
