-- Derived durable concurrency lives beside the non-expiring held funds, not
-- in the expiring synchronous slot. Rebuild absent legacy fields lazily; the
-- PG repair path rebuilds the complete index. No writes during validation.
local function durable_concurrency(balance, api_key)
    local raw = redis.call('HGET', balance, 'hc:' .. api_key)
    if raw then
        if raw ~= '0' and not string.match(raw, '^[1-9][0-9]*$') then return nil end
        local count = tonumber(raw)
        if not count or count > 128 then return nil end
        return count
    end
    local all = redis.call('HGETALL', balance)
    local count = 0
    for i = 1, #all, 2 do
        if string.sub(all[i], 1, 2) == 'h:' then
            local ok, hold = pcall(cjson.decode, all[i + 1])
            if not ok or type(hold) ~= 'table' or hold.phase ~= 'held'
                or type(hold.key) ~= 'string' or not string.match(hold.key, '^[1-9][0-9]*$') then
                return nil
            end
            if hold.key == api_key then count = count + 1 end
        end
    end
    if count > 128 then return nil end
    return count
end
