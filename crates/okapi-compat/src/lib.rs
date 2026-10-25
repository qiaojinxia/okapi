//! okapi-compat：适配规则引擎（设计见 `docs/compat-rules.md`）。
//!
//! 把「不同上游厂商 / 模型 / 下游终端之间的形状差异」变成带版本、可回放、可审计的**数据**，
//! 由一个无 IO 的纯引擎在流水线的固定挂载点执行：
//!
//! - [`validate`]：规则集（JSON 数组）→ 校验 + 编译，写入、加载、内置数据走同一个入口；
//! - [`resolve`]：厂商 ⊕ 渠道 ⊕ 护栏 分层合并，同 id 后者覆盖前者，`disabled` 关掉前者；
//! - [`EffectiveRules::apply_request`]：对发往上游的请求体执行请求阶段规则，返回改写后的体与轨迹。
//!
//! 边界（与设计文档一致）：
//! - **只做形状**：不碰金额。计费规则、价簿、预扣在别的 crate，规则看不到它们；
//! - **不执行脚本**：规则是封闭的操作集合（remove / keep_only / set / rename / copy / clamp /
//!   map_value / role_map），条件表达式（CEL）字段已预留但当前拒绝；
//! - **纯函数、幂等**：同输入同输出，规则应用两次等于一次；
//! - **失败开放**：规则出问题跳过并记录，只有标了 `required` 的规则失败关闭；
//! - **受保护路径唯一注册表**：[`protect`]，主链字段任何规则都写不进去。
//!
//! 当前只实现 `request` 阶段；`response` / `error` / `capability` 等阶段在 schema 里已有名字，
//! 校验器会明确拒绝，免得写了却静默不生效。

pub mod decimal;
pub mod glob;
pub mod pointer;
pub mod protect;
pub mod resolve;
pub mod rule;
pub mod select;
pub mod validate;
pub mod vendors;

mod exec;

pub use resolve::{
    EffectiveRule, EffectiveRules, Failure, Fired, Layer, LayerInfo, LayerInput, Outcome, Report,
    Shadowed, Skipped, resolve,
};
pub use rule::{Mode, Op, Phase, Rule, RuleDoc, SCHEMA_VERSION, Selector};
pub use select::{Context, ContextInput, DIALECTS, OPERATIONS, normalize_vendor};
pub use validate::{Issue, ValidRule, Validated, validate};
