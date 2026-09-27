-- Fence a pending admission before its zero-cost cancellation is committed in PG.
-- An earlier reserve either already froze funds (return its receipt), or this
-- permanent zero-credit receipt prevents a delayed reserve from ever debiting.
-- KEYS: balance, receipt. ARGV: id, maximum, api key, proof.
local raw = redis.call('GET', KEYS[2])
if raw then
    local old = cjson.decode(raw)
    if old.amount ~= ARGV[2] or old.key ~= ARGV[3] or old.proof ~= ARGV[4] then
        return cjson.encode({error='conflict'})
    end
    if old.phase == 'held' then
        if redis.call('HGET', KEYS[1], 'h:' .. ARGV[1]) ~= raw then
            return cjson.encode({error='recovery_required'})
        end
    elseif old.phase ~= 'closed' or old.pool ~= 0 or old.epoch ~= ''
        or old.actual ~= '0' or old.credit ~= '0' then
        return cjson.encode({error='conflict'})
    end
    return raw
end
if redis.call('HEXISTS', KEYS[1], 'h:' .. ARGV[1]) == 1 then
    return cjson.encode({error='recovery_required'})
end
local sealed = cjson.encode({amount=ARGV[2],key=ARGV[3],proof=ARGV[4],
    pool=0,epoch='',phase='closed',actual='0',credit='0'})
redis.call('SET', KEYS[2], sealed)
return sealed
