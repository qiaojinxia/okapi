-- NULL denotes historical/unscoped records; never invent historical window identity.
ALTER TABLE billing_records ADD COLUMN source_window TEXT;
ALTER TABLE billing_record_receipts ADD COLUMN source_window TEXT;
ALTER TABLE billing_sync ADD COLUMN source_window TEXT;
CREATE OR REPLACE VIEW billing_financial_records AS
SELECT request_id,user_id,api_key_id,status,amount_micro,pool,created_at,source_window FROM billing_records
UNION ALL
SELECT request_id,user_id,api_key_id,status,amount_micro,pool,created_at,source_window FROM billing_record_receipts;
