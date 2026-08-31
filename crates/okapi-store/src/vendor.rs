//! 模型名 → 供应商自动归类（吸收 new-api `model/pricing_default.go` 的前缀规则思路）。
//!
//! # 设计取舍
//!
//! new-api 把供应商建成独立 `Vendor` 表（id/name/icon/status）并在启动时自动 upsert。
//! 我们**不新增表**——`models.vendor` 已是字符串列，而供应商在本系统里只用于展示分组
//! 与筛选，没有需要外键约束的属性（图标属前端资源）。这符合 IMPLEMENTATION §11.4 的
//! 吸收判据②：同等语义能用现有维度表达就不新增表。
//!
//! 归类只在 `vendor` 为空时生效，管理员显式填写的值永不被覆盖。
//!
//! # 规则表的来源与验证
//!
//! 规则不凭印象罗列：以 LiteLLM `model_prices_and_context_window.json`（社区维护、
//! 更新最勤的真实模型清单，3400+ 条）反查覆盖率，逐个补齐未归类的厂商系列。
//! `coverage_over_real_model_catalog` 用例把这条纪律固化——新增规则后仍须达标。

/// Bedrock / Vertex 的区域与部署前缀。这些不是模型标识，必须先剥离再匹配，
/// 否则 `us.amazon.nova-lite-v1:0` 会完全匹配不上任何厂商规则。
const REGION_PREFIXES: &[&str] = &["us.", "eu.", "apac.", "global.", "us-gov."];

/// 前缀/子串 → 供应商。按**最长匹配优先**，故声明顺序无关（见 `classify`）。
///
/// 命名统一用厂商英文名：本系统标识符一律英文（00-project 规约），
/// 中文展示名由前端语言包渲染。
const RULES: &[(&str, &str)] = &[
    // ---- OpenAI ----
    ("gpt", "OpenAI"),
    ("chatgpt", "OpenAI"),
    ("o1", "OpenAI"),
    ("o3", "OpenAI"),
    ("o4", "OpenAI"),
    ("dall-e", "OpenAI"),
    ("whisper", "OpenAI"),
    ("tts-", "OpenAI"),
    ("text-embedding", "OpenAI"),
    ("text-moderation", "OpenAI"),
    ("omni-moderation", "OpenAI"),
    ("sora", "OpenAI"),
    ("codex", "OpenAI"),
    ("computer-use", "OpenAI"),
    ("deep-research", "OpenAI"),
    ("babbage", "OpenAI"),
    ("davinci", "OpenAI"),
    ("ft:", "OpenAI"),
    // ---- Anthropic ----
    ("claude", "Anthropic"),
    // ---- Google ----
    ("gemini", "Google"),
    ("gemma", "Google"),
    ("imagen", "Google"),
    ("veo", "Google"),
    ("medlm", "Google"),
    ("multimodalembedding", "Google"),
    ("textembedding-gecko", "Google"),
    ("text-multilingual-embedding", "Google"),
    ("text-unicorn", "Google"),
    ("chat-bison", "Google"),
    ("text-bison", "Google"),
    ("code-bison", "Google"),
    // ---- 中国厂商 ----
    ("deepseek", "DeepSeek"),
    ("qwen", "Alibaba"),
    ("qwq", "Alibaba"),
    ("qvq", "Alibaba"),
    ("moonshot", "Moonshot"),
    ("kimi", "Moonshot"),
    ("glm", "Zhipu"),
    ("chatglm", "Zhipu"),
    ("cogview", "Zhipu"),
    ("cogvideo", "Zhipu"),
    ("doubao", "ByteDance"),
    ("jimeng", "ByteDance"),
    ("seed", "ByteDance"),
    ("ernie", "Baidu"),
    ("hunyuan", "Tencent"),
    ("spark", "iFlytek"),
    ("minimax", "MiniMax"),
    ("abab", "MiniMax"),
    ("step-", "StepFun"),
    ("baichuan", "Baichuan"),
    ("yi-", "01.AI"),
    ("kling", "Kuaishou"),
    ("360", "Ai360"),
    // ---- 欧美其余 ----
    ("grok", "xAI"),
    ("llama", "Meta"),
    ("mistral", "Mistral"),
    ("mixtral", "Mistral"),
    ("codestral", "Mistral"),
    ("magistral", "Mistral"),
    ("devstral", "Mistral"),
    ("pixtral", "Mistral"),
    ("ministral", "Mistral"),
    ("command", "Cohere"),
    ("rerank", "Cohere"),
    ("embed-english", "Cohere"),
    ("embed-multilingual", "Cohere"),
    ("cohere.", "Cohere"),
    ("jina", "Jina"),
    ("amazon.", "Amazon"),
    ("nova-", "Amazon"),
    ("titan", "Amazon"),
    ("stability.", "Stability"),
    ("stable-diffusion", "Stability"),
    ("sd3", "Stability"),
    ("ai21.", "AI21"),
    ("jamba", "AI21"),
    ("j2-", "AI21"),
    ("nvidia.", "NVIDIA"),
    ("nemotron", "NVIDIA"),
    ("writer.", "Writer"),
    ("palmyra", "Writer"),
    ("twelvelabs.", "TwelveLabs"),
    ("perplexity", "Perplexity"),
    ("sonar", "Perplexity"),
    ("@cf/", "Cloudflare"),
    ("vidu", "Vidu"),
    ("luma", "Luma"),
    ("recraft", "Recraft"),
    ("flux", "BlackForestLabs"),
];

