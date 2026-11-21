use super::*;

#[cfg(test)]
mod gemini_admission_fields_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_camel_case_and_snake_case_preserve_caps_and_system_input() {
        for (config, cap, system) in [
            ("generationConfig", "maxOutputTokens", "systemInstruction"),
            (
                "generation_config",
                "max_output_tokens",
                "system_instruction",
            ),
            (
                "generationConfig",
                "max_output_tokens",
                "system_instruction",
            ),
            ("generation_config", "maxOutputTokens", "systemInstruction"),
        ] {
            let mut body = json!({"contents":[{"role":"user","parts":[{"text":"hi"}]}]});
            body[config] = json!({cap:512});
            body[system] = json!({"parts":[{"text":"System admission input"}]});
            let probe: GeminiRequestProbe = serde_json::from_value(body).unwrap();
            assert_eq!(probe.completion_cap_req(), Some(512), "{config}.{cap}");
            assert_eq!(
                probe.prompt_segments(),
                vec!["System admission input", "hi"]
            );
            assert_eq!(probe.prompt_chars(), 24);
        }
    }

    #[test]
    fn malformed_caps_are_rejected_for_both_field_styles() {
        for (config, cap) in [
            ("generationConfig", "maxOutputTokens"),
            ("generation_config", "max_output_tokens"),
        ] {
            for invalid in [json!(-1), json!(4_294_967_296_u64), json!("512"), json!([])] {
                let body = json!({config:{cap:invalid}});
                assert!(
                    serde_json::from_value::<GeminiRequestProbe>(body).is_err(),
                    "{config}.{cap}"
                );
            }
        }
    }
}

#[cfg(test)]
mod third_review_tests {
    use super::*;
    #[test]
    fn tools_contribute_identical_json_to_all_dialect_probes() {
        let tools = serde_json::json!([{"type":"function","function":{"name":"search","parameters":{"type":"object","properties":{"query":{"type":"string"}},"maxItems":100}}}]);
        let body = serde_json::json!({"model":"fixture","tools":tools});
        let chat: ChatRequestProbe = serde_json::from_value(body.clone()).unwrap();
        let messages: MessagesRequestProbe = serde_json::from_value(body.clone()).unwrap();
        let responses: ResponsesRequestProbe = serde_json::from_value(body.clone()).unwrap();
        let gemini: GeminiRequestProbe = serde_json::from_value(body).unwrap();
        assert_eq!(chat.prompt_segments(), vec![tools.to_string().as_str()]);
        assert_eq!(messages.prompt_chars(), chat.prompt_chars());
        assert_eq!(responses.prompt_chars(), chat.prompt_chars());
        assert_eq!(gemini.prompt_chars(), chat.prompt_chars());
    }
    #[test]
    fn reasoning_only_chunks_are_billable_output() {
        let chunk: ChunkProbe =
            serde_json::from_str(r#"{"choices":[{"delta":{"reasoning_content":"thinking"}}]}"#)
                .unwrap();
        assert!(chunk.has_output());
        assert_eq!(chunk.content_chars(), 8);
    }
}

#[cfg(test)]
mod tool_traffic_tests {
    use super::*;
    use serde_json::json;

