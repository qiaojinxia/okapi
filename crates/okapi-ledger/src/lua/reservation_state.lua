-- Read-only validation shared by closing reservations and resetting subscriptions.
local ledger_maximum = 9007199254740991
local function ledger_integer(raw, signed)
    if type(raw) ~= 'string' then return nil end
    if raw ~= '0' and not string.match(raw, '^[1-9][0-9]*$')
        and not (signed and string.match(raw, '^%-[1-9][0-9]*$')) then return nil end
    local n = tonumber(raw)
    if not n or math.abs(n) > ledger_maximum then return nil end
    return n
end
local function ledger_decimal(n)
    return string.format('%.0f', n)
end
local function ledger_add(a, b)
    if (b > 0 and a > ledger_maximum - b) or (b < 0 and a < -ledger_maximum - b) then return nil end
    return a + b
end
local function ledger_key(raw)
    -- PG bigint identities must be compared as strings, including above 2^53.
    return raw == '0' or (string.match(raw, '^[1-9][0-9]*$')
        and (#raw < 19 or (#raw == 19 and raw <= '9223372036854775807')))
end
local function ledger_epoch(raw)
    return type(raw) == 'string' and #raw > 0 and #raw <= 128
        and string.match(raw, '^[%w%-%._:]+$') ~= nil
end
local function ledger_reservation(raw)
    if type(raw) ~= 'string' then return nil end
    local parts = {}
    for part in string.gmatch(raw .. '|', '(.-)|') do
        if part == '' or #parts >= 5 then return nil end
        parts[#parts + 1] = part
    end
    if #parts < 2 then return nil end
    local amount = ledger_integer(parts[1], false)
    local deadline = ledger_integer(parts[2], false)
    local key = parts[3] or '0'
    local pool = parts[4] or '0'
    if not amount or not deadline or not ledger_key(key) or (pool ~= '0' and pool ~= '1') then return nil end
    local epoch = nil
    if parts[5] then
        if pool ~= '1' or string.sub(parts[5], 1, 2) ~= 'w:' then return nil end
        epoch = string.sub(parts[5], 3)
        if not ledger_epoch(epoch) then return nil end
    end
    return {amount=amount, key=key, epoch=epoch, pool=tonumber(pool), field=pool == '1' and 'sub' or 'avail'}
end
