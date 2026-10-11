//! Normalize administrator credential imports once; forwarding keeps the shared manager.
use crate::gateway::error::AppError;
use okapi_providers::registry::{self, CredentialKind};
use okapi_store::credential::OAuthCredential;

pub fn normalize(provider: &str, plaintext: &str) -> Result<String, AppError> {
    let adapter =
        registry::lookup(provider).ok_or_else(|| AppError::bad_request().with_param("provider"))?;
    let CredentialKind::OAuth(_) = adapter.credential else {
        return Ok(plaintext.to_owned());
    };
    let rules = adapter
        .account_capabilities()
        .authorization
        .ok_or_else(|| AppError::bad_request().with_param("provider"))?;
    let input = plaintext.trim();
    let credential = if let Some(credential) = OAuthCredential::parse(input) {
        credential
    } else if rules
        .access_token_prefix
        .is_some_and(|prefix| input.starts_with(prefix))
    {
        OAuthCredential {
            access_token: input.into(),
            refresh_token: String::new(),
            expires_at: 0,
            account_id: None,
            account_label: None,
            scope: None,
        }
    } else {
        return Err(AppError::bad_request().with_param("credential"));
    };
    if !valid_token(&credential.access_token)
        || credential.can_refresh() && !valid_token(&credential.refresh_token)
        || credential.expires_at < 0
        || credential.can_refresh() && credential.expires_at == 0
        || rules.account_id_required
            && credential
                .account_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
    {
        return Err(AppError::bad_request().with_param("credential"));
    }
    Ok(credential.to_plaintext())
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_access_token_without_inventing_refresh_or_expiry() {
        let text = normalize("anthropic_max", "  sk-ant-oat01-example  ").unwrap();
        let credential = OAuthCredential::parse(&text).unwrap();
        assert_eq!(credential.access_token, "sk-ant-oat01-example");
        assert!(!credential.can_refresh());
        assert_eq!(credential.expires_at, 0);
        let exported = r#"{"kind":"oauth","access_token":"sk-ant-oat01-example"}"#;
        assert_eq!(normalize("anthropic_max", exported).unwrap(), text);
        assert_eq!(
            normalize("anthropic", "sk-ant-api03-key").unwrap(),
            "sk-ant-api03-key"
        );
    }

    #[test]
    fn rejects_wrong_token_type_and_incomplete_refreshable_credentials() {
        for input in [
            "",
            "sk-ant-api03-key",
            "sk-ant-ort01-refresh",
            "sk-ant-oat01-with space",
            r#"{"kind":"oauth","access_token":""}"#,
            r#"{"kind":"oauth","access_token":"access","refresh_token":"refresh"}"#,
            r#"{"kind":"oauth","access_token":"access","expires_at":-1}"#,
        ] {
            assert!(normalize("anthropic_max", input).is_err(), "{input}");
        }
        assert!(normalize("codex", "sk-ant-oat01-example").is_err());
        assert!(normalize("codex", r#"{"kind":"oauth","access_token":"access"}"#).is_err());
        let complete = r#"{"kind":"oauth","access_token":"access","refresh_token":"refresh","expires_at":2000000000,"account_id":"account"}"#;
        assert!(normalize("codex", complete).is_ok());
        assert!(normalize("anthropic_max", complete).is_ok());
    }
}
