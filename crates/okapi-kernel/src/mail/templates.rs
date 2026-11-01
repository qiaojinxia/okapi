//! 邮件模板（IMPLEMENTATION §8 第 5 条：按用户语言双套）。zh-CN / en 两套内置；
//! 纯文本 + 极简 HTML。邮件正文是内容不是 API 响应，不受"后端只回 error_code"约束。

use super::Outgoing;

/// 支持的模板语言；解析失败一律 en。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    ZhCn,
}

impl Lang {
    /// 请求体 `lang` 优先，其次 `Accept-Language`（取首个 `zh` 前缀即中文）。
    #[must_use]
    pub fn resolve(explicit: Option<&str>, accept_language: Option<&str>) -> Self {
        if let Some(l) = explicit.map(str::trim).filter(|l| !l.is_empty()) {
            return Self::parse(l);
        }
        accept_language
            .and_then(|al| al.split(',').next())
            .map(|first| first.split(';').next().unwrap_or(first).trim())
            .map_or(Self::En, Self::parse)
    }

    fn parse(tag: &str) -> Self {
        if tag.to_ascii_lowercase().starts_with("zh") {
            Self::ZhCn
        } else {
            Self::En
        }
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn wrap_html(title: &str, body_html: &str) -> String {
    format!(
        "<!doctype html><html><body style=\"font-family:-apple-system,Segoe UI,Helvetica,Arial,sans-serif;\
line-height:1.6;color:#1f2937;max-width:560px;margin:0 auto;padding:24px\">\
<h2 style=\"font-size:18px;margin:0 0 16px\">{title}</h2>{body_html}</body></html>"
    )
}

/// 注册邮箱验证码。
#[must_use]
pub fn verification_code(lang: Lang, site: &str, to: &str, code: &str, ttl_min: u32) -> Outgoing {
    let site_h = escape_html(site);
    let code_h = escape_html(code);
    let (subject, text, html) = match lang {
        Lang::ZhCn => (
            format!("[{site}] 邮箱验证码 {code}"),
            format!(
                "您正在注册 {site}。\n\n验证码：{code}\n\n{ttl_min} 分钟内有效。若非本人操作请忽略本邮件。"
            ),
            wrap_html(
                &format!("{site_h} 邮箱验证"),
                &format!(
                    "<p>您正在注册 {site_h}。</p>\
<p style=\"font-size:28px;letter-spacing:6px;font-weight:700;margin:16px 0\">{code_h}</p>\
<p style=\"color:#6b7280\">{ttl_min} 分钟内有效。若非本人操作请忽略本邮件。</p>"
                ),
            ),
        ),
        Lang::En => (
            format!("[{site}] Your verification code {code}"),
            format!(
                "You are signing up for {site}.\n\nVerification code: {code}\n\nIt expires in {ttl_min} minutes. If you did not request this, ignore this email."
            ),
            wrap_html(
                &format!("Verify your email for {site_h}"),
                &format!(
                    "<p>You are signing up for {site_h}.</p>\
<p style=\"font-size:28px;letter-spacing:6px;font-weight:700;margin:16px 0\">{code_h}</p>\
<p style=\"color:#6b7280\">It expires in {ttl_min} minutes. If you did not request this, ignore this email.</p>"
                ),
            ),
        ),
    };
    Outgoing {
        to: to.to_owned(),
        subject,
        text,
        html: Some(html),
    }
}

/// 找回密码链接。
#[must_use]
pub fn password_reset(lang: Lang, site: &str, to: &str, link: &str, ttl_min: u32) -> Outgoing {
    let site_h = escape_html(site);
    let link_h = escape_html(link);
    let (subject, text, html) = match lang {
        Lang::ZhCn => (
            format!("[{site}] 重置密码"),
            format!(
                "我们收到了重置 {site} 账户密码的请求。\n\n打开以下链接设置新密码（{ttl_min} 分钟内有效）：\n{link}\n\n若非本人操作请忽略本邮件，密码不会改变。"
            ),
            wrap_html(
                &format!("重置 {site_h} 密码"),
                &format!(
                    "<p>我们收到了重置您 {site_h} 账户密码的请求。</p>\
<p><a href=\"{link_h}\" style=\"display:inline-block;padding:10px 18px;background:#111827;color:#fff;\
border-radius:6px;text-decoration:none\">设置新密码</a></p>\
<p style=\"color:#6b7280\">链接 {ttl_min} 分钟内有效。按钮无法点击时复制此地址：<br>{link_h}</p>\
<p style=\"color:#6b7280\">若非本人操作请忽略本邮件，密码不会改变。</p>"
                ),
            ),
        ),
        Lang::En => (
            format!("[{site}] Reset your password"),
            format!(
                "We received a request to reset the password of your {site} account.\n\nOpen this link to set a new password (valid for {ttl_min} minutes):\n{link}\n\nIf you did not request this, ignore this email; your password will not change."
            ),
            wrap_html(
                &format!("Reset your {site_h} password"),
                &format!(
                    "<p>We received a request to reset the password of your {site_h} account.</p>\
<p><a href=\"{link_h}\" style=\"display:inline-block;padding:10px 18px;background:#111827;color:#fff;\
border-radius:6px;text-decoration:none\">Set a new password</a></p>\
<p style=\"color:#6b7280\">The link is valid for {ttl_min} minutes. If the button does not work, copy this address:<br>{link_h}</p>\
<p style=\"color:#6b7280\">If you did not request this, ignore this email; your password will not change.</p>"
                ),
            ),
        ),
    };
    Outgoing {
        to: to.to_owned(),
        subject,
        text,
        html: Some(html),
    }
}

/// 事件通知（worker notify email 通道）：主题 `[site] event`，正文为 payload JSON。
#[must_use]
pub fn event_notice(
    lang: Lang,
    site: &str,
    to: &str,
    event: &str,
    at: &str,
    payload: &serde_json::Value,
) -> Outgoing {
    let pretty = serde_json::to_string_pretty(payload).unwrap_or_default();
    let (subject, intro) = match lang {
        Lang::ZhCn => (
            format!("[{site}] 事件通知：{event}"),
            format!("{site} 于 {at} 触发事件 {event}，详情："),
        ),
        Lang::En => (
            format!("[{site}] Event: {event}"),
            format!("{site} raised event {event} at {at}. Details:"),
        ),
    };
    let text = format!("{intro}\n\n{pretty}\n");
    let html = wrap_html(
        &escape_html(&subject),
        &format!(
            "<p>{}</p><pre style=\"background:#f3f4f6;padding:12px;border-radius:6px;overflow:auto\">{}</pre>",
            escape_html(&intro),
            escape_html(&pretty)
        ),
    );
    Outgoing {
        to: to.to_owned(),
        subject,
        text,
        html: Some(html),
    }
}

/// 配置页测试信。
#[must_use]
pub fn test_message(lang: Lang, site: &str, to: &str) -> Outgoing {
    let (subject, text) = match lang {
        Lang::ZhCn => (
            format!("[{site}] SMTP 测试邮件"),
            format!("这是一封来自 {site} 的测试邮件。收到即表示 SMTP 配置可用。"),
        ),
        Lang::En => (
            format!("[{site}] SMTP test message"),
            format!(
                "This is a test message from {site}. Receiving it means the SMTP settings work."
            ),
        ),
    };
    Outgoing {
        to: to.to_owned(),
        subject,
        text,
        html: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_resolution_prefers_explicit_then_accept_language() {
        assert_eq!(Lang::resolve(Some("zh-CN"), None), Lang::ZhCn);
        assert_eq!(Lang::resolve(Some("en-US"), Some("zh-CN")), Lang::En);
        assert_eq!(
            Lang::resolve(None, Some("zh-CN,zh;q=0.9,en;q=0.8")),
            Lang::ZhCn
        );
        assert_eq!(Lang::resolve(None, Some("fr-FR,en;q=0.5")), Lang::En);
        assert_eq!(Lang::resolve(Some("  "), None), Lang::En, "空串回缺省");
        assert_eq!(Lang::resolve(None, None), Lang::En);
    }

    #[test]
    fn templates_carry_code_and_link_and_escape_html() {
        let m = verification_code(Lang::ZhCn, "Okapi", "a@b.c", "123456", 10);
        assert!(m.subject.contains("123456"));
        assert!(m.text.contains("123456") && m.text.contains("10 分钟"));
        assert!(m.html.as_deref().unwrap().contains("123456"));

        let link = "https://x.io/reset-password?token=abc&x=<1>";
        let m = password_reset(Lang::En, "Okapi", "a@b.c", link, 30);
        assert!(m.text.contains(link), "纯文本原样给链接");
        let html = m.html.unwrap();
        assert!(html.contains("&amp;x=&lt;1&gt;"), "HTML 里转义：{html}");
        assert!(!html.contains("<1>"));

        let m = event_notice(
            Lang::En,
            "Okapi",
            "ops@x",
            "drift",
            "2026-09-05T00:00:00Z",
            &serde_json::json!({"k": "<v>"}),
        );
        assert!(m.subject.ends_with("Event: drift"));
        assert!(m.html.unwrap().contains("&lt;v&gt;"));
    }
}
