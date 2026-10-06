use bytes::Bytes;
use okapi_api::UsageProbe;
use okapi_domain::{AnthropicToolUsage, ServerToolUsage};
use okapi_providers::{
    ChatEvent,
    anthropic::{AnthropicEvent, MetaScanner},
    convert::openai_to_anthropic::{
        StreamState, response_anthropic_to_openai, usage_from_anthropic,
    },
};
use serde_json::{Value, json};

fn tools(search: Option<u32>, fetch: Option<u32>) -> ServerToolUsage {
    ServerToolUsage::Anthropic(AnthropicToolUsage {
        web_search_requests: search,
        web_fetch_requests: fetch,
        code_execution_requests: None,
    })
}

fn stream(start: &Value, updates: Vec<Value>) -> [Option<UsageProbe>; 2] {
    let mut native = MetaScanner::new();
    let mut converted = StreamState::new("tools");
    let mut latest = [None, None];
    let events = std::iter::once(("message_start", json!({"message":{"usage":start}})))
        .chain(
            updates
                .into_iter()
                .map(|u| ("message_delta", json!({"usage":u}))),
        )
        .chain(std::iter::once(("message_stop", json!({}))));
    for (name, value) in events {
        let event = AnthropicEvent {
            event: name.into(),
            data: value.to_string(),
        };
        for (i, events) in [native.scan(Ok(event.clone())), converted.step(Ok(event))]
            .into_iter()
            .enumerate()
        {
            for e in events {
                if let ChatEvent::Data { usage: Some(u), .. } = e.unwrap() {
                    latest[i] = Some(u);
                }
            }
        }
    }
    latest
}

#[test]
fn json_round_trips_provider_tag_counts_zero_and_missing_across_chat_conversion() {
    for (raw_tools, expected) in [
        (Value::Null, None),
        (json!({}), Some(tools(None, None))),
        (json!({"web_search_requests":0}), Some(tools(Some(0), None))),
        (
            json!({"web_search_requests":2,"web_fetch_requests":3}),
            Some(tools(Some(2), Some(3))),
        ),
    ] {
        let raw = json!({"input_tokens":100,"output_tokens":50,"server_tool_use":raw_tools});
        let usage = usage_from_anthropic(Some(&raw))
            .unwrap()
            .to_token_usage()
            .unwrap();
        assert_eq!(usage.server_tool_usage, expected);
        assert_eq!(usage.total_raw(), 150);
        let (chat, _) = response_anthropic_to_openai(&Bytes::from(
            json!({"content":[],"usage":raw}).to_string(),
        ))
        .unwrap();
        let chat: Value = serde_json::from_slice(&chat).unwrap();
        let probe: UsageProbe = serde_json::from_value(chat["usage"].clone()).unwrap();
        assert_eq!(probe.to_token_usage().unwrap(), usage);
        let (anthropic, _) =
            okapi_providers::convert::anthropic_to_openai::response_openai_to_anthropic(
                &Bytes::from(chat.to_string()),
            )
            .unwrap();
        let anthropic: Value = serde_json::from_slice(&anthropic).unwrap();
        assert_eq!(
            usage_from_anthropic(Some(&anthropic["usage"]))
                .unwrap()
                .to_token_usage()
                .unwrap(),
            usage
        );
    }
}

#[test]
fn malformed_tool_counts_are_invalid_even_with_valid_token_usage() {
    for counts in [
        json!(-1),
        json!([]),
        json!("tools"),
        json!({"web_search_requests":-1}),
        json!({"web_search_requests":1.5}),
        json!({"web_search_requests":"1"}),
        json!({"web_search_requests":2_147_483_648_u64}),
        json!({"web_fetch_requests":-1}),
        json!({"web_fetch_requests":false}),
    ] {
        let raw = json!({"input_tokens":100,"output_tokens":50,"server_tool_use":counts});
        assert!(
            usage_from_anthropic(Some(&raw))
                .unwrap()
                .to_token_usage()
                .is_err(),
            "{raw}"
        );
    }
}

#[test]
fn sse_replayed_partial_and_token_only_updates_preserve_latest_native_counts() {
    for result in stream(
        &json!({"input_tokens":100,"output_tokens":1,"server_tool_use":{"web_search_requests":0}}),
        vec![
            json!({"server_tool_use":{"web_search_requests":2,"web_fetch_requests":3}}),
            json!({"server_tool_use":{"web_search_requests":2,"web_fetch_requests":3}}),
            json!({"output_tokens":50,"server_tool_use":{"web_fetch_requests":4}}),
            json!({"output_tokens":50,"server_tool_use":null}),
            json!({"output_tokens":50}),
        ],
    ) {
        let usage = result.unwrap().to_token_usage().unwrap();
        assert_eq!(usage.server_tool_usage, Some(tools(Some(2), Some(4))));
        assert_eq!(usage.total_raw(), 150);
    }
}

