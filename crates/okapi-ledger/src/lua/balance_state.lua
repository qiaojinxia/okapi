-- Compare the entire monetary/admission state, including reservations and fund
-- receipts. A delayed repair cannot overwrite a newer debit, credit or window.
local function ledger_balance_state(key)
    local all = redis.call('HGETALL', key)
    local fields, values = {}, {}
    for i=1,#all,2 do
        fields[#fields+1]=all[i]
        values[all[i]]=all[i+1]
    end
    table.sort(fields)
    local parts = {}
    for _,field in ipairs(fields) do
        local value=values[field]
        parts[#parts+1]=#field .. ':' .. field .. #value .. ':' .. value
    end
    return redis.sha1hex(table.concat(parts))
end
