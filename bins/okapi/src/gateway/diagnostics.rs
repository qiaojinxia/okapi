//! Request-local observations survive detached settlement through an owned trace.
//! Only bounded error messages are retained; request bodies and credentials are not.
use axum::{extract::Request, http::HeaderMap, middleware::Next, response::Response};
use okapi_providers::UpstreamError;
use okapi_store::ChannelCandidate;
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Instant,
};

const MAX_ATTEMPTS: usize = 24;
const MAX_MESSAGE: usize = 1024;

#[derive(Clone)]
pub(crate) struct Trace(Arc<Mutex<Data>>);

struct Data {
    fields: Value,
    secrets: Vec<String>,
    attempt_started: Option<Instant>,
    attempt_recorded: bool,
}

tokio::task_local! { static CURRENT: Trace; }

pub(crate) async fn scope(request: Request, next: Next) -> Response {
    let trace = Trace::new(request.headers());
    trace.scope(next.run(request)).await
}

impl Trace {
    pub(crate) fn new(headers: &HeaderMap) -> Self {
        let header = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| headers.get(*name)?.to_str().ok())
        };
        let secrets: Vec<String> = ["authorization", "x-api-key"]
            .iter()
            .filter_map(|name| headers.get(*name)?.to_str().ok())
            .map(|s| s.strip_prefix("Bearer ").unwrap_or(s).to_owned())
            .collect();
        let mut fields = json!({"attempts": []});
        if let Some(ua) = header(&["user-agent"]) {
            fields["user_agent"] = json!(bounded(&sanitize(ua, &secrets), 512));
        }
        // Explicit client correlation only; never persist a prompt-derived sticky hash.
        if let Some(session) = header(&["session_id", "x-session-id"]) {
            fields["session_id"] = json!(bounded(&sanitize(session, &secrets), 256));
        }
        Self(Arc::new(Mutex::new(Data {
            fields,
            secrets,
            attempt_started: None,
            attempt_recorded: false,
        })))
    }

    pub(crate) fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }

    /// Direct handler calls must share the same trace as upstream observations.
    pub(crate) async fn scope<T>(&self, future: impl Future<Output = T>) -> T {
        CURRENT.scope(self.clone(), future).await
    }

    pub(crate) fn snapshot(&self) -> Value {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .fields
            .clone()
    }

    pub(crate) fn set(&self, name: &str, value: Value) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .fields[name] = value;
    }

    pub(crate) fn response_model(&self, model: Option<&str>) {
        if let Some(model) = model.filter(|s| !s.is_empty()) {
            self.set("response_model", json!(bounded(model, 256)));
        }
    }

    pub(crate) fn stream_event(&self, raw: &str) {
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        if matches!(
            value["type"].as_str(),
            Some("error" | "response.failed" | "response.incomplete")
        ) || value["error"].is_object()
            || value["error"].is_string()
        {
            let error = value.get("response").unwrap_or(&value);
            self.failure(&UpstreamError::Status {
                status: 502,
                body: bytes::Bytes::from(error.to_string()),
                retry_after_secs: None,
            });
            self.set("request_failed", json!(true));
            self.set("stream_end_reason", json!("upstream_error"));
        }
    }

    pub(crate) fn media(&self, body: &[u8], video: bool) {
        #[derive(serde::Deserialize)]
        struct Media {
            size: Option<String>,
            quality: Option<String>,
            n: Option<u32>,
            seconds: Option<Value>,
        }
        let Ok(media) = serde_json::from_slice::<Media>(body) else {
            return;
        };
        let mut facts = json!({});
        if let Some(size) = media.size {
            facts[if video { "video_size" } else { "image_size" }] = json!(bounded(&size, 128));
        }
        if let Some(quality) = media.quality {
            facts["image_quality"] = json!(bounded(&quality, 64));
        }
        if !video && let Some(n) = media.n {
            facts["requested_images"] = json!(n);
        }
        if video
            && let Some(seconds) = media
                .seconds
                .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        {
            facts["requested_video_seconds"] = json!(seconds);
        }
        if facts.as_object().is_some_and(|o| !o.is_empty()) {
            self.set("media", facts);
        }
    }

    pub(crate) fn begin(&self, candidate: &ChannelCandidate, model: &str, endpoint: &str) {
        let mut data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !candidate.credential.is_empty() && !data.secrets.contains(&candidate.credential) {
            data.secrets.push(candidate.credential.clone());
            if let Ok(value) = serde_json::from_str::<Value>(&candidate.credential) {
                for key in [
                    "access_token",
                    "refresh_token",
                    "api_key",
                    "token",
                    "secret",
                ] {
                    if let Some(secret) = value[key].as_str().filter(|s| !s.is_empty()) {
                        data.secrets.push(secret.to_owned());
                    }
                }
            }
        }
        let open = data.attempt_started.is_some();
        let attempts = data.fields["attempts"]
            .as_array_mut()
            .expect("trace attempts array");
        if open
            && let Some(last) = attempts.last_mut()
            && last["channel_key_id"] == candidate.channel_key_id
        {
            last["upstream_endpoint"] = json!(endpoint);
            return;
        }
        if attempts.len() >= MAX_ATTEMPTS {
            data.fields["attempts_truncated"] = json!(true);
            data.attempt_started = None;
            data.attempt_recorded = false;
            return;
        }
        attempts.push(json!({
            "channel_id": candidate.channel_id, "channel_key_id": candidate.channel_key_id,
            "provider": candidate.provider, "upstream_model": bounded(model, 256),
            "upstream_endpoint": bounded(endpoint, 256),
        }));
        data.attempt_started = Some(Instant::now());
        data.attempt_recorded = true;
    }

    pub(crate) fn finish(&self, status: Option<i16>, error: Option<&UpstreamError>) {
        let mut data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ms = data
            .attempt_started
            .take()
            .map(|t| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX));
        let details = error.map(|err| error_details(err, &data.secrets));
        if let Some((phase, message)) = &details {
            data.fields["error_phase"] = json!(phase);
            data.fields["error_message"] = json!(message);
        }
        if data.attempt_recorded
            && let Some(last) = data.fields["attempts"]
                .as_array_mut()
                .and_then(|a| a.last_mut())
        {
            last["outcome"] = json!(if error.is_some() {
                "failure"
            } else {
                "success"
            });
            if let Some(ms) = ms {
                last["duration_ms"] = json!(ms);
            }
            if let Some(status) = status {
                last["status"] = json!(status);
            }
            if let Some(err) = error {
                last["error_code"] = json!(err.error_code());
            }
            if let Some((phase, message)) = details {
                last["error_phase"] = json!(phase);
                last["error_message"] = json!(message);
            }
        }
    }

    pub(crate) fn failure(&self, error: &UpstreamError) {
        self.finish(error.upstream_status(), Some(error));
    }
}

