//! Store-to-provider bridge. The gateway carries context; adapters own extension behavior.
use okapi_providers::{Outbound, profiles, registry};
use okapi_store::ChannelCandidate;

pub fn client_headers(headers: &axum::http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| profiles::is_client_header(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

/// `user_id` = 下游用户，模拟客户端派生会话 id 时用它隔开不同用户（见 `RequestContext::session_scope`）。
pub fn outbound_with_client(
    cand: &ChannelCandidate,
    client: &[(String, String)],
    user_id: Option<i64>,
) -> Outbound {
    let mut outbound = super::openai_dialect::outbound(cand);
    outbound.context.client_headers = client.to_vec();
    outbound.context.session_scope = user_id.map(|id| id.to_string());
    if registry::lookup(&cand.provider).is_some_and(|adapter| adapter.forward_client_identity) {
        outbound.extra_headers.extend(client.iter().cloned());
    }
    outbound
}
