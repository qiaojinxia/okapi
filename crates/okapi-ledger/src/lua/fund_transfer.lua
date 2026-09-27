-- One user's hash contains both balance and receipt. Never expire receipts
-- before PG acknowledges application. Validate every input before any write.
local maximum = 9007199254740991
local function integer(raw)
    if type(raw) ~= 'string' then return nil end
    local n = tonumber(raw)
    if not n or math.abs(n) > maximum or n ~= math.floor(n)
        or string.format('%.0f', n) ~= raw then return nil end
    return n
end
-- Sequence IDs are full PG bigint strings, not Lua doubles.
local function sequence(raw)
    if type(raw) ~= 'string' then return nil end
    if raw == '0' then return raw end
    if not string.match(raw, '^[1-9]%d*$') or #raw > 19
        or (#raw == 19 and raw > '9223372036854775807') then return nil end
    return raw
end
local function newer(a,b) return #a > #b or (#a == #b and a > b) end
local wanted = sequence(ARGV[4])
local watermark = sequence(redis.call('HGET', KEYS[1], 'fund_seq') or '0')
if not wanted or wanted == '0' or not watermark then return 'invalid' end
local amount = integer(ARGV[2])
local field = ARGV[3]
if not amount or (field ~= 'avail' and field ~= 'sub') then return 'invalid' end
local receipt = field .. '|' .. ARGV[2]
local old = redis.call('HGET', KEYS[1], 'c:' .. ARGV[1])
if old and old ~= receipt then return 'conflict' end
local raw = redis.call('HGET', KEYS[1], field)
if not raw then return 'missing' end
local before = integer(raw)
if not before then return 'invalid' end
-- Even a delayed old connection after PG acknowledgement/receipt cleanup
-- cannot reapply any sequence already included in this balance.
if not newer(wanted,watermark) then return 'applied' end
if old then
    redis.call('HSET', KEYS[1], 'fund_seq', wanted)
    return 'applied'
end
local after = before + amount
if math.abs(after) > maximum or after ~= math.floor(after) then return 'invalid' end
redis.call('HSET', KEYS[1], field, string.format('%.0f', after), 'c:' .. ARGV[1], receipt, 'fund_seq', wanted)
return 'applied'
