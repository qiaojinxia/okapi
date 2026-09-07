-- 分组级限流（IMPLEMENTATION §11.32，docs/database.md §1.2 / §2.1）。
--
-- default / vip / svip 是档位，档位之间除价差外还该有速率差：分组内**每用户**的
-- 分钟窗 / 小时窗请求上限。NULL = 不限；限额随鉴权缓存下发到网关，计数在 Redis
-- rl:{uid}:g:<group>:rpm|rph:<桶>。

ALTER TABLE price_groups
    ADD COLUMN IF NOT EXISTS rpm_limit INT CHECK (rpm_limit IS NULL OR rpm_limit > 0),
    ADD COLUMN IF NOT EXISTS rph_limit INT CHECK (rph_limit IS NULL OR rph_limit > 0);
COMMENT ON COLUMN price_groups.rpm_limit IS
    '分组内每用户每分钟请求上限（固定分钟窗，reserve 前检查；超限 429 rate_limited/group_rpm）；NULL = 不限';
COMMENT ON COLUMN price_groups.rph_limit IS
    '分组内每用户每小时请求上限（固定小时窗）；NULL = 不限';
