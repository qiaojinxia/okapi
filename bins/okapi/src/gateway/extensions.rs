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

pub fn outbound_with_client(cand: &ChannelCandidate, client: &[(String, String)]) -> Outbound {
    let mut outbound = super::openai_dialect::outbound(cand);
    outbound.context.client_headers = client.to_vec();
    if registry::lookup(&cand.provider).is_some_and(|adapter| adapter.forward_client_identity) {
        outbound.extra_headers.extend(client.iter().cloned());
    }
    outbound
}
