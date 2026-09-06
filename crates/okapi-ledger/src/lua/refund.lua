-- refund：全额释放预扣（上游失败/空回复不计费路径）+ 释放并发
-- KEYS[1] bal:{uid}  KEYS[2] conc:{uid}:k:<kid>
-- ARGV[1] request_id
-- 幂等：重复调用返回 {1,'0',avail,0}
-- 回到预扣所在池（字段第 4 段 pool；老格式缺省钱包）。

local field = 'r:' .. ARGV[1]
local r = redis.call('HGET', KEYS[1], field)
if not r then
    return {1, '0', redis.call('HGET', KEYS[1], 'avail') or '0', 0}
end

local parts = {}
for p in string.gmatch(r, '[^|]+') do parts[#parts + 1] = p end
local reserved = tonumber(parts[1])
local pool = (parts[4] == '1') and 1 or 0
local bal_field = (pool == 1) and 'sub' or 'avail'

redis.call('HINCRBY', KEYS[1], bal_field, reserved)
redis.call('HDEL', KEYS[1], field)
if tonumber(redis.call('GET', KEYS[2]) or '0') > 0 then redis.call('DECR', KEYS[2]) end
return {1, tostring(reserved), redis.call('HGET', KEYS[1], bal_field), pool}
