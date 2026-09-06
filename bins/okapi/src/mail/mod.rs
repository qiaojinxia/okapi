//! 邮件出口（IMPLEMENTATION §11.27）：`settings.smtp` → lettre SMTP 客户端。
//!
//! 三个使用面共用：注册邮箱验证码 / 找回密码（console `auth_web`）、事件通知（worker `notify`）、
//! 配置页测试发送（console `admin`）。低频面，每次发送新建连接（不开 lettre pool）；
//! 失败返回错误由调用方决定是回 5xx 还是只打日志。

pub mod templates;

use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport as _, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const SETTING_KEY: &str = "smtp";
const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// 连接安全：starttls（587 常见）/ tls（465 隐式 TLS）/ none（内网中继 / 测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Security {
    #[default]
    Starttls,
    Tls,
    None,
}

/// `settings.smtp` 形状。`host` 空 = 未配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SmtpConfig {
    pub host: String,
    /// 0 = 按 security 取缺省端口（starttls 587 / tls 465 / none 25）。
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub password: String,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
}

impl SmtpConfig {
    /// 从 settings 值解析；形状不对视为未配置（配错 JSON 不该让注册整个报 500）。
    #[must_use]
    pub fn from_value(value: Option<&serde_json::Value>) -> Option<Self> {
        let cfg: Self = serde_json::from_value(value?.clone()).ok()?;
        cfg.is_configured().then_some(cfg)
    }

    #[must_use]
    pub fn is_configured(&self) -> bool {
        !self.host.trim().is_empty() && !self.from_address.trim().is_empty()
    }

    #[must_use]
    pub fn effective_port(&self) -> u16 {
        if self.port != 0 {
            return self.port;
        }
        match self.security {
            Security::Starttls => 587,
            Security::Tls => 465,
            Security::None => 25,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("smtp_not_configured")]
    NotConfigured,
    #[error("invalid address: {0}")]
    Address(String),
    #[error("build: {0}")]
    Build(#[from] lettre::error::Error),
    #[error("smtp: {0}")]
    Smtp(#[from] lettre::transport::smtp::Error),
}

/// 一封待发邮件（纯文本必有；HTML 可选，有则发 multipart/alternative）。
pub struct Outgoing {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
}

pub struct Mailer {
    cfg: SmtpConfig,
}

impl Mailer {
    /// 从 settings 缓存读取配置；未配置返回 `NotConfigured`。
    pub async fn from_state(state: &crate::gateway::state::AppState) -> Result<Self, MailError> {
        let value = state.setting_cached(SETTING_KEY).await;
        Self::from_config(SmtpConfig::from_value(value.as_ref().as_ref()))
    }

    /// 直读 PG（worker 无 AppState 时用）。
    pub async fn from_pg(pg: &sqlx::PgPool) -> Result<Self, MailError> {
        let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'smtp'"#)
            .fetch_optional(pg)
            .await
            .ok()
            .flatten();
        Self::from_config(SmtpConfig::from_value(value.as_ref()))
    }

    pub fn from_config(cfg: Option<SmtpConfig>) -> Result<Self, MailError> {
        cfg.map(|cfg| Self { cfg }).ok_or(MailError::NotConfigured)
    }

    #[must_use]
    pub fn config(&self) -> &SmtpConfig {
        &self.cfg
    }

    fn transport(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>, MailError> {
        let host = self.cfg.host.trim();
        let builder = match self.cfg.security {
            Security::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?,
            Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host)?,
            Security::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
        };
        let mut builder = builder
            .port(self.cfg.effective_port())
            .timeout(Some(SEND_TIMEOUT));
        if !self.cfg.username.is_empty() {
            builder = builder.credentials(Credentials::new(
                self.cfg.username.clone(),
                self.cfg.password.clone(),
            ));
        }
        Ok(builder.build())
    }

    fn sender_mailbox(&self) -> Result<Mailbox, MailError> {
        let addr = self.cfg.from_address.trim();
        let name = self.cfg.from_name.trim();
        let raw = if name.is_empty() {
            addr.to_owned()
        } else {
            // 显示名含引号 / 非 ASCII 时交给 lettre 的 Mailbox 解析器处理编码
            format!("{name} <{addr}>")
        };
        raw.parse::<Mailbox>()
            .map_err(|_| MailError::Address(addr.to_owned()))
    }

    pub async fn send(&self, mail: Outgoing) -> Result<(), MailError> {
        let to: Mailbox = mail
            .to
            .trim()
            .parse()
            .map_err(|_| MailError::Address(mail.to.clone()))?;
        let mut builder = Message::builder()
            .from(self.sender_mailbox()?)
            .to(to)
            .subject(mail.subject);
        if let Some(reply_to) = self.cfg.reply_to.as_deref().map(str::trim)
            && !reply_to.is_empty()
            && let Ok(mb) = reply_to.parse::<Mailbox>()
        {
            builder = builder.reply_to(mb);
        }
        let message = match mail.html {
            Some(html) => builder.multipart(MultiPart::alternative_plain_html(mail.text, html))?,
            None => builder
                .header(lettre::message::header::ContentType::TEXT_PLAIN)
                .body(mail.text)?,
        };
        self.transport()?.send(message).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_parsing_and_defaults() {
        assert!(SmtpConfig::from_value(None).is_none());
        assert!(
            SmtpConfig::from_value(Some(&json!({"host": "", "from_address": "a@b"}))).is_none(),
            "host 空 = 未配置"
        );
        assert!(
            SmtpConfig::from_value(Some(&json!({"host": "smtp.x", "from_address": ""}))).is_none(),
            "from 空 = 未配置"
        );
        assert!(
            SmtpConfig::from_value(Some(&json!("garbage"))).is_none(),
            "形状不对当未配置"
        );
        let cfg = SmtpConfig::from_value(Some(&json!({
            "host": "smtp.x", "from_address": "no-reply@x", "security": "tls"
        })))
        .unwrap();
        assert_eq!(cfg.effective_port(), 465);
        assert_eq!(
            SmtpConfig {
                security: Security::Starttls,
                ..Default::default()
            }
            .effective_port(),
            587
        );
        assert_eq!(
            SmtpConfig {
                port: 2525,
                security: Security::None,
                ..Default::default()
            }
            .effective_port(),
            2525,
            "显式端口优先"
        );
    }

    #[test]
    fn sender_mailbox_handles_display_name() {
        let mailer = Mailer::from_config(Some(SmtpConfig {
            host: "smtp.x".into(),
            from_address: "no-reply@x.io".into(),
            from_name: "Okapi 网关".into(),
            ..Default::default()
        }))
        .unwrap();
        let mb = mailer.sender_mailbox().unwrap();
        assert_eq!(mb.email.to_string(), "no-reply@x.io");
        assert_eq!(mb.name.as_deref(), Some("Okapi 网关"));
    }
}
