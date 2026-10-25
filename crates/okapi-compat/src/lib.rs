//! okapi-compat：适配规则引擎（设计见 `docs/compat-rules.md`）。
//!
//! 把「不同上游厂商 / 模型 / 下游终端之间的形状差异」变成带版本、可回放、可审计的**数据**，
//! 由一个无 IO 的纯引擎在流水线的固定挂载点执行：
//!
//! - [`validate`]：规则集（JSON 数组）→ 校验 + 编译，写入、加载、内置数据走同一个入口；
//! - [`resolve`]：厂商 ⊕ 渠道 ⊕ 护栏 分层合并，同 id 后者覆盖前者，`disabled` 关掉前者；
//! - [`EffectiveRules::apply_request`]：对发往上游的请求体执行请求阶段规则，返回改写后的体与轨迹；
//!   [`EffectiveRules::apply_response`] / [`EffectiveRules::react`] / [`EffectiveRules::capabilities`]
//!   分别是响应、错误、能力阶段的入口。
//!
//! 边界（与设计文档一致）：
//! - **只做形状**：不碰金额。计费规则、价簿、预扣在别的 crate，规则看不到它们；
//! - **不执行脚本**：规则是封闭的操作集合（见 [`OP_NAMES`]），用量只能做整数加减（[`expr`]），
//!   条件表达式（CEL）字段已预留但当前拒绝；
//! - **纯函数、幂等**：同输入同输出，规则应用两次等于一次；
//! - **失败开放**：规则出问题跳过并记录，只有标了 `required` 的规则失败关闭；
//! - **受保护路径唯一注册表**：[`protect`]，主链字段任何规则都写不进去。
//!
//! 阶段：`request` / `response`（OpenAI chat 方言上游）/ `error`（不可重试的上游 HTTP 错误）/
//! `capability`（候选加载时合并的能力事实）已启用；`request_pre` / `wire` 仍是预留，
//! 校验器会明确拒绝，免得写了却静默不生效。

pub mod catalog;
pub mod decimal;
pub mod expr;
pub mod glob;
pub mod pointer;
pub mod probe;
pub mod protect;
pub mod resolve;
pub mod rule;
pub mod select;
pub mod validate;
pub mod vendors;

mod exec;

pub use resolve::{
    Action, EffectiveRule, EffectiveRules, Failure, Fired, Layer, LayerInfo, LayerInput, Mutation,
    Outcome, Reaction, Report, Shadowed, Skipped, resolve,
};
pub use rule::{
    CAPABILITY_FACTS, ErrorWhen, Mode, OP_NAMES, Op, Phase, Rule, RuleDoc, SCHEMA_VERSION, Selector,
};
pub use select::{Context, ContextInput, DIALECTS, OPERATIONS, normalize_vendor};
pub use validate::{Issue, ValidRule, Validated, validate};