pub(crate) async fn upstream<T>(
    candidate: &ChannelCandidate,
    model: &str,
    endpoint: &str,
    future: impl Future<Output = Result<T, UpstreamError>>,
) -> Result<T, UpstreamError> {
    let trace = Trace::current();
    if let Some(trace) = &trace {
        trace.begin(candidate, model, endpoint);
    }
    let result = future.await;
    if let Some(trace) = trace {
        trace.finish(
            result
                .as_ref()
                .err()
                .and_then(UpstreamError::upstream_status),
            result.as_ref().err(),
        );
    }
    result
}

pub(crate) fn snapshot() -> Option<Value> {
    Trace::current().map(|t| t.snapshot())
}

pub(crate) fn phase(code: &str) -> &'static str {
    match code {
        "no_available_channel" | "margin_blocked" | "unsupported_endpoint" => "routing",
        "insufficient_quota" | "rate_limited" | "overloaded" => "admission",
        "upstream_timeout" => "network",
        _ if code.starts_with("upstream") || code.starts_with("batch_") => "upstream",
        _ => "internal",
    }
}

fn error_details(error: &UpstreamError, secrets: &[String]) -> (&'static str, String) {
    let (phase, message) = match error {
        UpstreamError::Status { body, .. } => {
            // Persist an error summary, not a body that may contain echoed prompts.
            let parsed = serde_json::from_slice::<Value>(body).ok();
            let message = parsed
                .as_ref()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .or_else(|| v.pointer("/incomplete_details/reason"))
                        .or_else(|| v.get("message"))
                        .or_else(|| v.get("error"))
                        .or_else(|| v.get("detail"))
                        .and_then(Value::as_str)
                })
                .unwrap_or_else(|| error.error_code());
            ("upstream", message.to_owned())
        }
        UpstreamError::Timeout => ("network", "upstream_timeout".into()),
        UpstreamError::Connect(_) => ("network", "upstream_connect".into()),
        UpstreamError::Stream(reason) => ("stream", reason.clone()),
        UpstreamError::Session { reason, .. } => ("stream", (*reason).into()),
        UpstreamError::Build(reason) => ("request", reason.clone()),
    };
    (phase, sanitize(&message, secrets))
}

