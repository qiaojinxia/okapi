-- Explicit independent fees. NULL preserves legacy unconfigured pricing.
ALTER TABLE model_pricing ADD COLUMN server_tool_prices jsonb;
ALTER TABLE model_pricing ADD CONSTRAINT server_tool_prices_object
    CHECK (server_tool_prices IS NULL OR jsonb_typeof(server_tool_prices) = 'object');
