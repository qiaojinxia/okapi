-- Claude Code client simulation keeps only the latest captured client (2.1.290).
-- The retired channel switch settings.mimic_cc always rewrote requests, so it becomes an
-- explicit mimic profile; saved profiles that named a retired revision move to the latest.
-- mimic_cc / mimic_cc_version are no longer read anywhere.
UPDATE channels
SET settings = jsonb_set(
        settings - 'mimic_cc' - 'mimic_cc_version',
        '{extensions}',
        COALESCE(NULLIF(settings -> 'extensions', 'null'::jsonb), '{}'::jsonb)
            || '{"client_profile":{"name":"claude-code","mode":"mimic","revision":"2.1.290"}}'::jsonb,
        true)
WHERE settings -> 'mimic_cc' = 'true'::jsonb
  AND jsonb_typeof(COALESCE(NULLIF(settings -> 'extensions', 'null'::jsonb), '{}'::jsonb)) = 'object'
  AND NOT COALESCE(NULLIF(settings -> 'extensions', 'null'::jsonb), '{}'::jsonb) ? 'client_profile';

UPDATE channels
SET settings = settings - 'mimic_cc' - 'mimic_cc_version'
WHERE settings ?| ARRAY['mimic_cc', 'mimic_cc_version'];

UPDATE channels
SET settings = jsonb_set(settings, '{extensions,client_profile,revision}', '"2.1.290"'::jsonb)
WHERE settings #>> '{extensions,client_profile,name}' = 'claude-code'
  AND settings #>> '{extensions,client_profile,revision}' IN ('2.1.258', '2.1.286');
