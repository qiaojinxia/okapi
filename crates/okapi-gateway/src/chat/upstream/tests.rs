use super::*;
use axum::http::HeaderMap;
use okapi_domain::Money;
use okapi_pricing::CalcContext;
use okapi_providers::convert::tool_loss::ForeignTools;
use okapi_providers::reasoning::{Effort, ReasoningDirective};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

fn empty_book() -> okapi_pricing::PriceBook {
    okapi_pricing::book::compile(okapi_pricing::PriceBookSource {
        epoch: 0,
        models: Vec::new(),
        groups: Vec::new(),
        overrides: Vec::new(),
        rules: Vec::new(),
    })
    .expect("empty pricebook compiles")
}

fn test_state() -> crate::state::AppState {
    let pg = sqlx::PgPool::connect_lazy("postgres://127.0.0.1:5432/okapi_test").expect("lazy pool");
    let redis = fred::prelude::Builder::from_config(fred::prelude::Config::default())
        .build()
        .expect("redis client");
    let sched = okapi_kernel::sched_redis::SchedulerRedis::new(redis.clone());
    let ledger = okapi_ledger::BalanceLedger::new(redis);
    let in_flight = okapi_kernel::inflight::InFlightGauge::new(sched.clone(), "test");
    let resources = okapi_providers::inference::Resources::new().expect("resources");
    let inference = okapi_providers::inference::Registry::builtin(&resources).expect("inference");
    let openai = resources.openai.clone();
    crate::state::AppState {
        core: okapi_kernel::state::CoreState {
            pg,
            ledger,
            sched,
            pricebook: Arc::new(okapi_pricing::PriceBookHandle::new(empty_book())),
            node: Arc::from("test"),
            ch: None,
            stats_query_cache: okapi_kernel::state::build_query_cache(),
            nats: None,
            master_key: None,
            settings_cache: moka::future::Cache::builder().max_capacity(4).build(),
            in_flight,
            settlements: okapi_kernel::shutdown::Pending::default(),
        },
        upstreams: crate::state::Upstreams {
            azure: okapi_providers::AzureUpstream::new(openai.clone()),
            openai,
            inference,
            anthropic: resources.anthropic,
            bedrock: resources.bedrock,
            vertex: resources.vertex,
            pass: okapi_providers::PassUpstream::new().expect("pass"),
            refresh_gate: Arc::default(),
        },
        routing: crate::state::RoutingCaches {
            model_cache: moka::future::Cache::builder().max_capacity(4).build(),
            cand_cache: moka::future::Cache::builder().max_capacity(4).build(),
            channel_cost_cache: moka::future::Cache::builder().max_capacity(4).build(),
            channel_control_cache: moka::future::Cache::builder().max_capacity(4).build(),
            margin_cache: moka::future::Cache::builder().max_capacity(1).build(),
            request_policy: moka::future::Cache::builder().max_capacity(1).build(),
            pool_policies: moka::future::Cache::builder().max_capacity(4).build(),
        },
        health: crate::state::HealthMarks {
            key_failures: moka::future::Cache::builder().max_capacity(4).build(),
            egress_failures: moka::future::Cache::builder().max_capacity(4).build(),
            egress_verifying: moka::future::Cache::builder().max_capacity(4).build(),
            dirty_keys_cache: moka::future::Cache::builder().max_capacity(1).build(),
            dirty_egress_cache: moka::future::Cache::builder().max_capacity(1).build(),
        },
        flow: crate::flow_control_for_pool(16),
        media: crate::state::Media {
            image_download_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            batch_submit_gate: Arc::new(tokio::sync::Semaphore::new(1)),
            image_storage: Arc::new(
                crate::images::tasks::objects::Storage::from_env().expect("storage"),
            ),
        },
        data_plane: None,
    }
}

