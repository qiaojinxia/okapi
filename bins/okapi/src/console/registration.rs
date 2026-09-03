//! 注册策略（IMPLEMENTATION §11.16）：`settings.registration_policy` 一把结构化开关。
//!
//! new-api 运营设置里最常用的一组——`RegisterEnabled`、邮箱域名限制、`QuotaForNewUser`、
//! 邀请人 / 被邀请人奖励；Sub2API 的注册开关与邮箱域名黑名单（#6485）同类。此前注册
//! 恒开、无域名限制、新用户零额度：想做邀请制或封停注册只能改代码。
//!
//! 策略只在 `/auth/register` 一处生效，读取走 60s 进程缓存（与其它 settings 同一取舍）；
//! 公开端点 `GET /api/registration` 只透出登录页渲染需要的三个字段。

use crate::gateway::error::AppError;
use crate::gateway::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use okapi_domain::Money;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SETTING_KEY: &str = "registration_policy";

/// open = 任何人可注册；invite_only = 必须带有效邀请码；closed = 关闭注册。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RegisterMode {
    #[default]
    Open,
    InviteOnly,
    Closed,
}

/// any = 不限；allowlist = 只允许清单内域名；blocklist = 拒绝清单内域名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DomainMode {
    #[default]
    Any,
    Allowlist,
    Blocklist,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RegistrationPolicy {
    pub mode: RegisterMode,
    pub email_domain_mode: DomainMode,
    /// 小写域名，如 `gmail.com`；支持 `*.edu.cn` 通配后缀。
    pub email_domains: Vec<String>,
    /// 新用户注册即送（micro-USD；0 = 不送）。
    pub new_user_credit_micro: i64,
    /// 被邀请人额外奖励（带有效邀请码注册时叠加在新用户赠送之上）。
    pub invitee_credit_micro: i64,
    /// 邀请人奖励（被邀请人注册成功即入账；充值返利另见 aff_percent_bp）。
    pub inviter_credit_micro: i64,
}

impl RegistrationPolicy {
    /// 从 settings 缓存读取；未配置或形状不对一律回缺省（开放注册、无限制、零赠送）——
    /// 配错 JSON 不该把注册整个关掉。
    pub async fn load(state: &AppState) -> Self {
        state
            .setting_cached(SETTING_KEY)
            .await
            .as_ref()
            .as_ref()
            .and_then(|v| serde_json::from_value::<Self>(v.clone()).ok())
            .unwrap_or_default()
    }

    /// 邮箱域名是否放行。清单条目 `*.edu.cn` 匹配任意子域，其余精确匹配（大小写不敏感）。
    #[must_use]
    pub fn domain_allowed(&self, email: &str) -> bool {
        let Some(domain) = email.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()) else {
            return false;
        };
        let listed = self.email_domains.iter().any(|entry| {
            let entry = entry.trim().to_ascii_lowercase();
            if let Some(suffix) = entry.strip_prefix("*.") {
                domain == suffix || domain.ends_with(&format!(".{suffix}"))
            } else {
                domain == entry
            }
        });
        match self.email_domain_mode {
            DomainMode::Any => true,
            DomainMode::Allowlist => listed,
            DomainMode::Blocklist => !listed,
        }
    }
}

/// 注册前置校验（限流之后、写库之前）：关闭 / 邮箱域名。邀请码另在调用方按模式判定。
pub fn check(policy: &RegistrationPolicy, email: &str) -> Result<(), AppError> {
    if policy.mode == RegisterMode::Closed {
        return Err(AppError::new(StatusCode::FORBIDDEN, "registration_closed"));
    }
    if !policy.domain_allowed(email) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "email_domain_rejected",
        ));
    }
    Ok(())
}

