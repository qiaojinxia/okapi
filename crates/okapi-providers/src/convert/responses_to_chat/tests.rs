use super::*;
use crate::types::ChatEvent;

#[cfg(test)]
mod third_review_tests {
    use super::*;

    #[test]
    fn reasoning_fragments_survive_and_only_delta_characters_are_counted() {
        for with_usage in [false, true] {
            let mut state = ChatStreamToResponses::new("fixture");
            let mut events = Vec::new();
            let usage = serde_json::from_value::<UsageProbe>(json!({"prompt_tokens":7,"completion_tokens":10,"completion_tokens_details":{"reasoning_tokens":3}})).unwrap();
            for (value, probe) in [
                (
                    json!({"choices":[{"delta":{"reasoning_content":"思考"}}]}),
                    None,
                ),
                (json!({"choices":[{"delta":{"reasoning":" done"}}]}), None),
                (json!({"choices":[{"delta":{"content":"OK"}}]}), None),
                (json!({"choices":[]}), with_usage.then_some(usage)),
            ] {
                events.extend(state.step(Ok(ChatEvent::Data {
                    raw: value.to_string(),
                    event: None,
                    has_output: false,
                    content_chars: 0,
                    usage: probe,
                })));
            }
            events.extend(state.step(Ok(ChatEvent::Done)));
            let counted: usize = events
                .iter()
                .filter_map(|event| match event {
                    Ok(ChatEvent::Data { content_chars, .. }) => Some(*content_chars),
                    _ => None,
                })
                .sum();
            assert_eq!(counted, "思考 doneOK".chars().count());
            assert!(events.iter().any(|event| matches!(event,Ok(ChatEvent::Data{has_output:true,event:Some(name),..}) if name=="response.reasoning_summary_text.delta")));
            let completed = events
                .iter()
                .find_map(|event| match event {
                    Ok(ChatEvent::Data {
                        raw,
                        event: Some(name),
                        ..
                    }) if name == "response.completed" => serde_json::from_str::<Value>(raw).ok(),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                completed["response"]["output"][0]["summary"][0]["text"],
                "思考 done"
            );
            assert_eq!(
                completed["response"]["output"][1]["content"][0]["text"],
                "OK"
            );
            if with_usage {
                assert_eq!(completed["response"]["usage"]["output_tokens"], 10);
            } else {
                assert!(completed["response"]["usage"].is_null());
            }
        }
        let (body, _) = response_chat_to_responses(&Bytes::from(
            json!({"choices":[{"message":{"content":"OK","reasoning_content":"think"}}]})
                .to_string(),
        ))
        .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["output"][0]["summary"][0]["text"], "think");
    }
    #[test]
    fn streamed_tool_arguments_survive_fragmentation_and_completion() {
        let mut state = ChatStreamToResponses::new("fixture");
        let data = |value: Value| {
            Ok(ChatEvent::Data {
                raw: value.to_string(),
                event: None,
                has_output: true,
                content_chars: 0,
                usage: None,
            })
        };
        state.step(data(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":"{\"q\":"}}]}}]})));
        let delta=state.step(data(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]}}]})));
        assert!(delta.iter().any(|event|matches!(event,Ok(ChatEvent::Data{event:Some(name),..}) if name=="response.function_call_arguments.delta")));
        let done = state.step(Ok(ChatEvent::Done));
        let completed = done
            .iter()
            .find_map(|event| match event {
                Ok(ChatEvent::Data {
                    raw,
                    event: Some(name),
                    ..
                }) if name == "response.completed" => serde_json::from_str::<Value>(raw).ok(),
                _ => None,
            })
            .unwrap();
        assert_eq!(completed["response"]["output"][0]["name"], "search");
        assert_eq!(completed["response"]["output"][0]["call_id"], "call_1");
        assert_eq!(
            completed["response"]["output"][0]["arguments"],
            r#"{"q":"hi"}"#
        );
    }
}

