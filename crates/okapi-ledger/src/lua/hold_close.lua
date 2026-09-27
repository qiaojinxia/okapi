-- KEYS: bal:{uid}, hold:{uid}:<id>. ARGV: id, canonical closed receipt JSON.
-- PG settlement is committed first. Replays only verify the original receipt.
local wanted = cjson.decode(ARGV[2])
if wanted.phase ~= 'closed' or (wanted.pool ~= 0 and wanted.pool ~= 1) then
    return cjson.encode({error='conflict'})
end
local raw = redis.call('GET', KEYS[2])
if not raw then return cjson.encode({error='recovery_required'}) end
local old = cjson.decode(raw)
if old.amount ~= wanted.amount or old.key ~= wanted.key or old.proof ~= wanted.proof
    or old.pool ~= wanted.pool or old.epoch ~= wanted.epoch then
    return cjson.encode({error='conflict'})
end
if old.phase == 'closed' then
    if old.actual ~= wanted.actual or old.credit ~= wanted.credit then
        return cjson.encode({error='conflict'})
    end
    return raw
end
if old.phase ~= 'held' or redis.call('HGET', KEYS[1], 'h:' .. ARGV[1]) ~= raw then
    return cjson.encode({error='recovery_required'})
end
local durable_slots = durable_concurrency(KEYS[1], old.key)
if not durable_slots or durable_slots < 1 then return cjson.encode({error='recovery_required'}) end
local credit = tonumber(wanted.credit)
local actual = tonumber(wanted.actual)
local amount = tonumber(old.amount)
if not credit or not actual or credit < 0 or actual < 0 or actual > amount
    or credit > amount - actual or credit ~= math.floor(credit) or actual ~= math.floor(actual)
    or (old.pool == 0 and credit ~= amount - actual) then
    return cjson.encode({error='invalid_amount'})
end
local field = old.pool == 1 and 'sub' or 'avail'
if old.pool == 1 and credit > 0 and (redis.call('HGET', KEYS[1], 'sub_epoch') or '') ~= old.epoch then
    return cjson.encode({error='window_conflict'})
end
local current = tonumber(redis.call('HGET', KEYS[1], field) or '0')
-- Check before arithmetic: an out-of-range input can round back into the accepted
-- interval after adding credit, while Redis HINCRBY still uses its exact integer.
if not current or math.abs(current) > 9007199254740991 or current ~= math.floor(current) then
    return cjson.encode({error='invalid_amount'})
end
local next_balance = current + credit
if math.abs(next_balance) > 9007199254740991 then
    return cjson.encode({error='invalid_amount'})
end
redis.call('HINCRBY', KEYS[1], field, wanted.credit)
redis.call('HDEL', KEYS[1], 'h:' .. ARGV[1])
redis.call('HSET', KEYS[1], 'hc:' .. old.key, tostring(durable_slots - 1))
redis.call('SET', KEYS[2], ARGV[2])
return ARGV[2]
