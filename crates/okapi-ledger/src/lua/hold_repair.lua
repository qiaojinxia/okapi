-- Atomic two-pool repair, with PG-confirmed durable holds and pending Redis receipts.
-- KEYS[1] balance; KEYS[2..] receipts, matching manifest order. Same {uid} slot.
-- ARGV: wallet target, subscription target, hold manifest, sub epoch/until, fund manifest, fund high-water sequence, expected balance-state fingerprint.
-- Validate everything before the first write: Redis script errors do not roll back writes.
if not ARGV[8] or ledger_balance_state(KEYS[1]) ~= ARGV[8] then
    return cjson.encode({error='recovery_required'})
end
local maximum = 9007199254740991
local function integer(raw)
    local n = tonumber(raw)
    if not n or math.abs(n) > maximum or n ~= math.floor(n) then return nil end
    return n
end
local wallet, sub = integer(ARGV[1]), integer(ARGV[2])
if not wallet or not sub then return cjson.encode({error='invalid_amount'}) end
local manifest = cjson.decode(ARGV[3])
local known, plans, concurrency = {}, {}, {}
local frozen = {[0]=0,[1]=0}
for i, item in ipairs(manifest) do
    local field = 'h:' .. item.id
    known[field] = true
    local receipt = redis.call('GET', KEYS[i+1])
    local active = redis.call('HGET', KEYS[1], field)
    local old = receipt and cjson.decode(receipt) or (active and cjson.decode(active) or nil)
    if old and (old.amount ~= item.amount or old.key ~= item.key or old.proof ~= item.proof) then
        return cjson.encode({error='conflict'})
    end
    if old and ((old.pool ~= 0 and old.pool ~= 1) or type(old.epoch) ~= 'string'
        or (old.pool == 1) == (old.epoch == '') or (old.phase ~= 'held' and old.phase ~= 'closed')
        or (old.phase == 'held' and (old.actual ~= nil or old.credit ~= nil))) then
        return cjson.encode({error='conflict'})
    end
    local wanted = item.receipt
    if item.state ~= 'pending' and old and (old.pool ~= wanted.pool or old.epoch ~= wanted.epoch) then
        return cjson.encode({error='conflict'})
    end
    if item.state == 'pending' then
        wanted = old
        local sealed = item.seal and item.seal ~= cjson.null
        if old and old.phase ~= 'held' then
            if not sealed or old.phase ~= 'closed' or old.actual ~= '0' or old.credit ~= '0'
                or old.pool ~= 0 or old.epoch ~= '' then return cjson.encode({error='conflict'}) end
        elseif not old and sealed then wanted = item.seal end
    elseif old and old.phase == 'closed' then
        if wanted.phase ~= 'closed' or old.actual ~= wanted.actual or old.credit ~= wanted.credit then
            return cjson.encode({error='conflict'})
        end
    end
    if wanted and wanted ~= cjson.null then
        if wanted.pool ~= 0 and wanted.pool ~= 1 then return cjson.encode({error='conflict'}) end
        local amount = integer(wanted.amount)
        if not amount or amount < 0 then return cjson.encode({error='invalid_amount'}) end
        if wanted.phase == 'held' then
            frozen[wanted.pool] = frozen[wanted.pool] + amount
            concurrency[wanted.key] = (concurrency[wanted.key] or 0) + 1
        elseif wanted.phase ~= 'closed' then return cjson.encode({error='conflict'}) end
        plans[#plans+1] = {field=field, key=KEYS[i+1], value=cjson.encode(wanted), phase=wanted.phase}
    end
end
local old_epoch = redis.call('HGET', KEYS[1], 'sub_epoch') or ''
if (old_epoch ~= '' and not ledger_epoch(old_epoch)) or (ARGV[4] ~= '' and not ledger_epoch(ARGV[4])) then
    return cjson.encode({error='invalid_reservation'})
end
local all = redis.call('HGETALL', KEYS[1])
for i=1,#all,2 do
    local field = all[i]
    if string.sub(field,1,2) == 'r:' then
        local receipt = ledger_reservation(all[i+1])
        if not receipt then return cjson.encode({error='invalid_reservation'}) end
        if receipt.pool == 0 or not receipt.epoch or receipt.epoch == ARGV[4] then
            frozen[receipt.pool] = frozen[receipt.pool] + receipt.amount
        end
    elseif string.sub(field,1,2) == 'h:' and not known[field] then
        return cjson.encode({error='unknown_hold'})
    end
end
local next_wallet, next_sub = wallet - frozen[0], sub - frozen[1]
if not integer(frozen[0]) or not integer(frozen[1]) or not integer(next_wallet) or not integer(next_sub) then
    return cjson.encode({error='invalid_amount'})
end
local previous_wallet = redis.call('HGET', KEYS[1], 'avail') or '0'
local previous_sub = redis.call('HGET', KEYS[1], 'sub') or '0'
if not integer(previous_wallet) or not integer(previous_sub) then return cjson.encode({error='invalid_amount'}) end
local transfers = cjson.decode(ARGV[6])
local function sequence(raw)
    if type(raw) ~= 'string' then return nil end
    if raw == '0' then return raw end
    if not string.match(raw, '^[1-9]%d*$') or #raw > 19
        or (#raw == 19 and raw > '9223372036854775807') then return nil end
    return raw
end
local wanted = sequence(ARGV[7])
local previous = sequence(redis.call('HGET', KEYS[1], 'fund_seq') or '0')
if not wanted or not previous or #previous > #wanted or (#previous == #wanted and previous > wanted) then
    return cjson.encode({error='conflict'})
end
for _,item in ipairs(transfers) do
    local old = redis.call('HGET', KEYS[1], 'c:' .. item.id)
    if old and old ~= item.receipt then return cjson.encode({error='conflict'}) end
end
local function decimal(n) return string.format('%.0f', n) end
-- Reconstruct from authoritative final held receipts, clearing stale derived
-- entries as well. Failed validation above leaves the original index untouched.
for i = 1, #all, 2 do
    if string.sub(all[i], 1, 3) == 'hc:' then redis.call('HDEL', KEYS[1], all[i]) end
end
for key, count in pairs(concurrency) do
    redis.call('HSET', KEYS[1], 'hc:' .. key, tostring(count))
end
for _,plan in ipairs(plans) do
    if plan.phase == 'held' then redis.call('HSET', KEYS[1], plan.field, plan.value)
    else redis.call('HDEL', KEYS[1], plan.field) end
    redis.call('SET', plan.key, plan.value)
end
redis.call('HSET', KEYS[1], 'avail', decimal(next_wallet), 'sub', decimal(next_sub),
    'sub_epoch', ARGV[4], 'sub_until', ARGV[5], 'fund_seq', wanted)
return cjson.encode({wallet={before=previous_wallet,after=decimal(next_wallet),inflight=decimal(frozen[0])},
    sub={before=previous_sub,after=decimal(next_sub),inflight=decimal(frozen[1])}})
