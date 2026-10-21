ORDER BY CASE WHEN p.pricing_mode = 'ratio' AND p.model_ratio_scaled >= 0 THEN
    p.model_ratio_scaled::numeric * CASE WHEN $12::text IS NULL THEN 1 ELSE (
        SELECT pg.ratio_scaled FROM price_groups g
        JOIN jsonb_to_recordset($8::jsonb -> 'groups') AS pg(group_code text, ratio_scaled bigint)
            ON pg.group_code = g.group_code
        WHERE g.group_code = $12 AND (g.self_select OR EXISTS (
            SELECT 1 FROM user_groups ug WHERE ug.group_code = g.group_code AND ug.user_id = $7
        ) OR (g.is_default AND NOT EXISTS (
            SELECT 1 FROM user_groups ug WHERE ug.user_id = $7 AND NOT EXISTS (
                SELECT 1 FROM user_subscriptions s WHERE s.user_id = ug.user_id AND s.status = 1
                    AND s.granted_group AND s.group_code_snapshot = ug.group_code)
        )))
    ) END
END ASC NULLS LAST, lower(COALESCE(NULLIF(m.display_name, ''), m.model_name)), m.model_name
