use super::*;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn modal_prices_validate_publish_preserve_clear_and_inherit_user_overrides() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let client = reqwest::Client::new();
    let put = |body: Value| {
        client
            .post(format!("http://{}/admin/models", bed.console))
            .bearer_auth(&bed.admin_token)
            .json(&body)
            .send()
    };
    for bad in [
        json!([]),
        json!({"unknown":"1"}),
        json!({"image_output":6}),
        json!({"image_cache_read":"-1"}),
    ] {
        let response =
            put(json!({"model_name":bed.model,"model_ratio":"99","modality_ratios":bad}))
                .await
                .unwrap();
        assert_eq!(response.status(), 400);
        let ratio: String = sqlx::query_scalar("SELECT model_ratio::text FROM model_pricing WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
            .bind(&bed.model).fetch_one(&bed.pg).await.unwrap();
        assert_eq!(ratio, "1.000000");
    }
    let rates = json!({"image_cache_read":"0.4","image_output":"6","audio_cache_read":"0.8"});
    let mut body = json!({"model_name":bed.model,"model_ratio":"2.5","completion_ratio":"4",
        "image_ratio":"1.6","audio_ratio":"16","cache_ratio":"0.25","cache_write_ratio":"1.25","modality_ratios":rates});
    assert_eq!(put(body.clone()).await.unwrap().status(), 200);
    // Omitting the field must preserve the independent prices in both ratio and tiered upserts.
    body.as_object_mut().unwrap().remove("modality_ratios");
    for tier in ["0:5,1000:10", ""] {
        body["tier_expr"] = json!(tier);
        assert_eq!(put(body.clone()).await.unwrap().status(), 200);
        assert_eq!(stored_rates(&bed).await, rates);
    }
    let published = client
        .post(format!("http://{}/admin/pricing/publish", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), 200);
    assert!(
        gateway::refresh_pricebook_if_newer(&bed.state)
            .await
            .unwrap()
    );
    let public: Value = client
        .get(format!(
            "http://{}/api/pricing?model={}",
            bed.console, bed.model
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(public["models"][0]["modality_ratios"], rates, "{public}");
    sqlx::query("INSERT INTO user_pricing (user_id,model_id,override_kind,custom_model_ratio,custom_completion_ratio,custom_cache_ratio) SELECT $1,id,'ratio',5,4,0.25 FROM models WHERE model_name=$2")
        .bind(bed.user_id).bind(&bed.model).execute(&bed.pg).await.unwrap();
    published_pricing::publish(&bed.pg, bed.user_id).await;
    let book = gateway::pricing_loader::load_pricebook(&bed.pg)
        .await
        .unwrap();
    let ctx = okapi_pricing::CalcContext {
        user: okapi_domain::UserId::new(bed.user_id),
        model: bed.model.clone().into(),
        group: "default".into(),
        user_multiplier: okapi_pricing::RatioFp::ONE,
        monthly_tokens: 0,
        monthly_spend_micro: 0,
        local_minute_of_day: 0,
        now_unix: 0,
        utc_offset_seconds: 0,
        surge_active: false,
        service_tier: None,
    };
    let quote = okapi_pricing::calculate(
        &book,
        &ctx,
        okapi_domain::TokenUsage {
            prompt_tokens: 100,
            cached_tokens: 50,
            cache_write_tokens: 4,
            image_prompt_tokens: 40,
            cache_read_modalities: Some(okapi_domain::CacheModalities {
                image_tokens: 40,
                audio_tokens: 0,
            }),
            completion_tokens: 200,
            image_completion_tokens: 200,
            ..okapi_domain::TokenUsage::default()
        },
    )
    .unwrap();
    // NULL custom cache-write rate inherits 1.25, rather than silently dropping the write premium.
    assert_eq!(quote.amount.as_micros(), 12935);
    body["modality_ratios"] = json!({});
    assert_eq!(put(body).await.unwrap().status(), 200);
    assert_eq!(stored_rates(&bed).await, json!({}));
}

async fn stored_rates(bed: &Bed) -> Value {
    sqlx::query_scalar("SELECT modality_ratios FROM model_pricing WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&bed.model).fetch_one(&bed.pg).await.unwrap()
}
