-- refund：全额释放预扣（上游失败/空回复不计费路径）+ 释放并发
-- KEYS[1] bal:{uid}  KEYS[2] conc:{uid}:k:<kid>
-- ARGV[1] request_id
-- 幂等：重复调用返回 {1,'0',balance}

local field = 'r:' .. ARGV[1]
local r = redis.call('HGET', KEYS[1], field)
if not r then
    return {1, '0', redis.call('HGET', KEYS[1], 'avail') or '0'}
end

local reserved = tonumber(string.match(r, '^(%d+)'))
redis.call('HINCRBY', KEYS[1], 'avail', reserved)
redis.call('HDEL', KEYS[1], field)
if tonumber(redis.call('GET', KEYS[2]) or '0') > 0 then redis.call('DECR', KEYS[2]) end
return {1, tostring(reserved), redis.call('HGET', KEYS[1], 'avail')}
