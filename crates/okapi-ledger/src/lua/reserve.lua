-- reserve：余额预扣 + key 级限速/并发准入（docs/database.md §2.2）
-- KEYS[1] bal:{uid}
-- KEYS[2] rl:{uid}:k:<kid>:rpm:<bucket>  KEYS[3] rl:{uid}:k:<kid>:tpm:<bucket>
-- KEYS[4] rl:{uid}:k:<kid>:rpd:<day>     KEYS[5] conc:{uid}:k:<kid>
-- ARGV[1] request_id  ARGV[2] est_micro  ARGV[3] deadline_ms
-- ARGV[4] rpm_cap  ARGV[5] tpm_cap  ARGV[6] rpd_cap  ARGV[7] conc_cap
-- ARGV[8] est_tokens  ARGV[9] api_key_id  ARGV[10] now_unix_s
-- 全部 KEYS 同 {uid} hash-tag（Cluster 单槽原子）。cap<=0 = 不限。
-- 预扣字段值 = "<est_micro>|<deadline_ms>|<api_key_id>|<pool>"（pool 0 钱包 / 1 订阅池；
-- 释放时按 kid 归还并发槽、按 pool 回到同一个池）。
-- 选池（IMPLEMENTATION §11.28）：订阅池 sub > 0 且 now < sub_until → 从 sub 扣，**不校验足额**
-- （允许最后一笔越界，下窗重置）；否则钱包，avail >= est 才放行（fail-closed）。
-- 精度注：整数绝对值与递增结果最多 2^53-1，超出在写入前拒绝。

-- 原预扣存在时，重复调用不能重新准入：否则再次扣款却覆盖唯一恢复记录。
-- 必须在限流与选池之前检查，订阅越界/窗口变更也不能让同请求改扣钱包。
-- 不延长 deadline、不改写旧/异常格式记录，由原请求或对账路径处理。
if redis.call('HEXISTS', KEYS[1], 'r:' .. ARGV[1]) == 1 then
    return {0, 'RESERVATION_EXISTS'}
end

local maximum = 9007199254740991
local function integer(raw, signed)
    if type(raw) ~= 'string' then return nil end
    if raw ~= '0' and not string.match(raw, '^[1-9][0-9]*$')
        and not (signed and string.match(raw, '^%-[1-9][0-9]*$')) then return nil end
    local value = tonumber(raw)
    if not value or math.abs(value) > maximum then return nil end
    return value
end

local est = integer(ARGV[2], false)
local est_tokens = integer(ARGV[8], false)
if not est or not est_tokens then return {0, 'INVALID_RESERVATION'} end

-- A script is isolated, but Redis does not roll back writes on runtime errors.
-- Validate ALL counters, including unlimited ones, before changing any state.
local increments = {1, est_tokens, 1, 1}
local current, caps = {}, {}
for i = 1, 4 do
    caps[i] = tonumber(ARGV[i + 3])
    if not caps[i] or caps[i] > maximum then return {0, 'INVALID_RESERVATION'} end
    current[i] = integer(redis.pcall('GET', KEYS[i + 1]) or '0', false)
    if not current[i] or current[i] > maximum - increments[i] then
        return {0, 'ADMISSION_STATE_INVALID'}
    end
end
local axes = {'rpm', 'tpm', 'rpd'}
for i = 1, 3 do
    if caps[i] > 0 and current[i] + increments[i] > caps[i] then
        return {0, 'RATE_LIMITED', axes[i]}
    end
end
local durable_slots = nil
if caps[4] > 0 then
    durable_slots = durable_concurrency(KEYS[1], ARGV[9])
    if not durable_slots then return {0, 'HOLD_RECOVERY_REQUIRED'} end
    if current[4] + 1 > caps[4] - durable_slots then return {0, 'RATE_LIMITED', 'concurrency'} end
end

local now = tonumber(ARGV[10] or '0')
local sub = integer(redis.call('HGET', KEYS[1], 'sub') or '0', true)
local sub_until = integer(redis.call('HGET', KEYS[1], 'sub_until') or '0', true)
if not sub or not sub_until then return {0, 'ADMISSION_STATE_INVALID'} end

local field = 'avail'
local pool = 0
local epoch = ''
if sub > 0 and now < sub_until then
    field = 'sub'
    pool = 1
    epoch = redis.call('HGET', KEYS[1], 'sub_epoch') or ''
    if epoch ~= '' and not ledger_epoch(epoch) then return {0, 'ADMISSION_STATE_INVALID'} end
else
    local bal = integer(redis.call('HGET', KEYS[1], 'avail') or '0', true)
    if not bal then return {0, 'ADMISSION_STATE_INVALID'} end
    if bal < est then
        return {0, 'INSUFFICIENT', redis.call('HGET', KEYS[1], 'avail') or '0'}
    end
end

-- Keep large safe integers as decimal strings when passing them to Redis.
redis.call('HINCRBY', KEYS[1], field, est == 0 and '0' or '-' .. ARGV[2])
if durable_slots then redis.call('HSET', KEYS[1], 'hc:' .. ARGV[9], tostring(durable_slots)) end
local receipt = ARGV[2] .. '|' .. ARGV[3] .. '|' .. ARGV[9] .. '|' .. pool
if epoch ~= '' then receipt = receipt .. '|w:' .. epoch end
redis.call('HSET', KEYS[1], 'r:' .. ARGV[1], receipt)
redis.call('INCR', KEYS[2]); redis.call('EXPIRE', KEYS[2], 120)
redis.call('INCRBY', KEYS[3], ARGV[8]); redis.call('EXPIRE', KEYS[3], 120)
redis.call('INCR', KEYS[4]); redis.call('EXPIRE', KEYS[4], 172800)
redis.call('INCR', KEYS[5]); redis.call('EXPIRE', KEYS[5], 3600)
return {1, redis.call('HGET', KEYS[1], field), pool, epoch}