/// 邀请码解析：存在且不是自己 → Some(inviter_id)。invite_only 模式下 None 即拒绝。
pub async fn resolve_inviter(
    state: &AppState,
    aff_code: Option<&str>,
) -> Result<Option<i64>, AppError> {
    let Some(code) = aff_code.map(str::trim).filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    let id = sqlx::query_scalar!(
        r#"SELECT id FROM users WHERE aff_code = $1 AND deleted_at IS NULL AND status = 1"#,
        code
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(okapi_store::StoreError::from)?;
    Ok(id)
}

/// 注册成功后的入账：新用户赠送（+ 被邀请人奖励）与邀请人奖励。
/// 走与管理员入账相同的 Redis 账本 + billing_events 双写；actor 标 `system:register`，
/// 事件 payload 带 tag，资金流入概要里按 adjust 正向归到"送出"桶，不会被误读成充值。
pub async fn grant_credits(
    state: &AppState,
    policy: &RegistrationPolicy,
    user_id: i64,
    inviter_id: Option<i64>,
) {
    let mut gift = policy.new_user_credit_micro.max(0);
    if inviter_id.is_some() {
        gift = gift.saturating_add(policy.invitee_credit_micro.max(0));
    }
    if gift > 0 {
        credit(
            state,
            user_id,
            gift,
            json!({ "tags": ["new_user_gift"], "invited": inviter_id.is_some() }),
        )
        .await;
    }
    if let Some(inviter) = inviter_id
        && policy.inviter_credit_micro > 0
    {
        credit(
            state,
            inviter,
            policy.inviter_credit_micro,
            json!({ "tags": ["invite_reward"], "invitee_user_id": user_id }),
        )
        .await;
    }
}

async fn credit(state: &AppState, user_id: i64, micro: i64, payload: Value) {
    let amount = Money::from_micros(micro);
    if let Err(err) = state.ledger.credit(user_id, amount).await {
        tracing::error!(user_id, error = %err, "注册赠送入账失败（Redis）");
        return;
    }
    if let Err(err) = okapi_ledger::pg::record_credit(
        &state.pg,
        user_id,
        amount,
        "adjust",
        "system:register",
        payload,
    )
    .await
    {
        tracing::error!(user_id, error = %err, "注册赠送事件落库失败（余额已入 Redis，对账可检出）");
    }
}

/// GET /api/registration：登录页据此决定注册页签怎么画——关闭时不摆一个必然失败的表单，
/// 邀请制时把邀请码变成必填，有赠送时把"注册即送 $X"写出来（new-api 注册页同有）。
pub async fn public_policy(State(state): State<AppState>) -> Json<Value> {
    let policy = RegistrationPolicy::load(&state).await;
    Json(json!({
        "mode": policy.mode,
        "new_user_credit_micro": policy.new_user_credit_micro.max(0),
        "invitee_credit_micro": policy.invitee_credit_micro.max(0),
        // 白名单模式把允许的域名告诉用户（省一次失败）；黑名单不透出，避免给刷号者对照表
        "allowed_domains": if policy.email_domain_mode == DomainMode::Allowlist {
            policy.email_domains
        } else {
            Vec::new()
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: DomainMode, domains: &[&str]) -> RegistrationPolicy {
        RegistrationPolicy {
            email_domain_mode: mode,
            email_domains: domains.iter().map(|d| (*d).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn domain_matching_exact_wildcard_and_case() {
        let allow = policy(DomainMode::Allowlist, &["Gmail.com", "*.edu.cn"]);
        assert!(allow.domain_allowed("a@gmail.com"));
        assert!(allow.domain_allowed("a@GMAIL.COM"));
        assert!(
            allow.domain_allowed("a@mail.tsinghua.edu.cn"),
            "通配后缀匹配子域"
        );
        assert!(allow.domain_allowed("a@edu.cn"), "通配也匹配裸后缀");
        assert!(!allow.domain_allowed("a@outlook.com"));
        assert!(!allow.domain_allowed("no-at-sign"));

        let block = policy(DomainMode::Blocklist, &["tempmail.io"]);
        assert!(!block.domain_allowed("x@tempmail.io"));
        assert!(block.domain_allowed("x@gmail.com"));
        assert!(policy(DomainMode::Any, &[]).domain_allowed("x@anything.tld"));
    }

    #[test]
    fn malformed_setting_falls_back_to_open() {
        let bad: Result<RegistrationPolicy, _> = serde_json::from_value(json!({"mode": "banana"}));
        assert!(bad.is_err(), "非法枚举值应解析失败 → 调用方回缺省而非关站");
        let partial: RegistrationPolicy =
            serde_json::from_value(json!({"mode": "closed"})).unwrap();
        assert_eq!(partial.mode, RegisterMode::Closed);
        assert_eq!(partial.new_user_credit_micro, 0, "缺省字段补零");
    }
}