#[cfg(test)]
mod part_tests {
    use super::*;

    fn convert(input: &Value) -> Result<Value, UpstreamError> {
        let body = Bytes::from(json!({"model": "m", "input": input}).to_string());
        request_responses_to_chat(&body, "up").map(|out| serde_json::from_slice(&out).unwrap())
    }

    /// 内联文件与 refusal 照样带过去；chat 表达不了的引用报错，不能静默丢掉。
    #[test]
    fn files_and_refusals_carry_over_and_references_are_rejected() {
        let out = convert(&json!([
            {"role": "user", "content": [
                {"type": "input_text", "text": "read"},
                {"type": "input_file", "filename": "a.pdf",
                 "file_data": "data:application/pdf;base64,JVBERi0="}
            ]},
            {"role": "assistant", "content": [{"type": "refusal", "refusal": "no"}]}
        ]))
        .unwrap();
        assert_eq!(
            out["messages"][0]["content"][1],
            json!({"type": "file", "file": {
                "filename": "a.pdf", "file_data": "data:application/pdf;base64,JVBERi0="}})
        );
        assert_eq!(out["messages"][1]["content"], "no");
        for part in [
            json!({"type": "input_file", "file_id": "file-1"}),
            json!({"type": "input_image", "file_id": "file-2"}),
        ] {
            let kind = part["type"].as_str().unwrap().to_owned();
            let err = convert(&json!([{"role": "user", "content": [part]}])).unwrap_err();
            assert!(
                matches!(&err, UpstreamError::Build(reason)
                    if *reason == format!("unsupported_content:{kind}")),
                "{err:?}"
            );
        }
    }
}

#[cfg(test)]
mod fifth_review_tests {
    use super::*;

    fn request(body: &Value) -> Result<Value, UpstreamError> {
        request_responses_to_chat(&Bytes::from(body.to_string()), "up")
            .map(|out| serde_json::from_slice(&out).unwrap())
    }

