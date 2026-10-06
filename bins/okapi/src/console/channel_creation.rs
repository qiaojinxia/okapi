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
    #[serde(default)]
    pub data_retention: Option<String>,
    /// 出口绑定（§11.41）；缺省 = 继承全局默认。OAuth 登录在换码前就按它选出口。
    #[serde(default)]
    pub egress: Option<okapi_store::egress::Binding>,
}

pub struct Prepared {
    pub priority: i32,
    pub max_concurrency: Option<i32>,
    pub pools: Vec<okapi_store::admin::PoolMember>,
    pub settings: Value,
    pub cost_milli: Option<i64>,
    /// 未校验可见性：调用方用 `egress::validate_binding` 按操作者范围再判。
    pub egress: okapi_store::egress::Binding,
}

pub async fn endpoint<'a>(
    state: &AppState,
    provider: &str,
    configured: Option<&'a str>,
) -> Result<&'a str, AppError> {
    let base = configured
        .map(str::trim)
        .filter(|base| !base.is_empty())
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
) -> Result<Prepared, AppError> {
    admin::ensure_cost_milli(options.cost_milli)?;
    admin::ensure_max_concurrency(options.max_concurrency)?;
    admin::ensure_data_retention(options.data_retention.as_deref())?;
    admin::validate_channel_settings(state, provider, options.settings.as_ref()).await?;
    let pools = match options.pools {
        Some(members) => admin::normalize_members(state, members).await?,
        None => vec![okapi_store::admin::PoolMember {
            pool_code: okapi_store::channels::DEFAULT_POOL.into(),
            priority_override: None,
            weight_override: None,
        }],
    };
    let mut settings = options.settings.unwrap_or_else(|| json!({}));
    if let Some(retention) = options.data_retention {
        if retention.is_empty() {
            settings.as_object_mut().unwrap().remove("data_retention");
        } else {
            settings["data_retention"] = json!(retention);
        }
    }
    Ok(Prepared {
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
