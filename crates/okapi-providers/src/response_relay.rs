//! Upstream response headers the gateway hands back to its own client. They are captured in a
//! task-local slot scoped around one attempt, so no response type has to carry them.
use std::cell::RefCell;

/// Codex 后端的轮内粘性令牌：同一轮的第一个响应带回，客户端在这一轮后续请求里原样回传，
/// 上游据此把整轮路由到同一处（Codex CLI 0.160 `core/src/client.rs`）。
pub const TURN_STATE_HEADER: &str = "x-codex-turn-state";

tokio::task_local! {
    static TURN_STATE: RefCell<Option<String>>;
}

/// Runs one upstream attempt and returns the turn state its response carried, if any.
pub async fn capture_turn_state<F: Future>(attempt: F) -> (F::Output, Option<String>) {
    TURN_STATE
        .scope(RefCell::new(None), async move {
            let output = attempt.await;
            (output, TURN_STATE.with(|slot| slot.borrow_mut().take()))
        })
        .await
}

/// Outside [`capture_turn_state`] (probes, token counting) nothing is recorded.
pub(crate) fn record(headers: &reqwest::header::HeaderMap) {
    if let Some(value) = headers
        .get(TURN_STATE_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        let _ = TURN_STATE.try_with(|slot| *slot.borrow_mut() = Some(value.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(TURN_STATE_HEADER, value.parse().unwrap());
        headers
    }

    #[tokio::test]
    async fn turn_state_is_captured_only_inside_the_attempt_scope() {
        record(&headers("outside"));
        let ((), state) = capture_turn_state(async { record(&headers("ts-1")) }).await;
        assert_eq!(state.as_deref(), Some("ts-1"));
        let ((), state) = capture_turn_state(async {}).await;
        assert_eq!(state, None, "每次尝试独立，上一次的值不会漏过来");
        let ((), state) = capture_turn_state(async { record(&headers("")) }).await;
        assert_eq!(state, None);
    }
}
