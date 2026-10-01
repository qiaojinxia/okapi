ORDER BY CASE WHEN p.pricing_mode = 'ratio' AND p.model_ratio_scaled >= 0 AND p.completion_ratio_scaled >= 0 THEN
    p.model_ratio_scaled::numeric * p.completion_ratio_scaled::numeric * CASE WHEN $12::text IS NULL THEN 1 ELSE (
        SELECT pg.ratio_scaled FROM price_groups g
        JOIN jsonb_to_recordset($8::jsonb -> 'groups') AS pg(group_code text, ratio_scaled bigint)
            ON pg.group_code = g.group_code
        WHERE g.group_code = $12 AND (g.self_select OR g.is_default OR EXISTS (
            SELECT 1 FROM user_groups ug WHERE ug.group_code = g.group_code AND ug.user_id = $7
        ))
    ) END
END ASC NULLS LAST, lower(COALESCE(NULLIF(m.display_name, ''), m.model_name)), m.model_name
