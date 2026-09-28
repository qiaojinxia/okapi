-- sub_set：旧低层订阅池置额；生产激活 / 滚窗 / 到期使用 PG + UserGuard 恢复。
-- KEYS[1] bal:{uid}
-- ARGV[1] quota_micro    —— 新窗额度（到期传 0）
-- ARGV[2] sub_until_unix —— 池可用截止（= min(window_end, expires_at)；到期传 0）
-- ARGV[3] sub_epoch —— subscription id/window-start；续期不改变它。
-- 返回 {prev_sub, new_sub}（字符串；调用方据 new - prev 记 pool=1 事件）
--
-- new_sub = quota + Σ无周期身份的旧在途(pool=1)，仅保留低层兼容行为。
-- 旧热账本 epoch 已知时先补到旧凭据；有身份的普通预扣不带入新窗。
-- 生产订阅流程对仍无身份的凭据返回待恢复错误，不能依赖此脚本推断归属。
-- 钱包字段与钱包在途一律不碰；长期 h:* 使用独立 PG 冻结契约。

local quota = ledger_integer(ARGV[1], false)
local until_at = ledger_integer(ARGV[2], false)
if not quota or not until_at then return redis.error_reply('invalid_subscription_amount') end
local epoch = ARGV[3] or ''
local old_epoch = redis.call('HGET', KEYS[1], 'sub_epoch') or ''
if (epoch ~= '' and not ledger_epoch(epoch)) or (old_epoch ~= '' and not ledger_epoch(old_epoch)) then
    return redis.error_reply('invalid_subscription_amount')
end
local updates = {}
local inflight = 0
local all = redis.call('HGETALL', KEYS[1])
for i = 1, #all, 2 do
    if string.sub(all[i], 1, 2) == 'r:' then
        local receipt = ledger_reservation(all[i + 1])
        if not receipt then return redis.error_reply('invalid_subscription_reservation') end
        if receipt.pool == 1 and not receipt.epoch and old_epoch ~= '' then
            receipt.epoch = old_epoch
            updates[#updates+1] = {field=all[i],value=all[i+1] .. '|w:' .. old_epoch}
        end
        if receipt.pool == 1 and not receipt.epoch then
            inflight = ledger_add(inflight, receipt.amount)
            if not inflight then return redis.error_reply('invalid_subscription_amount') end
        end
    end
end

local prev = ledger_integer(redis.call('HGET', KEYS[1], 'sub') or '0', true)
local next_sub = ledger_add(quota, inflight)
if not prev or not next_sub then return redis.error_reply('invalid_subscription_amount') end
for _,item in ipairs(updates) do redis.call('HSET', KEYS[1], item.field, item.value) end
redis.call('HSET', KEYS[1], 'sub', ledger_decimal(next_sub))
redis.call('HSET', KEYS[1], 'sub_until', ARGV[2])
redis.call('HSET', KEYS[1], 'sub_epoch', ARGV[3] or '')
return {ledger_decimal(prev), ledger_decimal(next_sub)}
