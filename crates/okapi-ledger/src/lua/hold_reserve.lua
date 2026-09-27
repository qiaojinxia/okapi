-- KEYS: bal:{uid}, hold:{uid}:<id>, conc:{uid}:k:<kid>. Same slot.
-- ARGV: id, maximum_micro, api_key_id, request_hash, now_unix_s, PG window epoch/until.
-- ARGV[8]: current API-key max_concurrency; <=0/omitted is unlimited.
-- The durable PG intent must exist before this call. Receipts do not expire.
local field = 'h:' .. ARGV[1]
local receipt = redis.call('GET', KEYS[2])
if receipt then
    local h = cjson.decode(receipt)
    if h.amount ~= ARGV[2] or h.key ~= ARGV[3] or h.proof ~= ARGV[4] then
        return cjson.encode({error='conflict'})
    end
    if h.phase ~= 'held' or redis.call('HGET', KEYS[1], field) ~= receipt then
        return cjson.encode({error='recovery_required'})
    end
    return receipt
end
if redis.call('HEXISTS', KEYS[1], field) == 1 then
    return cjson.encode({error='recovery_required'})
end
local durable_slots = durable_concurrency(KEYS[1], ARGV[3])
if not durable_slots or durable_slots >= 128 then return cjson.encode({error='recovery_required'}) end
local cap = tonumber(ARGV[8] or '0')
if cap > 0 then
    local raw = redis.call('GET', KEYS[3]) or '0'
    if raw ~= '0' and not string.match(raw, '^[1-9][0-9]*$') then return cjson.encode({error='recovery_required'}) end
    local ordinary = tonumber(raw)
    if not ordinary or ordinary > 9007199254740991 then return cjson.encode({error='recovery_required'}) end
    if ordinary + durable_slots + 1 > cap then return cjson.encode({error='concurrency'}) end
end
local maximum = 9007199254740991
local function money(raw)
    local n = tonumber(raw)
    if not n or math.abs(n) > maximum or n ~= math.floor(n) then return nil end
    return n
end
local amount = money(ARGV[2])
if not amount or amount < 0 then
    return cjson.encode({error='invalid_amount'})
end
local pool, balance_field, epoch = 0, 'avail', ''
local sub = money(redis.call('HGET', KEYS[1], 'sub') or '0')
if not sub then return cjson.encode({error='invalid_amount'}) end
local until_at = tonumber(redis.call('HGET', KEYS[1], 'sub_until') or '0')
local current = sub
if sub > 0 and tonumber(ARGV[5]) < until_at then
    epoch = redis.call('HGET', KEYS[1], 'sub_epoch') or ''
    -- Backfill legacy subscriptions only from an exactly matching active PG window.
    if epoch == '' and ARGV[6] and ARGV[6] ~= '' and tonumber(ARGV[7]) == until_at then
        epoch = ARGV[6]
    end
    if epoch == '' then return cjson.encode({error='window_required'}) end
    pool, balance_field = 1, 'sub'
else
    local balance = redis.call('HGET', KEYS[1], 'avail') or '0'
    current = money(balance)
    if not current then return cjson.encode({error='invalid_amount'}) end
    if current < amount then
        return cjson.encode({error='insufficient', balance=balance})
    end
end
local next_balance = current - amount
if math.abs(next_balance) > maximum then
    return cjson.encode({error='invalid_amount'})
end
local encoded = cjson.encode({amount=ARGV[2], key=ARGV[3], proof=ARGV[4],
    pool=pool, epoch=epoch, phase='held'})
redis.call('HINCRBY', KEYS[1], balance_field, amount == 0 and '0' or ('-' .. ARGV[2]))
if pool == 1 then redis.call('HSET', KEYS[1], 'sub_epoch', epoch) end
redis.call('HSET', KEYS[1], field, encoded)
redis.call('HSET', KEYS[1], 'hc:' .. ARGV[3], tostring(durable_slots + 1))
redis.call('SET', KEYS[2], encoded)
return encoded
