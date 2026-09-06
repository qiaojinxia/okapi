-- sub_set：订阅池置额（激活 / 滚窗 / 到期同一脚本；IMPLEMENTATION §11.28）
-- KEYS[1] bal:{uid}
-- ARGV[1] quota_micro    —— 新窗额度（到期传 0）
-- ARGV[2] sub_until_unix —— 池可用截止（= min(window_end, expires_at)；到期传 0）
-- 返回 {prev_sub, new_sub}（字符串；调用方据 new - prev 记 pool=1 事件）
--
-- new_sub = quota + Σ在途(pool=1)：老窗口尚未结算的请求在 commit 时会按各自预扣冲回
-- 同一个池（reserved - actual），把它们的预扣额加回来，新窗才不会被老请求的结算吃掉；
-- 不变式 `sub + Σ在途₁ == Σ events(pool=1)` 在事件按 new - prev 记录后仍然成立。
-- 钱包字段与钱包在途一律不碰。

local quota = tonumber(ARGV[1])
local inflight = 0
local all = redis.call('HGETALL', KEYS[1])
for i = 1, #all, 2 do
    if string.sub(all[i], 1, 2) == 'r:' then
        local parts = {}
        for p in string.gmatch(all[i + 1], '[^|]+') do parts[#parts + 1] = p end
        if (parts[4] or '0') == '1' then
            inflight = inflight + (tonumber(parts[1]) or 0)
        end
    end
end

local prev = tonumber(redis.call('HGET', KEYS[1], 'sub') or '0')
local next_sub = quota + inflight
redis.call('HSET', KEYS[1], 'sub', next_sub)
redis.call('HSET', KEYS[1], 'sub_until', ARGV[2])
return {tostring(prev), tostring(next_sub)}
