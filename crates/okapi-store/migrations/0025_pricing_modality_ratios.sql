-- Decimal-string prices relative to text input. {} retains legacy fallback rates.
ALTER TABLE model_pricing ADD COLUMN modality_ratios jsonb NOT NULL DEFAULT '{}'
    CHECK (jsonb_typeof(modality_ratios) = 'object');
