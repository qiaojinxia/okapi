//! Provider capability defaults shared by storage decoding and executable adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeResponses {
    Unsupported,
    Optional { default: bool },
    Required,
}

impl NativeResponses {
    #[must_use]
    pub fn enabled(self, configured: Option<bool>) -> bool {
        match self {
            Self::Unsupported => false,
            Self::Optional { default } => configured.unwrap_or(default),
            Self::Required => true,
        }
    }
}

/// Protocol metadata has one owner, shared by storage and executable registrations.
/// Extending protocol defaults adds a catalog entry; no query or gateway branch changes.
#[derive(Clone, Copy, Debug)]
pub struct ProtocolDefaults {
    pub id: &'static str,
    pub native_responses: NativeResponses,
}

pub const OPENAI: ProtocolDefaults = ProtocolDefaults {
    id: "openai",
    native_responses: NativeResponses::Optional { default: true },
};
pub const OPENAI_COMPAT: ProtocolDefaults = ProtocolDefaults {
    id: "openai_compat",
    native_responses: NativeResponses::Optional { default: false },
};
pub const CODEX: ProtocolDefaults = ProtocolDefaults {
    id: "codex",
    native_responses: NativeResponses::Required,
};
pub const PROTOCOL_DEFAULTS: &[ProtocolDefaults] = &[OPENAI, OPENAI_COMPAT, CODEX];

#[must_use]
pub fn native_responses_for(provider: &str) -> NativeResponses {
    PROTOCOL_DEFAULTS
        .iter()
        .find(|entry| entry.id == provider)
        .map_or(NativeResponses::Unsupported, |entry| entry.native_responses)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_catalog_is_unique_and_unknown_ids_fail_closed() {
        let ids: std::collections::HashSet<_> =
            PROTOCOL_DEFAULTS.iter().map(|entry| entry.id).collect();
        assert_eq!(ids.len(), PROTOCOL_DEFAULTS.len());
        assert_eq!(
            native_responses_for("unregistered"),
            NativeResponses::Unsupported
        );
        assert!(native_responses_for(OPENAI.id).enabled(None));
        assert!(!native_responses_for(OPENAI_COMPAT.id).enabled(None));
        assert!(native_responses_for(CODEX.id).enabled(Some(false)));
    }
}
