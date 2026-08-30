-- reserve：余额预扣 + key 级限速/并发准入（docs/database.md §2.2）
-- KEYS[1] bal:{uid}
-- KEYS[2] rl:{uid}:k:<kid>:rpm:<bucket>  KEYS[3] rl:{uid}:k:<kid>:tpm:<bucket>
-- KEYS[4] rl:{uid}:k:<kid>:rpd:<day>     KEYS[5] conc:{uid}:k:<kid>
-- ARGV[1] request_id  ARGV[2] est_micro  ARGV[3] deadline_ms
-- ARGV[4] rpm_cap  ARGV[5] tpm_cap  ARGV[6] rpd_cap  ARGV[7] conc_cap
-- ARGV[8] est_tokens  ARGV[9] api_key_id
-- 全部 KEYS 同 {uid} hash-tag（Cluster 单槽原子）。cap<=0 = 不限。
-- 预扣字段值 = "<est_micro>|<deadline_ms>|<api_key_id>"（释放时按 kid 归还并发槽）。
-- 精度注：Lua number 为 double，余额比较精度上限 2^53 micro ≈ $90 亿，超出视为配置错误。

local function over_cap(key, cap, incr)
    if cap <= 0 then return false end
    local cur = tonumber(redis.call('GET', key) or '0')
    return (cur + incr) > cap
end

local rpm_cap = tonumber(ARGV[4])
local tpm_cap = tonumber(ARGV[5])
local rpd_cap = tonumber(ARGV[6])
local conc_cap = tonumber(ARGV[7])
local est_tokens = tonumber(ARGV[8])

if over_cap(KEYS[2], rpm_cap, 1) then return {0, 'RATE_LIMITED', 'rpm'} end
if over_cap(KEYS[3], tpm_cap, est_tokens) then return {0, 'RATE_LIMITED', 'tpm'} end
if over_cap(KEYS[4], rpd_cap, 1) then return {0, 'RATE_LIMITED', 'rpd'} end
if conc_cap > 0 then
    local conc = tonumber(redis.call('GET', KEYS[5]) or '0')
    if conc + 1 > conc_cap then return {0, 'RATE_LIMITED', 'concurrency'} end
end

local est = tonumber(ARGV[2])
local bal = tonumber(redis.call('HGET', KEYS[1], 'avail') or '0')
if bal < est then
    return {0, 'INSUFFICIENT', redis.call('HGET', KEYS[1], 'avail') or '0'}
end

redis.call('HINCRBY', KEYS[1], 'avail', -est)
redis.call('HSET', KEYS[1], 'r:' .. ARGV[1], ARGV[2] .. '|' .. ARGV[3] .. '|' .. ARGV[9])
redis.call('INCR', KEYS[2]); redis.call('EXPIRE', KEYS[2], 120)
redis.call('INCRBY', KEYS[3], est_tokens); redis.call('EXPIRE', KEYS[3], 120)
redis.call('INCR', KEYS[4]); redis.call('EXPIRE', KEYS[4], 172800)
redis.call('INCR', KEYS[5]); redis.call('EXPIRE', KEYS[5], 3600)
return {1, redis.call('HGET', KEYS[1], 'avail')}
