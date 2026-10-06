use super::*;
use crate::registry;

#[test]
fn authorization_dispatch_preserves_provider_state_and_pkce_rules() {
    let pkce = Pkce::from_bytes(&[7; 32]);
    let nonce = "independent-nonce";
    for (provider, format) in [
        ("anthropic_max", CodeFormat::CodeState),
        ("codex", CodeFormat::CallbackUrl),
    ] {
        let descriptor = registry::lookup(provider).unwrap();
        let hook = descriptor.account.unwrap();
        let rules = hook.capabilities().authorization.unwrap();
        let authorization = hook.authorize(&pkce, nonce).unwrap();
        let url = reqwest::Url::parse(&authorization.url).unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(params["code_challenge"], pkce.challenge);
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["redirect_uri"], authorization.redirect_uri);
        assert_eq!(params["state"], authorization.state);
        assert_eq!(rules.code_format, format);
        match format {
            CodeFormat::CodeState => {
                assert_eq!(authorization.state, pkce.verifier);
                assert_eq!(rules.access_token_prefix, Some("sk-ant-oat"));
                assert!(!rules.account_id_required);
                let profile = rules.import_profile.unwrap();
                let settings = serde_json::json!({"client_profile":profile});
                crate::profiles::validate_extensions(provider, &settings).unwrap();
            }
            CodeFormat::CallbackUrl => {
                assert_eq!(authorization.state, nonce);
                assert_ne!(authorization.state, pkce.verifier);
                assert!(rules.access_token_prefix.is_none());
                assert!(rules.account_id_required);
                assert!(rules.import_profile.is_none());
            }
        }
        // Catalog metadata must contain capabilities, never verifier/token material.
        let metadata = serde_json::to_string(&hook.capabilities()).unwrap();
        assert!(!metadata.contains(&pkce.verifier));
        assert!(!metadata.contains(nonce));
    }
    assert!(
        registry::lookup("openai")
            .unwrap()
            .account_capabilities()
            .authorization
            .is_none()
    );
}

#[test]
fn quota_only_plugins_do_not_acquire_authorization_behavior() {
    #[derive(Debug)]
    struct QuotaOnly;
    impl AccountHooks for QuotaOnly {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                quota: true,
                ..Default::default()
            }
        }
    }
    assert!(
        QuotaOnly
            .authorize(&Pkce::from_bytes(&[0; 32]), "nonce")
            .is_none()
    );
    assert!(QuotaOnly.capabilities().authorization.is_none());
}