#[test]
fn sse_regression_and_malformed_counts_cannot_be_healed_by_later_valid_tokens() {
    for counts in [
        json!({"web_search_requests":1}),
        json!({"web_fetch_requests":0}),
        json!({"web_search_requests":-1}),
        json!({"web_fetch_requests":"3"}),
    ] {
        for result in stream(
            &json!({"input_tokens":100,"output_tokens":1,"server_tool_use":{"web_search_requests":2,"web_fetch_requests":3}}),
            vec![
                json!({"server_tool_use":counts}),
                json!({"output_tokens":50,"server_tool_use":{"web_search_requests":4,"web_fetch_requests":4}}),
            ],
        ) {
            assert!(result.unwrap().invalid);
        }
    }
}

#[test]
fn tool_only_updates_and_local_token_estimates_keep_original_observation() {
    for result in stream(
        &Value::Null,
        vec![json!({"server_tool_use":{"web_search_requests":2}})],
    ) {
        let usage = result.unwrap().with_estimates(100, 50).unwrap();
        assert_eq!(usage.server_tool_usage, Some(tools(Some(2), None)));
        assert_eq!(usage.total_raw(), 150);
        assert_eq!(
            (usage.prompt_source(), usage.completion_source()),
            ("estimated", "estimated")
        );
    }
}

#[test]
fn client_tool_names_and_visible_search_blocks_do_not_create_usage_counters() {
    for content in [
        json!([{"type":"tool_use","id":"call_1","name":"web_search","input":{}}]),
        json!([{"type":"server_tool_use","id":"srv_1","name":"web_search","input":{}},{"type":"web_search_tool_result","tool_use_id":"srv_1","content":{"type":"web_search_tool_result_error","error_code":"unavailable"}}]),
    ] {
        let body = json!({"content":content,"usage":{"input_tokens":100,"output_tokens":50}});
        let (_, probe) = response_anthropic_to_openai(&Bytes::from(body.to_string())).unwrap();
        assert_eq!(
            probe.unwrap().to_token_usage().unwrap().server_tool_usage,
            None
        );
    }
}

#[test]
fn execution_request_counts_round_trip_without_becoming_container_duration() {
    for count in [Value::Null, json!(0), json!(1), json!(2_147_483_647)] {
        let raw = json!({"input_tokens":100,"output_tokens":50,"server_tool_use":{"code_execution_requests":count}});
        let usage = usage_from_anthropic(Some(&raw))
            .unwrap()
            .to_token_usage()
            .unwrap();
        let Some(ServerToolUsage::Anthropic(tools)) = usage.server_tool_usage else {
            panic!("native observation missing")
        };
        assert_eq!(tools.code_execution_requests.map(u64::from), count.as_u64());
        assert_eq!(usage.total_raw(), 150);
        let (chat, _) = response_anthropic_to_openai(&Bytes::from(
            json!({"content":[],"usage":raw}).to_string(),
        ))
        .unwrap();
        let (native, _) =
            okapi_providers::convert::anthropic_to_openai::response_openai_to_anthropic(&chat)
                .unwrap();
        let native: Value = serde_json::from_slice(&native).unwrap();
        assert_eq!(
            usage_from_anthropic(Some(&native["usage"]))
                .unwrap()
                .to_token_usage()
                .unwrap(),
            usage
        );
    }
}

#[test]
fn execution_sse_counts_replace_replays_and_reject_invalid_or_regressing_values() {
    for result in stream(
        &json!({"input_tokens":100,"output_tokens":1,"server_tool_use":{"code_execution_requests":0}}),
        vec![
            json!({"server_tool_use":{"code_execution_requests":2}}),
            json!({"server_tool_use":{"code_execution_requests":2}}),
            json!({"server_tool_use":{"web_fetch_requests":3}}),
            json!({"output_tokens":50}),
        ],
    ) {
        let usage = result.unwrap().to_token_usage().unwrap();
        let Some(ServerToolUsage::Anthropic(tools)) = usage.server_tool_usage else {
            panic!("native observation missing")
        };
        assert_eq!(tools.code_execution_requests, Some(2));
        assert_eq!(tools.web_fetch_requests, Some(3));
        assert_eq!(usage.total_raw(), 150);
    }
    for count in [
        json!(1),
        json!(-1),
        json!(1.5),
        json!("2"),
        json!(true),
        json!([]),
        json!(2_147_483_648_u64),
    ] {
        for result in stream(
            &json!({"input_tokens":100,"output_tokens":1,"server_tool_use":{"code_execution_requests":2}}),
            vec![
                json!({"server_tool_use":{"code_execution_requests":count}}),
                json!({"output_tokens":50,"server_tool_use":{"code_execution_requests":3}}),
            ],
        ) {
            assert!(result.unwrap().invalid, "{count}");
        }
    }
}
