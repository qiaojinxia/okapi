use super::*;

#[tokio::test]
async fn database_endpoint_filter_agrees_with_routing_for_provider_and_mapping_variants() {
    let env = setup().await;
    let group = format!("{}-route", env.prefix);
    sqlx::query("INSERT INTO channel_pools(pool_code) VALUES($1)")
        .bind(&group)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("INSERT INTO price_groups(group_code,pool_code,group_ratio,self_select) VALUES($1,$1,1,true)")
        .bind(&group)
        .execute(&env.pg)
        .await
        .unwrap();
    let variants = [
        ("openai", None, None, "gpt"),
        ("openai", Some(false), None, "gpt"),
        ("openai_compat", None, None, "gpt"),
        ("openai_compat", Some(true), None, "gpt"),
        ("codex", None, None, "gpt"),
        ("codex", None, Some(false), "gpt"),
        ("gemini", None, None, "gemini"),
        ("anthropic", None, None, "claude"),
        ("vertex", None, None, "CLAUDE-sonnet"),
        ("vertex", None, None, "gemini"),
        ("bedrock", None, None, "claude"),
        ("azure", None, None, "gpt"),
    ];
    for (i, (provider, native, compact, upstream)) in variants.iter().enumerate() {
        let model = &env.models[i];
        let (id, _) = okapi_store::provision::create_channel(
            &env.pg,
            &format!("ingress-{model}"),
            provider,
            "http://127.0.0.1:9/hidden",
            "private",
            &[model],
            false,
            None,
        )
        .await
        .unwrap();
        okapi_store::admin::set_channel_pool_codes(&env.pg, id, std::slice::from_ref(&group))
            .await
            .unwrap();
        sqlx::query("UPDATE channels SET settings=$2,capabilities=$3,model_mapping=$4 WHERE id=$1")
            .bind(id)
            .bind(json!({"responses_native":native}))
            .bind(json!({"compact":compact}))
            .bind(json!({(model):upstream}))
            .execute(&env.pg)
            .await
            .unwrap();
    }
    env.publish().await;
    let baseline = env
        .get(&[("q", &env.prefix), ("group", &group), ("limit", "100")])
        .await;
    assert_eq!(baseline["total"], 12);
    for (endpoint, indices) in [
        ("/v1/chat/completions", vec![0, 1, 2, 3, 6, 7, 8, 9, 10, 11]),
        ("/v1/responses", (0..12).collect()),
        ("/v1/responses/compact", vec![0, 3, 4]),
        ("/v1/messages", vec![0, 1, 2, 3, 7, 8, 10, 11]),
        (
            "/v1beta/models/{model}:generateContent",
            vec![0, 1, 2, 3, 6, 7, 8, 9, 10, 11],
        ),
    ] {
        let expected: Vec<_> = indices.iter().map(|i| env.models[*i].clone()).collect();
        let derived: Vec<_> = baseline["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| {
                m["chat_endpoints_by_group"][&group]
                    .as_array()
                    .unwrap()
                    .contains(&json!(endpoint))
            })
            .map(|m| m["model"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(derived, expected, "routing {endpoint}");
        let page = env
            .get(&[
                ("q", &env.prefix),
                ("group", &group),
                ("endpoint", endpoint),
                ("limit", "100"),
            ])
            .await;
        assert_eq!(names(&page), expected, "database {endpoint}");
        assert_eq!(page["total"], expected.len());
    }
}
