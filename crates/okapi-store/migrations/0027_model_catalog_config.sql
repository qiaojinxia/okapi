-- Declarative catalog metadata, separate from channel/endpoint routing capabilities.
ALTER TABLE models ADD COLUMN catalog_config JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE models ADD CONSTRAINT models_catalog_config_object
    CHECK (jsonb_typeof(catalog_config) = 'object');
-- All ratio axes share the same 1e6 fixed-point precision; do not round cache rates to 4 decimals.
ALTER TABLE model_pricing ALTER COLUMN cache_ratio TYPE NUMERIC(12,6);
ALTER TABLE model_pricing ALTER COLUMN cache_write_ratio TYPE NUMERIC(12,6);