fn test_billing(ingress: Ingress, model: &str) -> (RequestBilling, ProbeInfo) {
    let book = Arc::new(empty_book());
    let state = test_state();
    let bill = RequestBilling {
        trace: super::super::super::diagnostics::Trace::new(&HeaderMap::new()),
        state,
        ingress,
        book,
        calc: CalcContext {
            user: okapi_domain::UserId::new(1),
            model: okapi_domain::ModelCode::from(model),
            group: okapi_domain::GroupCode::from("default"),
            user_multiplier: okapi_pricing::ratio::RatioFp::ONE,
            monthly_tokens: 0,
            monthly_spend_micro: 0,
            local_minute_of_day: 0,
            now_unix: 0,
            utc_offset_seconds: 0,
            surge_active: false,
            service_tier: None,
        },
        user_id: 1,
        key_id: 1,
        member_user_id: None,
        request_id: Uuid::new_v4(),
        reservation_pool: okapi_ledger::Pool::Wallet,
        source_window: None,
        est_prompt: 10,
        density: 1000,
        completion_cap: 1024,
        choices: 1,
        default_output_cap: None,
        directive: None,
        prefs: super::super::super::routing_prefs::RoutingPrefs::default(),
        unified_reasoning: UnifiedReasoning::Absent,
        model: model.to_owned(),
        requested_model: model.to_owned(),
        group: "default".to_owned(),
        is_stream: false,
        started: Instant::now(),
        response_parent: None,
        session: None,
        client_type: "unknown",
        pin: None,
        claude_code_native: false,
        codex_client: None,
        client_ip: None,
        client_headers: Arc::new(Vec::new()),
        pool_chain: vec!["default".to_owned()],
        pool_strategy: None,
        effective_policy: Arc::new(
            okapi_store::pool_policy::StoredPoolPolicy::fallback_default("default").resolve(
                &okapi_domain::pool_policy::RequestFeatures {
                    model,
                    ..Default::default()
                },
            ),
        ),
        service_tier: None,
        has_tier_pricing: false,
        fallback_models: Arc::new(Vec::new()),
        server_tools: super::super::super::server_tools::ToolAdmission::default(),
        reserved_amount: Money::ZERO,
        reservation_snapshot: Arc::new(serde_json::json!({})),
        downgraded_from: None,
        settlement: super::super::SettlementHandoff::default(),
    };
    let info = ProbeInfo {
        authenticated: None,
        requested_model: model.to_owned(),
        stream: false,
        completion_cap_req: Some(1024),
        choices: 1,
        prompt_tokens: 10,
        prompt_chars: 10,
        session: None,
        needs_tools: false,
        needs_vision: false,
        foreign_tools: ForeignTools::default(),
        service_tier: None,
    };
    (bill, info)
}

fn candidate(provider: &str, upstream_model: Option<&str>) -> ChannelCandidate {
    let model_mapping = upstream_model.map_or_else(
        || serde_json::json!({}),
        |m| serde_json::json!({"claude-sonnet-5-5": m, "gpt-5": m}),
    );
    ChannelCandidate {
        channel_id: 1,
        channel_key_id: 1,
        channel_name: "c".into(),
        provider: provider.into(),
        api_base: None,
        credential: "sk-test".into(),
        priority: 1,
        weight: 1,
        trust_upstream_usage: false,
        max_concurrency: None,
        rpm_limit: None,
        daily_spend_cap_micro: None,
        model_mapping,
        thinking_to_content: false,
        bill_by_response_model: false,
        strip_request_fields: Vec::new(),
        inject_request_fields: serde_json::Map::new(),
        tool_loss_policy: None,
        responses_native: false,
        api_version: None,
        aws_region: None,
        oauth_token_url: None,
        extensions: serde_json::json!({}),
        proxy_url: None,
        egress_proxy_id: None,
        egress_max_concurrency: None,
        egress_rotation: None,
        extra_headers: Vec::new(),
        capabilities: serde_json::json!({}),
        cost_milli: 1000,
        same_key_retries: 1,
        first_output_timeout_secs: 30,
        pool_rank: 0,
        pool_role: "primary".to_owned(),
        pool_tags: Vec::new(),
        pool_model_filter: None,
        count_in_flight: false,
        warm_affinity: false,
    }
}

#[tokio::test]
async fn same_dialect_native_request_preserves_exact_caller_bytes() {
    let (mut bill, info) = test_billing(Ingress::Anthropic, "claude-sonnet-5-5");
    bill.directive = Some(ReasoningDirective {
        effort: None,
        budget_tokens: None,
    });
    let raw = Bytes::from_static(
        br#"{"messages":[{"role":"user","content":"hi"}],"output_config":{"effort":"high"},"thinking":{"type":"adaptive","display":"updates"},"max_tokens":1024,"model":"claude-sonnet-5-5"}"#,
    );
    let parsed: serde_json::Value = serde_json::from_slice(&raw).expect("valid json");
    let cand = candidate("anthropic", None);
    let built = build_upstream_body(
        &bill,
        &info,
        &cand,
        &raw,
        "claude-sonnet-5-5",
        Some(&parsed),
    )
    .expect("build succeeds");
    assert_eq!(built.as_ref(), raw.as_ref());
    let outbound = shape_and_verify_outbound(&bill, &cand, built).expect("shape succeeds");
    assert_eq!(outbound.as_ref(), raw.as_ref());
}

