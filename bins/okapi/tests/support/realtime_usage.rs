use super::*;

fn mixed_usage() -> Value {
    json!({"input_tokens":100,"output_tokens":50,"total_tokens":150,
        "input_token_details":{"text_tokens":20,"audio_tokens":60,"image_tokens":20,
            "cached_tokens":40,"cached_tokens_details":{"text_tokens":10,"audio_tokens":25,"image_tokens":5}},
        "output_token_details":{"text_tokens":10,"audio_tokens":40}})
}

async fn request_usage(ws: &mut WsClient, id: &str, usage: Value) {
    ws.send(CliMsg::text(
        json!({"type":"response.create", "mock_response_id":id,"mock_usage":usage}).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(recv_text(ws).await["type"], "response.output_text.delta");
    assert_eq!(recv_text(ws).await["type"], "response.done");
}

async fn report(env: &TestEnv, path: &str) -> Value {
    let resp = reqwest::Client::new()
        .get(format!("http://{}{path}", env.console))
        .bearer_auth(&env.token)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body
}

async fn verify_record(env: &TestEnv, input: i64, output: i64, amount: i64, known: bool) {
    let (status, actual, pt, ct) = wait_record(&env.pg, env.user_id, &env.model).await.unwrap();
    assert_eq!(status, 20);
    assert_eq!(
        (i64::from(pt), i64::from(ct), actual),
        (input, output, amount)
    );
    let records = report(env, "/api/me/logs").await;
    let record = &records["data"][0];
    assert_eq!(record["usage"]["cached_tokens"], 40);
    assert_eq!(record["usage"]["cache_read_reported"], known);
    assert_eq!(record["usage"]["audio_prompt_tokens"], 35);
    assert_eq!(record["usage"]["image_prompt_tokens"], 15);
    assert_eq!(record["usage"]["audio_completion_tokens"], 40);
    assert_eq!(
        record["usage"]["cache_read_modalities"],
        json!({"audio_tokens":25,"image_tokens":5})
    );
    let aggregate = report(env, "/api/me/logs/stat").await;
    assert_eq!(aggregate["cache_read_samples"], i64::from(known));
    assert_eq!(aggregate["prompt_tokens"], input);
    assert_eq!(aggregate["completion_tokens"], output);
    let payload: Value = sqlx::query_scalar("SELECT payload FROM billing_outbox WHERE topic='billing.completed' AND payload->>'request_id'=$1")
        .bind(record["request_id"].as_str().unwrap()).fetch_one(&env.pg).await.unwrap();
    for field in [
        "prompt_tokens",
        "cached_tokens",
        "completion_tokens",
        "cache_read_reported",
    ] {
        assert_eq!(payload[field], record["usage"][field], "{field}");
    }
    for field in ["amount_micro", "original_amount_micro", "discount_micro"] {
        assert_eq!(payload[field], record[field], "{field}");
    }
    if let Some(ch) = &env.state.ch {
        ch.ensure_schema().await.unwrap();
        for _ in 0..100 {
            if okapi::worker::chsink::process_once(&env.pg, ch)
                .await
                .unwrap()
                == 0
            {
                break;
            }
        }
        let metrics = report(env, "/api/me/stats/breakdown?days=1").await;
        assert_eq!(metrics["total"]["requests"], 1);
        assert_eq!(metrics["total"]["tokens"], input + output);
        assert_eq!(metrics["total"]["cached_tokens"], 40);
        assert_eq!(
            metrics["total"]["cache_read_known_requests"],
            i64::from(known)
        );
        if known {
            assert_eq!(metrics["total"]["cache_hit_bp"], 4000);
        } else {
            assert!(metrics["total"]["cache_hit_bp"].is_null());
        }
    } else {
        eprintln!("SKIP: Realtime usage ClickHouse assertions require OKAPI_CLICKHOUSE_URL");
    }
}

#[tokio::test]
async fn realtime_modal_usage_replays_and_partial_reports_match_ledger_and_statistics() {
    for partial in [false, true] {
        let env = setup(Money::from_micros(50_000_000)).await;
        sqlx::query("UPDATE model_pricing SET audio_ratio=8,audio_completion_ratio=2,image_ratio=3,modality_ratios=$2 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
            .bind(&env.model).bind(json!({"audio_cache_read":"2","image_cache_read":"1"})).execute(&env.pg).await.unwrap();
        env.state.pricebook.replace(
            gateway::pricing_loader::load_pricebook(&env.pg)
                .await
                .unwrap(),
        );
        let mut ws = connect(&env).await.unwrap();
        assert_eq!(recv_text(&mut ws).await["type"], "session.created");
        request_usage(&mut ws, "r1", mixed_usage()).await;
        request_usage(&mut ws, "r1", mixed_usage()).await;
        if partial {
            request_usage(&mut ws, "r2", json!({"input_tokens":10,"output_tokens":20})).await;
        }
        ws.close(None).await.unwrap();
        drop(ws);
        // Base 4 micro: 10 text + 35*8 audio + 15*3 image + 10*.5 text cache
        // + 25*2 cached audio + 5*1 cached image + 10*4 text out + 40*16 audio out.
        let amount = 4300 + if partial { 360 } else { 0 };
        verify_record(
            &env,
            if partial { 110 } else { 100 },
            if partial { 70 } else { 50 },
            amount,
            !partial,
        )
        .await;
        assert_eq!(
            env.ledger.balance(env.user_id).await.unwrap().as_micros(),
            50_000_000 - amount
        );
    }
}

#[tokio::test]
async fn invalid_realtime_usage_closes_with_error_and_settles_only_verified_prefix() {
    let env = setup(Money::from_micros(50_000_000)).await;
    let mut ws = connect(&env).await.unwrap();
    recv_text(&mut ws).await;
    let valid =
        json!({"input_tokens":100,"output_tokens":50,"input_token_details":{"cached_tokens":20}});
    request_usage(&mut ws, "r1", valid.clone()).await;
    let mut invalid = valid;
    invalid["input_token_details"]["cached_tokens"] = json!(101);
    ws.send(CliMsg::text(
        json!({"type":"response.create","mock_response_id":"bad","mock_usage":invalid}).to_string(),
    ))
    .await
    .unwrap();
    assert_eq!(
        recv_text(&mut ws).await["type"],
        "response.output_text.delta"
    );
    let error = recv_text(&mut ws).await;
    assert_eq!(error["type"], "error");
    assert_eq!(error["error"]["code"], "upstream_error");
    drop(ws);
    let (status, amount, pt, ct) = wait_record(&env.pg, env.user_id, &env.model).await.unwrap();
    assert_eq!((status, amount, pt, ct), (20, 1160, 100, 50));
    assert_eq!(
        env.ledger.balance(env.user_id).await.unwrap().as_micros(),
        50_000_000 - amount
    );
}
