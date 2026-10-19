//! Store-to-provider bridge. The gateway carries context; adapters own extension behavior.
use super::sched_redis::response_affinity::ResponseBinding;
use okapi_providers::{Outbound, profiles, registry, response_relay::TURN_STATE_HEADER};
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
    let client: Vec<(String, String)> = client
        .iter()
        .filter_map(|(name, value)| {
            if name.eq_ignore_ascii_case(TURN_STATE_HEADER) {
                turn_state_for_upstream(cand, value).map(|value| (name.clone(), value))
            } else {
                Some((name.clone(), value.clone()))
            }
        })
        .collect();
    let mut outbound = super::openai_dialect::outbound(cand);
    outbound.context.session_scope = user_id.map(|id| id.to_string());
    if registry::lookup(&cand.provider).is_some_and(|adapter| adapter.forward_client_identity) {
        outbound.extra_headers.extend(client.iter().cloned());
    }
    outbound.context.client_headers = client;
    outbound
}

/// 上游的轮内粘性令牌回给客户端前加上账号标签。客户端在同一轮里带回时，只转给签发它的
/// 那个上游账号；故障转移到别的账号就不带——令牌只对签发方有意义，也不该把一个账号的
/// 路由信息交给另一个账号。标签取 Responses 历史绑定的上游身份摘要（OAuth 刷新不改
/// account_id 时不变），只在请求或应答带着 turn-state 时才算。
pub fn turn_state_for_client(cand: &ChannelCandidate, upstream: &str) -> String {
    format!("{}.{upstream}", ResponseBinding::account_tag(cand))
}

fn turn_state_for_upstream(cand: &ChannelCandidate, client: &str) -> Option<String> {
    let (tag, upstream) = client.split_once('.')?;
    (tag == ResponseBinding::account_tag(cand) && !upstream.is_empty()).then(|| upstream.to_owned())
}
