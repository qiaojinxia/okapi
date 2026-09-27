-- Persist normalized billing inputs independently of ClickHouse retention/availability.
-- Historical records stay NULL: an unrecorded cache field is not a measured zero.
ALTER TABLE billing_records ADD COLUMN usage_details JSONB;
COMMENT ON COLUMN billing_records.usage_details IS
    'Normalized token usage and public request dimensions at settlement; NULL for historical records';
