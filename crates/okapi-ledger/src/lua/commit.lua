-- commit：结算（多退少补）+ 释放并发（docs/database.md §2.2）
-- KEYS[1] bal:{uid}  KEYS[2] conc:{uid}:k:<kid>
-- ARGV[1] request_id  ARGV[2] actual_micro
-- 幂等：预扣字段不存在 → {0,'NO_RESERVATION'}（调用方转对账路径，不直接改余额）

local field = 'r:' .. ARGV[1]
local r = redis.call('HGET', KEYS[1], field)
if not r then return {0, 'NO_RESERVATION'} end

local reserved = tonumber(string.match(r, '^(%d+)'))
local actual = tonumber(ARGV[2])
local delta = reserved - actual

redis.call('HINCRBY', KEYS[1], 'avail', delta)
redis.call('HDEL', KEYS[1], field)
if tonumber(redis.call('GET', KEYS[2]) or '0') > 0 then redis.call('DECR', KEYS[2]) end
return {1, tostring(delta), redis.call('HGET', KEYS[1], 'avail')}