/// 按模型名推断供应商；无匹配返回 None（长尾/自建模型交管理员填写，不硬猜）。
///
/// 取**最长**匹配：`chatglm` 同时含 `glm`，`text-embedding` 同时含 `tts-` 之外的短规则，
/// 若按声明顺序首次匹配，加规则时极易踩到顺序坑。
#[must_use]
pub fn classify(model_name: &str) -> Option<&'static str> {
    let lower = model_name.trim().to_ascii_lowercase();
    // 剥离 Bedrock/Vertex 区域前缀（可能叠加，如 "us.amazon." 只有一层但留出余量）
    let name = REGION_PREFIXES
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .unwrap_or(&lower);
    RULES
        .iter()
        .filter(|(needle, _)| name.starts_with(needle) || name.contains(needle))
        .max_by_key(|(needle, _)| needle.len())
        .map(|(_, vendor)| *vendor)
}

#[cfg(test)]
mod tests {
    use super::classify;

    #[test]
    fn classifies_current_generation_models() {
        for (model, vendor) in [
            ("gpt-5.1", "OpenAI"),
            ("gpt-4o-audio-preview", "OpenAI"),
            ("o4-mini", "OpenAI"),
            ("codex-mini-latest", "OpenAI"),
            ("claude-opus-4-20250514", "Anthropic"),
            ("gemini-3-pro-preview", "Google"),
            ("deepseek-v3.2", "DeepSeek"),
            ("qwen3-max", "Alibaba"),
            ("kimi-k2-thinking", "Moonshot"),
            ("doubao-seed-1.6", "ByteDance"),
            ("glm-4.6", "Zhipu"),
            ("grok-4-fast", "xAI"),
            ("llama-4-maverick", "Meta"),
            ("mistral-medium-2508", "Mistral"),
            ("command-a-03-2025", "Cohere"),
            ("sonar-reasoning-pro", "Perplexity"),
            ("flux-pro-1.1", "BlackForestLabs"),
        ] {
            assert_eq!(classify(model), Some(vendor), "{model}");
        }
    }

    /// Bedrock/Vertex 区域前缀必须剥离——否则整个 Bedrock 生态都归类不了。
    #[test]
    fn strips_region_prefixes() {
        assert_eq!(classify("us.amazon.nova-2-pro-preview"), Some("Amazon"));
        assert_eq!(classify("eu.anthropic.claude-sonnet-4"), Some("Anthropic"));
        assert_eq!(classify("apac.amazon.nova-lite-v1:0"), Some("Amazon"));
        assert_eq!(classify("global.amazon.nova-2-lite-v1:0"), Some("Amazon"));
    }

    #[test]
    fn longest_match_wins_regardless_of_declaration_order() {
        assert_eq!(classify("chatglm-4"), Some("Zhipu"));
        assert_eq!(classify("text-embedding-3-large"), Some("OpenAI"));
        assert_eq!(classify("embed-multilingual-v3.0"), Some("Cohere"));
    }

    #[test]
    fn case_insensitive_and_trimmed() {
        assert_eq!(classify("  GPT-5  "), Some("OpenAI"));
        assert_eq!(classify("Claude-Opus-4"), Some("Anthropic"));
    }

    #[test]
    fn unknown_model_returns_none() {
        // 长尾/自建模型交由管理员显式填写，不硬猜
        assert_eq!(classify("my-private-model"), None);
        assert_eq!(classify(""), None);
        // 按参数量计价的档位不是模型标识，不该被归类
        assert_eq!(classify("together-ai-21.1b-41b"), None);
        assert_eq!(classify("fireworks-ai-56b-to-176b"), None);
    }

    /// 覆盖率纪律：以真实模型清单（LiteLLM 定价库快照）反查，纯模型名归类率须 ≥ 92%。
    ///
    /// 快照随仓库固定，避免用例依赖网络；更新快照时若覆盖率下降即说明有新厂商系列
    /// 未登记——这正是本用例要拦住的回归。
    #[test]
    fn coverage_over_real_model_catalog() {
        let catalog = include_str!("../tests/fixtures/model_catalog.txt");
        let names: Vec<&str> = catalog
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert!(names.len() > 300, "快照样本过小：{}", names.len());
        let missed: Vec<&&str> = names.iter().filter(|n| classify(n).is_none()).collect();
        let rate = 100 - missed.len() * 100 / names.len();
        // 当前实测 98%；残留为社区长尾（dolphin / daybreak 等）与非模型配置键，
        // 阈值设 95% 留出清单波动余量，同时仍能拦住"整个厂商系列漏登记"
        assert!(
            rate >= 95,
            "归类覆盖率 {rate}%（< 92%），未归类 {} 例，前 10：{:?}",
            missed.len(),
            &missed[..missed.len().min(10)]
        );
    }
}
