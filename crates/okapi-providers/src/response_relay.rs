//! Upstream response headers the gateway hands back to its own client. They are captured in a
//! task-local slot scoped around one attempt, so no response type has to carry them.
use std::cell::RefCell;

/// Codex 后端的轮内粘性令牌：同一轮的第一个响应带回，客户端在这一轮后续请求里原样回传，
/// 上游据此把整轮路由到同一处（Codex CLI 0.160 `core/src/client.rs`）。
pub const TURN_STATE_HEADER: &str = "x-codex-turn-state";
/// 上游实际服务的模型。Codex CLI 拿它和请求的模型比，不同就提示被改路由。
pub const MODEL_HEADER: &str = "openai-model";
/// 上游声明历史推理已计入上下文。Codex CLI 据此估算上下文用量、决定何时自动压缩。
pub const REASONING_INCLUDED_HEADER: &str = "x-reasoning-included";

/// 一次尝试里上游给出的、要回给客户端的头。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Relayed {
    pub turn_state: Option<String>,
    pub model: Option<String>,
    pub reasoning_included: Option<String>,
}

tokio::task_local! {
    static RELAYED: RefCell<Relayed>;
}

/// Runs one upstream attempt and returns the headers its response carried for the client.
pub async fn capture_relayed<F: Future>(attempt: F) -> (F::Output, Relayed) {
    RELAYED
        .scope(RefCell::new(Relayed::default()), async move {
            let output = attempt.await;
            (output, RELAYED.with(RefCell::take))
        })
        .await
}

/// Outside [`capture_relayed`] (probes, token counting) nothing is recorded.
pub(crate) fn record(headers: &reqwest::header::HeaderMap) {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let relayed = Relayed {
        turn_state: value(TURN_STATE_HEADER),
        model: value(MODEL_HEADER),
        reasoning_included: value(REASONING_INCLUDED_HEADER),
    };
    let _ = RELAYED.try_with(|slot| *slot.borrow_mut() = relayed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(*name, value.parse().unwrap());
        }
        headers
    }

    #[tokio::test]
    async fn relayed_headers_are_captured_only_inside_the_attempt_scope() {
        record(&headers(&[(TURN_STATE_HEADER, "outside")]));
        let ((), relayed) = capture_relayed(async {
            record(&headers(&[
                (TURN_STATE_HEADER, "ts-1"),
                (MODEL_HEADER, "gpt-6.1-sol"),
                (REASONING_INCLUDED_HEADER, "true"),
            ]));
        })
        .await;
        assert_eq!(
            relayed,
            Relayed {
                turn_state: Some("ts-1".into()),
                model: Some("gpt-6.1-sol".into()),
                reasoning_included: Some("true".into()),
            }
        );
        let ((), relayed) = capture_relayed(async {}).await;
        assert_eq!(
            relayed,
            Relayed::default(),
            "每次尝试独立，上一次的值不会漏过来"
        );
        let ((), relayed) =
            capture_relayed(async { record(&headers(&[(TURN_STATE_HEADER, "")])) }).await;
        assert_eq!(relayed.turn_state, None);
    }
}