    fn stream(chunks: &[Value]) -> Vec<(String, Value)> {
        let mut state = ChatStreamToResponses::new("fixture");
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(state.step(Ok(ChatEvent::Data {
                raw: chunk.to_string(),
                event: None,
                has_output: true,
                content_chars: 0,
                usage: None,
            })));
        }
        events.extend(state.step(Ok(ChatEvent::Done)));
        events
            .into_iter()
            .filter_map(|event| match event.unwrap() {
                ChatEvent::Data {
                    raw,
                    event: Some(name),
                    ..
                } => Some((name, serde_json::from_str(&raw).unwrap())),
                _ => None,
            })
            .collect()
    }

    fn completed(events: &[(String, Value)]) -> &Value {
        &events
            .iter()
            .find(|(name, _)| name == "response.completed")
            .unwrap()
            .1["response"]
    }

    #[test]
    fn named_tool_choice_is_reshaped_and_hosted_choices_are_rejected() {
        let tools = json!([{"type":"function","name":"f","parameters":{"type":"object"}}]);
        for choice in [
            json!({"type":"function","name":"f"}),
            json!({"type":"function","function":{"name":"f"}}),
        ] {
            let out = request(&json!({"input":"hi","tools":tools,"tool_choice":choice})).unwrap();
            assert_eq!(
                out["tool_choice"],
                json!({"type":"function","function":{"name":"f"}})
            );
        }
        let out = request(&json!({"input":"hi","tools":tools,"tool_choice":"required"})).unwrap();
        assert_eq!(out["tool_choice"], "required");
        let err = request(&json!({"input":"hi","tool_choice":{"type":"web_search_preview"}}))
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("unsupported_content:tool_choice"),
            "{err}"
        );
    }

    #[test]
    fn stored_history_references_are_rejected_instead_of_dropped() {
        for key in ["previous_response_id", "conversation"] {
            let err = request(&json!({"input":"next", key:"resp_1"})).unwrap_err();
            assert!(
                format!("{err:?}").contains(&format!("unsupported_content:{key}")),
                "{err}"
            );
        }
        assert!(request(&json!({"input":"next","previous_response_id":null})).is_ok());
    }

    #[test]
    fn array_tool_outputs_keep_their_text_and_reject_media() {
        let out = request(
            &json!({"input":[{"type":"function_call_output","call_id":"c1",
            "output":[{"type":"input_text","text":"a"},{"type":"input_text","text":"b"}]}]}),
        )
        .unwrap();
        assert_eq!(out["messages"][0]["content"], "a\nb");
        let err = request(
            &json!({"input":[{"type":"function_call_output","call_id":"c1",
            "output":[{"type":"input_image","image_url":"data:image/png;base64,AA=="}]}]}),
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("unsupported_content:input_image"));
    }

    #[test]
    fn refusals_reach_the_client_in_json_and_stream() {
        let (body, _) = response_chat_to_responses(&Bytes::from(
            json!({"choices":[{"finish_reason":"stop",
                "message":{"content":null,"refusal":"I can't help with that."}}]})
            .to_string(),
        ))
        .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["output"][0]["content"],
            json!([{"type":"refusal","refusal":"I can't help with that."}])
        );

        let events = stream(&[
            json!({"id":"x","choices":[{"delta":{"refusal":"I can't"}}]}),
            json!({"choices":[{"delta":{"refusal":" help."},"finish_reason":"stop"}]}),
        ]);
        let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"response.refusal.delta"), "{names:?}");
        let done = events
            .iter()
            .find(|(name, _)| name == "response.refusal.done")
            .unwrap();
        assert_eq!(done.1["refusal"], "I can't help.");
        assert_eq!(
            completed(&events)["output"][0]["content"],
            json!([{"type":"refusal","refusal":"I can't help."}])
        );
    }

    #[test]
    fn empty_streams_still_open_with_response_created() {
        let events = stream(&[]);
        assert_eq!(events[0].0, "response.created");
        assert_eq!(events.last().unwrap().0, "response.completed");
    }

    #[test]
    fn tool_deltas_without_index_follow_their_call_id() {
        let events = stream(&[
            json!({"choices":[{"delta":{"tool_calls":[{"id":"call_1","function":{"name":"f","arguments":"{\"a\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"1}"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"id":"call_2","function":{"name":"g","arguments":"{}"}}]}}]}),
        ]);
        let output = &completed(&events)["output"];
        assert_eq!(output[0]["call_id"], "call_1");
        assert_eq!(output[0]["arguments"], "{\"a\":1}");
        assert_eq!(output[1]["call_id"], "call_2");
        assert_eq!(output[1]["name"], "g");
    }

    #[test]
    fn unknown_input_items_are_rejected_instead_of_dropped() {
        for kind in ["item_reference", "web_search_call", "custom_tool_call"] {
            let err =
                request(&json!({"input":[{"type":kind,"id":"x"},{"role":"user","content":"hi"}]}))
                    .unwrap_err();
            assert!(
                matches!(&err, UpstreamError::Build(reason) if reason.ends_with(&format!("input_item.{kind}"))),
                "{kind}: {err:?}"
            );
        }
        // 回放的 reasoning 项照旧略过
        assert!(request(&json!({"input":[{"type":"reasoning","id":"r","summary":[]},{"role":"user","content":"hi"}]})).is_ok());
    }
}

#[cfg(test)]
mod sixth_scan_tests {
    use super::*;

    fn request(body: &Value) -> Result<Value, UpstreamError> {
        request_responses_to_chat(&Bytes::from(body.to_string()), "up")
            .map(|out| serde_json::from_slice(&out).unwrap())
    }

    /// `text.format` 是 Responses 的结构化输出开关：降级到 chat 时对应 `response_format`，
    /// 不能悄悄丢掉让客户端拿到自由文本。
    #[test]
    fn structured_output_formats_reach_chat_as_response_format() {
        let out = request(&json!({"input":"hi","text":{"format":{
            "type":"json_schema","name":"answer","strict":true,"description":"d",
            "schema":{"type":"object","properties":{"a":{"type":"string"}}}}}}))
        .unwrap();
        assert_eq!(
            out["response_format"],
            json!({"type":"json_schema","json_schema":{
                "name":"answer","strict":true,"description":"d",
                "schema":{"type":"object","properties":{"a":{"type":"string"}}}}})
        );
        // name 缺了按 response 补；strict / description 没给就不带
        let out = request(&json!({"input":"hi","text":{"format":{
            "type":"json_schema","schema":{"type":"object"}}}}))
        .unwrap();
        assert_eq!(
            out["response_format"],
            json!({"type":"json_schema","json_schema":{"name":"response","schema":{"type":"object"}}})
        );
        let out = request(&json!({"input":"hi","text":{"format":{"type":"json_object"}}})).unwrap();
        assert_eq!(out["response_format"], json!({"type":"json_object"}));
        // 纯文本是缺省：不带 response_format
        for text in [
            json!({"format":{"type":"text"}}),
            json!({"verbosity":"low"}),
        ] {
            let out = request(&json!({"input":"hi","text":text})).unwrap();
            assert!(out.get("response_format").is_none(), "{out}");
        }
        // 表达不了的：报错，不静默退成自由文本
        for format in [json!({"type":"grammar"}), json!({"type":"json_schema"})] {
            let err = request(&json!({"input":"hi","text":{"format":format}})).unwrap_err();
            assert!(
                format!("{err:?}").contains("unsupported_content:text.format"),
                "{err:?}"
            );
        }
    }

    #[test]
    fn parallel_tool_calls_follows_the_tools() {
        let tools = json!([{"type":"function","name":"f","parameters":{"type":"object"}}]);
        for flag in [true, false] {
            let out =
                request(&json!({"input":"hi","tools":tools,"parallel_tool_calls":flag})).unwrap();
            assert_eq!(out["parallel_tool_calls"], flag);
        }
        // 没有工具时上游拒收这个参数
        let out = request(&json!({"input":"hi","parallel_tool_calls":false})).unwrap();
        assert!(out.get("parallel_tool_calls").is_none(), "{out}");
    }

    /// 部分兼容上游把 `content` 回成分段数组：文本段照样交付，不能丢掉却照样计费。
    #[test]
    fn segmented_chat_content_is_delivered_in_json_and_stream() {
        let (body, _) = response_chat_to_responses(&Bytes::from(
            json!({"choices":[{"finish_reason":"stop","message":{"content":[
                {"type":"text","text":"Hel"},
                {"type":"image_url","image_url":{"url":"x"}},
                {"text":"lo"}]}}]})
            .to_string(),
        ))
        .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["output"][0]["content"][0]["text"], "Hello", "{body}");

        let mut state = ChatStreamToResponses::new("fixture");
        let mut events = Vec::new();
        for chunk in [
            json!({"choices":[{"delta":{"content":[{"type":"text","text":"Hel"}]}}]}),
            json!({"choices":[{"delta":{"content":[{"type":"text","text":"lo"}]},"finish_reason":"stop"}]}),
        ] {
            events.extend(state.step(Ok(ChatEvent::Data {
                raw: chunk.to_string(),
                event: None,
                has_output: true,
                content_chars: 0,
                usage: None,
            })));
        }
        events.extend(state.step(Ok(ChatEvent::Done)));
        let completed = events
            .into_iter()
            .find_map(|event| match event.unwrap() {
                ChatEvent::Data {
                    raw,
                    event: Some(name),
                    ..
                } if name == "response.completed" => {
                    Some(serde_json::from_str::<Value>(&raw).unwrap())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "Hello"
        );
    }
}