    fn joined(segments: &[Cow<'_, str>]) -> String {
        segments.concat()
    }

    #[test]
    fn tool_results_and_arguments_are_prompt_input() {
        let chat: ChatRequestProbe = serde_json::from_value(json!({
            "model": "m", "n": 3,
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c", "type": "function",
                    "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}}]},
                {"role": "tool", "tool_call_id": "c", "content": "fn main() {}"}
            ]
        }))
        .unwrap();
        assert_eq!(chat.choices(), 3);
        let text = joined(&chat.prompt_segments());
        assert!(text.contains("a.rs") && text.contains("fn main"), "{text}");

        let messages: MessagesRequestProbe = serde_json::from_value(json!({
            "model": "m", "max_tokens": 10,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t", "name": "read", "input": {"path": "b.rs"}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t",
                        "content": [{"type": "text", "text": "struct B;"}]},
                    {"type": "tool_result", "tool_use_id": "u", "content": "plain result"}]}
            ]
        }))
        .unwrap();
        let text = joined(&messages.prompt_segments());
        for needle in ["b.rs", "struct B;", "plain result"] {
            assert!(text.contains(needle), "{needle}: {text}");
        }

        let responses: ResponsesRequestProbe = serde_json::from_value(json!({
            "model": "m",
            "input": [
                {"type": "function_call", "call_id": "c", "name": "read",
                    "arguments": "{\"path\":\"c.rs\"}"},
                {"type": "function_call_output", "call_id": "c", "output": "enum C {}"},
                {"type": "custom_tool_call", "call_id": "d", "name": "apply_patch",
                    "input": "*** patch"},
                {"type": "custom_tool_call_output", "call_id": "d", "output": "applied"}
            ]
        }))
        .unwrap();
        let text = joined(&responses.prompt_segments());
        for needle in ["c.rs", "enum C", "*** patch", "applied"] {
            assert!(text.contains(needle), "{needle}: {text}");
        }

        let gemini: GeminiRequestProbe = serde_json::from_value(json!({
            "generationConfig": {"candidateCount": 2},
            "contents": [
                {"role": "model", "parts": [{"functionCall": {"name": "read", "args": {"path": "d.rs"}}}]},
                {"role": "user", "parts": [{"function_response": {"name": "read",
                    "response": {"text": "mod d;"}}}]}
            ]
        }))
        .unwrap();
        assert_eq!(gemini.choices(), 2);
        let text = joined(&gemini.prompt_segments());
        assert!(text.contains("d.rs") && text.contains("mod d;"), "{text}");
    }

    #[test]
    fn streamed_tool_arguments_count_as_generated_output() {
        let probe: ChunkProbe = serde_json::from_value(json!({"choices": [{"index": 0, "delta": {
            "tool_calls": [{"index": 0, "function": {"arguments": "{\"x\":1}"}}]}}]}))
        .unwrap();
        assert!(probe.has_output());
        assert_eq!(probe.content_chars(), 7);
    }

    #[test]
    fn choice_counts_default_to_one() {
        let chat: ChatRequestProbe = serde_json::from_value(json!({"model": "m", "n": 0})).unwrap();
        assert_eq!(chat.choices(), 1);
        let gemini: GeminiRequestProbe = serde_json::from_value(json!({})).unwrap();
        assert_eq!(gemini.choices(), 1);
    }
}

#[cfg(test)]
mod client_session_tests {
    use super::MessagesRequestProbe;
    use serde_json::json;

    fn probe(metadata: &serde_json::Value) -> MessagesRequestProbe {
        serde_json::from_value(json!({"model":"m","metadata":metadata})).unwrap()
    }

    #[test]
    fn claude_code_session_is_read_from_both_user_id_formats() {
        let current = json!({"user_id": json!({"device_id":"d","account_uuid":"a",
            "session_id":"4e15302c-9891-462e-8782-87b1eec70318"}).to_string()});
        assert_eq!(
            probe(&current).client_session_id().as_deref(),
            Some("4e15302c-9891-462e-8782-87b1eec70318")
        );
        let legacy = json!({"user_id":"user_abc_account_acc-1_session_9b2f3c1e-0000-4000-8000-000000000001"});
        assert_eq!(
            probe(&legacy).client_session_id().as_deref(),
            Some("9b2f3c1e-0000-4000-8000-000000000001")
        );
        for other in [
            json!({}),
            json!({"user_id":"tenant-42"}),
            json!({"user_id":"{\"session_id\":\"  \"}"}),
            json!({"user_id":"{not json"}),
        ] {
            assert_eq!(probe(&other).client_session_id(), None);
        }
    }
}
