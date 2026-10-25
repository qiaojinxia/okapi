//! Validated creation settings shared by API keys and account authorization plugins.
use super::admin::{self, PoolMemberReq};
use crate::gateway::{error::AppError, state::AppState};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default, Deserialize)]
pub struct Options {
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub max_concurrency: Option<i32>,
    #[serde(default)]
    pub pools: Option<Vec<PoolMemberReq>>,
    #[serde(default)]
    pub settings: Option<Value>,
    #[serde(default)]
    pub cost_milli: Option<i64>,
    /// 出口绑定（§11.41）；缺省 = 继承全局默认。OAuth 登录在换码前就按它选出口。
    #[serde(default)]
    pub egress: Option<okapi_store::egress::Binding>,
    /// 内置厂商（`builtin:deepseek` 或裸 id）；缺省 = 不套厂商层适配规则。
    #[serde(default)]
    pub vendor: Option<String>,
    /// 渠道层适配规则（取代原来的 settings.strip_request_fields / inject_request_fields）。
    #[serde(default)]
    pub compat: Option<super::compat::RulesInput>,
}

pub struct Prepared {
    pub priority: i32,
    pub max_concurrency: Option<i32>,
    pub pools: Vec<okapi_store::admin::PoolMember>,
    pub settings: Value,
    pub cost_milli: Option<i64>,
    /// 未校验可见性：调用方用 `egress::validate_binding` 按操作者范围再判。
    pub egress: okapi_store::egress::Binding,
    /// 带命名空间的内置厂商。
    pub vendor: Option<String>,
    /// 通过校验的初始渠道层规则与备注（空数组等同没有）。
    pub rules: Option<(Value, Option<String>)>,
}

/// 选了内置厂商时，渠道协议对应的上游方言（厂商端点表按它取地址）。
fn vendor_protocol(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" | "openai_compat" => Some("openai.chat"),
        "anthropic" => Some("anthropic.messages"),
        "gemini" => Some("gemini.generate"),
        _ => None,
    }
}

/// 渠道要连的地址：填了的 > 选定厂商为这个协议给的端点 > 协议的缺省地址（官方 openai / anthropic …）。
/// 都没有就拒绝——`openai_compat` 没有缺省，不能悄悄发往 OpenAI。厂商给的地址在这里**落成明确的值**，
/// 之后的路由只认渠道上存的地址，不依赖运行期的隐含缺省。选了适配档案（`custom:<slug>`）时取它叠加的
/// 内置厂商的端点（档案自己没有端点表）。
pub async fn endpoint<'a>(
    state: &AppState,
    provider: &str,
    configured: Option<&'a str>,
    vendor: Option<&str>,
) -> Result<&'a str, AppError> {
    let extended = match vendor.and_then(okapi_store::compat::profile_slug) {
        Some(slug) => okapi_store::compat::profile_by_slug(&state.pg, slug)
            .await?
            .and_then(|profile| profile.extends),
        None => None,
    };
    let vendor = extended.as_deref().or(vendor);
    let base = configured
        .map(str::trim)
        .filter(|base| !base.is_empty())
        .or_else(|| okapi_compat::vendors::endpoint_base(vendor?, vendor_protocol(provider)?))
        .or_else(|| {
            okapi_providers::registry::lookup(provider).and_then(|adapter| adapter.default_base)
        })
        .ok_or_else(|| AppError::bad_request().with_param("api_base"))?;
    super::ssrf::validate_api_base(state, base).await?;
    Ok(base)
}

pub async fn prepare(
    state: &AppState,
    provider: &str,
    options: Options,
    scope: okapi_store::auth::PermScope,
    actor: &okapi_store::AuthedKey,
) -> Result<Prepared, AppError> {
    admin::ensure_cost_milli(options.cost_milli)?;
    admin::ensure_max_concurrency(options.max_concurrency)?;
    admin::validate_channel_settings(state, provider, options.settings.as_ref()).await?;
    let pools = match options.pools {
        // own 范围不能选池（见 admin::ensure_pool_scope）：显式带了池就拒，不带就建成孤儿
        Some(members) if scope != okapi_store::auth::PermScope::All => {
            if !members.is_empty() {
                admin::ensure_pool_scope(scope)?;
            }
            Vec::new()
        }
        None if scope != okapi_store::auth::PermScope::All => Vec::new(),
        Some(members) => admin::normalize_members(state, members).await?,
        None => vec![okapi_store::admin::PoolMember {
            pool_code: okapi_store::channels::DEFAULT_POOL.into(),
            priority_override: None,
            weight_override: None,
        }],
    };
    let settings = options.settings.unwrap_or_else(|| json!({}));
    let vendor = match options.vendor.as_deref() {
        Some(raw) => Some(super::compat::normalize_vendor(state, raw).await?),
        None => None,
    };
    let rules = match &options.compat {
        Some(input) => {
            let prepared = super::compat::prepare_rules(state, actor, None, input).await?;
            // 落库的是校验过、去过空白的备注（prepare_rules 已确认不超长）
            Some((prepared.rules, input.note()?.map(str::to_owned)))
        }
        None => None,
    };
    Ok(Prepared {
        vendor,
        rules,
        priority: options.priority,
        max_concurrency: options.max_concurrency,
        pools,
        settings,
        cost_milli: options.cost_milli,
        egress: options
            .egress
            .unwrap_or(okapi_store::egress::Binding::Inherit),
    })
}

impl Prepared {
    /// 创建时一并落库的厂商与初始规则。
    pub fn compat_create(&self, actor: i64) -> okapi_store::provision::CompatCreate<'_> {
        okapi_store::provision::CompatCreate {
            vendor: self.vendor.as_deref(),
            rules: self.rules.as_ref().map(|(rules, _)| rules),
            note: self.rules.as_ref().and_then(|(_, note)| note.as_deref()),
            actor: Some(actor),
        }
    }
}