#[tokio::test]
async fn two_hop_conversion_and_shaping_produce_expected_body_in_single_pass() {
    let (mut bill, info) = test_billing(Ingress::Responses, "claude-sonnet-5-5");
    bill.prefs.directive_present = true;
    bill.unified_reasoning = UnifiedReasoning::Present;
    bill.directive = Some(ReasoningDirective {
        effort: Some(Effort::High),
        budget_tokens: None,
    });
    let raw = Bytes::from_static(
        br#"{"model":"claude-sonnet-5-5","input":"hello","provider":{"allow_fallbacks":false},"reasoning":{"effort":"high","exclude":true}}"#,
    );
    let parsed: serde_json::Value = serde_json::from_slice(&raw).expect("valid json");
    let mut cand = candidate("anthropic", None);
    cand.strip_request_fields = vec!["temperature".into()];
    cand.inject_request_fields
        .insert("metadata".into(), serde_json::json!({"env": "prod"}));

    let built = build_upstream_body(
        &bill,
        &info,
        &cand,
        &raw,
        "claude-sonnet-5-5",
        Some(&parsed),
    )
    .expect("two-hop build succeeds");
    let outbound = shape_and_verify_outbound(&bill, &cand, built).expect("shape succeeds");
    let v: serde_json::Value = serde_json::from_slice(&outbound).expect("valid json");
    assert_eq!(v["model"], "claude-sonnet-5-5");
    assert_eq!(v["max_tokens"], 1024);
    assert_eq!(v["output_config"]["effort"], "high");
    assert_eq!(v["thinking"]["type"], "adaptive");
    assert!(v.get("provider").is_none());
    assert!(v.get("reasoning").is_none());
    assert_eq!(v["metadata"], serde_json::json!({"env": "prod"}));

    let (mut openai_bill, _) = test_billing(Ingress::OpenAi, "gpt-5");
    openai_bill.prefs.directive_present = true;
    openai_bill.unified_reasoning = UnifiedReasoning::Present;
    let shaped = shape_and_verify_outbound(
        &openai_bill,
        &cand,
        Bytes::from_static(
            br#"{"model":"gpt-5","messages":[],"temperature":0.7,"provider":{"allow_fallbacks":false},"reasoning":{"effort":"high"}}"#,
        ),
    )
    .expect("shape succeeds");
    let sv: serde_json::Value = serde_json::from_slice(&shaped).expect("valid json");
    assert!(sv.get("temperature").is_none());
    assert!(sv.get("provider").is_none());
    assert!(sv.get("reasoning").is_none());
    assert_eq!(sv["metadata"], serde_json::json!({"env": "prod"}));
}

fn chat_data(raw: &str) -> okapi_providers::ChatEvent {
    okapi_providers::ChatEvent::Data {
        event: None,
        has_output: false,
        content_chars: 0,
        usage: None,
        raw: raw.to_owned(),
    }
}

fn chat_stream(items: Vec<okapi_providers::ChatEvent>) -> ChatResponse {
    ChatResponse::Stream(StreamHandle {
        upstream_request_id: None,
        events: Box::pin(futures::stream::iter(items.into_iter().map(Ok))),
    })
}

async fn drain(resp: ChatResponse) -> Vec<Result<okapi_providers::ChatEvent, UpstreamError>> {
    use futures::StreamExt as _;
    let ChatResponse::Stream(h) = resp else {
        panic!("expected a stream");
    };
    h.events.collect().await
}

/// chat 上游在流里报错（`{"error":…}`）而客户端说别的方言：转换之前就得认成上游错误。
/// 否则转换器把它当普通块，网关转换后再找已经找不到，客户端只拿到空回复（看不到 529 / 429）。
#[tokio::test]
async fn chat_stream_errors_survive_conversion_to_other_dialects() {
    use futures::StreamExt as _;
    let overloaded = r#"{"error":{"code":"server_is_overloaded","message":"busy"}}"#;
    let is_overloaded = |item: &Result<okapi_providers::ChatEvent, UpstreamError>| matches!(item, Err(UpstreamError::Status { status, .. }) if *status >= 500);
    // Responses 客户端：出口转换自带识别
    let out = drain(
        wrap_responses_egress(
            chat_stream(vec![
                chat_data(overloaded),
                okapi_providers::ChatEvent::Done,
            ]),
            "m",
        )
        .expect("egress wraps"),
    )
    .await;
    assert!(
        out.iter().any(is_overloaded),
        "Responses 出口丢了流内错误：{out:?}"
    );
    // Anthropic 客户端：与 dispatch 分支同一组合（先认错误，再转 Anthropic 事件）
    let ChatResponse::Stream(h) = surface_stream_failures(chat_stream(vec![chat_data(overloaded)]))
    else {
        panic!("expected a stream");
    };
    let mut st = conv_a2o::OaiStreamToAnthropic::new("m");
    let out: Vec<_> = h
        .events
        .flat_map(move |item| futures::stream::iter(st.step(item)))
        .collect()
        .await;
    assert!(
        out.first().is_some_and(is_overloaded),
        "Anthropic 出口丢了流内错误：{out:?}"
    );
    // Gemini 客户端
    let out = drain(
        conv_g2o::wrap_chat_as_gemini(
            surface_stream_failures(chat_stream(vec![chat_data(overloaded)])),
            "m",
        )
        .expect("gemini wraps"),
    )
    .await;
    assert!(
        out.iter().any(is_overloaded),
        "Gemini 出口丢了流内错误：{out:?}"
    );
    // 普通增量与认不出类型的错误照常通过（后者留给空回复诊断）
    for raw in [
        r#"{"id":"c1","choices":[{"index":0,"delta":{"content":"hi"}}]}"#,
        r#"{"error":{"code":"something_new","message":"?"}}"#,
    ] {
        let out = drain(surface_stream_failures(chat_stream(vec![chat_data(raw)]))).await;
        assert!(
            matches!(&out[..], [Ok(okapi_providers::ChatEvent::Data { raw: got, .. })] if got == raw),
            "{raw}: {out:?}"
        );
    }
}
