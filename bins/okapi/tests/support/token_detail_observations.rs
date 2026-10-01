use super::{get, payload, poll_row, setup};
use okapi_domain::{CacheModalities, ModalitiesReported, TokenDetailsReported, TokenUsage};
use serde_json::json;
use uuid::Uuid;

fn usage(count: u32, observed: bool) -> TokenUsage {
    let modal = ModalitiesReported {
        audio: true,
        image: true,
    };
    TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 200,
        audio_prompt_tokens: count,
        image_prompt_tokens: count,
        audio_completion_tokens: count,
        image_completion_tokens: count,
        reasoning_tokens: count,
        cache_read_modalities: Some(CacheModalities {
            audio_tokens: 0,
            image_tokens: 0,
        }),
        cache_write_modalities: Some(CacheModalities {
            audio_tokens: 0,
            image_tokens: 0,
        }),
        reported_details: observed.then_some(TokenDetailsReported {
            prompt: modal,
            completion: modal,
            cache_read: modal,
            cache_write: modal,
            reasoning: true,
        }),
        ..TokenUsage::default()
    }
}

#[tokio::test]
async fn detail_observation_metadata_and_coverage_match_pg_and_ch_log_apis() {
    let env = setup().await;
    assert!(env.state.ch.is_some(), "requires isolated ClickHouse");
    let mut ids = Vec::new();
    for u in [usage(9, true), usage(0, true), usage(0, false)] {
        u.validate().unwrap();
        let id = Uuid::new_v4();
        ids.push(id);
        let tokens = serde_json::to_value(u).unwrap();
        sqlx::query("INSERT INTO billing_records (request_id,user_id,api_key_id,model_name,status,prompt_tokens,cached_tokens,completion_tokens,reasoning_tokens,usage_details) VALUES ($1,$2,$3,$4,20,100,0,200,$5,$6)")
            .bind(id).bind(env.user_id).bind(env.user_key_id).bind(&env.model).bind(i32::try_from(u.reasoning_tokens).unwrap()).bind(json!({"tokens":tokens}))
            .execute(&env.pg).await.unwrap();
        let mut event = payload(&env, None);
        event
            .as_object_mut()
            .unwrap()
            .extend(tokens.as_object().unwrap().clone());
        event["request_id"] = json!(id);
        sqlx::query("INSERT INTO billing_outbox(topic,payload) VALUES ('request_log',$1)")
            .bind(event)
            .execute(&env.pg)
            .await
            .unwrap();
    }
    for id in &ids {
        let admin = poll_row(
            &env,
            &format!("/admin/logs?model={}&request_id={id}&hours=1", env.model),
            |_| true,
        )
        .await;
        let (status, personal) = get(
            &env,
            &format!("/api/me/logs?request_id={id}"),
            &env.user_token,
        )
        .await;
        assert_eq!(status, 200, "{personal}");
        assert_eq!(
            admin["usage"]["reported_details"],
            personal["data"][0]["usage"]["reported_details"]
        );
    }
    let (_, admin) = get(
        &env,
        &format!("/admin/logs/stat?model={}&hours=1&limit=1", env.model),
        &env.super_token,
    )
    .await;
    let (status, personal) = get(
        &env,
        &format!("/api/me/logs/stat?model={}&limit=1&before=1", env.model),
        &env.user_token,
    )
    .await;
    assert_eq!(status, 200, "{personal}");
    assert_eq!(personal["records"], 3);
    assert_eq!(admin["requests"], 3);
    assert_eq!(
        admin["token_detail_observations"],
        personal["token_detail_observations"]
    );
    for (field, expected) in [
        ("image_completion_tokens", 9),
        ("audio_prompt_tokens", 9),
        ("reasoning_tokens", 9),
        ("cache_read_audio_tokens", 0),
        ("cache_write_image_tokens", 0),
    ] {
        let sample = &personal["token_detail_observations"][field];
        assert!(sample["tokens"].is_null(), "{sample}");
        assert_eq!(sample["observed_tokens"], expected);
        assert_eq!(sample["observed_records"], 2);
        assert_eq!(sample["coverage_bp"], 6666);
    }
    verify_filtered_records(&env, &ids).await;
}

async fn verify_filtered_records(env: &super::Env, ids: &[Uuid]) {
    // Restrict to the known zero record: every observed field is truly zero.
    for (path, token) in [
        (
            format!("/api/me/logs/stat?request_id={}", ids[1]),
            &env.user_token,
        ),
        (
            format!(
                "/admin/logs/stat?model={}&request_id={}&hours=1",
                env.model, ids[1]
            ),
            &env.super_token,
        ),
    ] {
        let (status, result) = get(env, &path, token).await;
        assert_eq!(status, 200, "{result}");
        let field = &result["token_detail_observations"]["reasoning_tokens"];
        assert_eq!(field["tokens"], 0);
        assert_eq!(field["complete"], true);
        assert_eq!(field["coverage_bp"], 10000);
    }
    for (path, token) in [
        (
            format!("/api/me/logs/stat?request_id={}", ids[2]),
            &env.user_token,
        ),
        (
            format!(
                "/admin/logs/stat?model={}&request_id={}&hours=1",
                env.model, ids[2]
            ),
            &env.super_token,
        ),
    ] {
        let (_, result) = get(env, &path, token).await;
        let field = &result["token_detail_observations"]["reasoning_tokens"];
        assert!(field["tokens"].is_null() && field["observed_tokens"].is_null());
        assert_eq!(field["observed_records"], 0);
        assert_eq!(field["complete"], false);
    }
    let (_, empty) = get(
        env,
        &format!("/api/me/logs/stat?request_id={}", Uuid::new_v4()),
        &env.user_token,
    )
    .await;
    assert_eq!(empty["records"], 0);
    assert!(empty["token_detail_observations"]["reasoning_tokens"]["coverage_bp"].is_null());
    assert_eq!(empty["token_detail_samples_basis"], "stored_values");
}