fn bounded(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn sanitize(value: &str, secrets: &[String]) -> String {
    let mut clean = value.to_owned();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        clean = clean.replace(secret, "[redacted]");
    }
    let mut redact_next = false;
    let clean = clean
        .split_whitespace()
        .map(|word| {
            let redact = redact_next;
            redact_next = word.eq_ignore_ascii_case("bearer");
            if redact || word.contains("sk-") || word.starts_with("eyJ") {
                "[redacted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    bounded(&clean, MAX_MESSAGE)
}

/// Personal logs never disclose channel credentials, routing attempts or account IDs.
pub(crate) fn public(value: &Value) -> Value {
    let mut result = json!({});
    for key in [
        "error_phase",
        "error_message",
        "response_model",
        "reasoning_effort",
        "stream_end_reason",
        "request_failed",
        "media",
    ] {
        if let Some(value) = value.get(key) {
            result[key] = value.clone();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_summary_redacts_secrets_and_excludes_echoed_request() {
        let error = UpstreamError::Status { status: 429, body: bytes::Bytes::from_static(
            br#"{"error":{"message":"Quota exhausted for Bearer unknown-token sk-secret"},"request":{"messages":["private prompt"]}}"#), retry_after_secs: None };
        let (phase, summary) = error_details(&error, &["sk-secret".into()]);
        assert_eq!(phase, "upstream");
        assert_eq!(summary, "Quota exhausted for Bearer [redacted] [redacted]");
        assert!(!summary.contains("private prompt"));
        assert_eq!(
            sanitize(&"中".repeat(2000), &[]).chars().count(),
            MAX_MESSAGE
        );
    }
    #[test]
    fn personal_projection_never_exposes_routing_or_credentials() {
        let projected = public(
            &json!({"error_message":"quota", "error_phase":"upstream", "attempts":[{"channel_key_id":12}], "session_id":"secret", "user_agent":"sdk"}),
        );
        assert_eq!(
            projected,
            json!({"error_message":"quota", "error_phase":"upstream"})
        );
    }

    #[test]
    fn headers_redact_both_credentials_and_media_excludes_prompt() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret-one".parse().unwrap());
        headers.insert("x-api-key", "secret-two".parse().unwrap());
        headers.insert("user-agent", "sdk secret-one".parse().unwrap());
        headers.insert("session_id", "secret-two".parse().unwrap());
        let trace = Trace::new(&headers);
        trace.media(
            br#"{"size":"1024x1024","quality":"high","n":2,"prompt":"private prompt"}"#,
            false,
        );
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["user_agent"], "sdk [redacted]");
        assert_eq!(snapshot["session_id"], "[redacted]");
        assert_eq!(
            snapshot["media"],
            json!({"image_size":"1024x1024", "image_quality":"high", "requested_images":2})
        );
        assert!(!snapshot.to_string().contains("private prompt"));
    }

    #[test]
    fn stream_failure_is_retained_without_overwriting_a_truncated_attempt() {
        let trace = Trace::new(&HeaderMap::new());
        trace.set("attempts", json!([{"outcome":"success", "status":200}]));
        // A new attempt beyond the storage limit is deliberately not recorded.
        trace.stream_event(
            r#"{"type":"response.failed","response":{"error":{"message":"quota exceeded"}}}"#,
        );
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["request_failed"], true);
        assert_eq!(snapshot["error_message"], "quota exceeded");
        assert_eq!(snapshot["attempts"][0]["status"], 200);
        assert_eq!(snapshot["attempts"][0]["outcome"], "success");
    }

    #[test]
    fn openai_stream_error_without_type_retains_upstream_message() {
        let trace = Trace::new(&HeaderMap::new());
        trace.stream_event(r#"{"error":null,"choices":[]}"#);
        assert!(trace.snapshot()["request_failed"].is_null());
        trace.stream_event(
            r#"{"error":{"type":"invalid_request_error","message":"stream quota exceeded"},"request":{"messages":["private prompt"]}}"#,
        );
        let snapshot = trace.snapshot();
        assert_eq!(snapshot["request_failed"], true);
        assert_eq!(snapshot["error_message"], "stream quota exceeded");
        assert_eq!(snapshot["error_phase"], "upstream");
        assert_eq!(snapshot["stream_end_reason"], "upstream_error");
        assert!(!snapshot.to_string().contains("private prompt"));
    }
}
