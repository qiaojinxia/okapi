-- 模态 token 分轴（DESIGN §3.2 prompt 五段 / completion 两段）。
--
-- 背景：多模态模型的音频、图片 token 与文本**不同价**，而此前全部按 model_ratio 计。
-- 以 gpt-4o-audio-preview 官方定价为例（text in $2.5/1M、audio in $40/1M、
-- audio out $80/1M）：音频输入是文本的 16×，统一按文本计会漏收约 8 成
-- （回归断言见 crates/okapi-pricing/tests/parity.rs::openai_audio_official_pricing_parity）。
--
-- 语义（与 new-api 的 audio_ratio / audio_completion_ratio / image_ratio 对齐）：
--   audio_ratio            音频输入相对文本的倍数（gpt-4o-audio = 16）
--   audio_completion_ratio 音频输出相对音频输入的倍数，**叠乘在 audio_ratio 之上**（= 2）
--   image_ratio            图片输入相对文本的倍数
-- 缺省 1.0 = 按文本计（保持旧行为），故对纯文本模型与既有站点零影响。

ALTER TABLE model_pricing
    ADD COLUMN IF NOT EXISTS audio_ratio            NUMERIC(12,6) NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS audio_completion_ratio NUMERIC(12,6) NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS image_ratio            NUMERIC(12,6) NOT NULL DEFAULT 1;

COMMENT ON COLUMN model_pricing.audio_ratio IS
    '音频输入倍率（相对文本；gpt-4o-audio 官方 16.0，缺省 1.0=按文本计）';
COMMENT ON COLUMN model_pricing.audio_completion_ratio IS
    '音频输出倍率，叠乘在 audio_ratio 之上（官方 2.0 → 输出 = 文本×16×2）';
COMMENT ON COLUMN model_pricing.image_ratio IS
    '图片输入倍率（相对文本，缺省 1.0）';
