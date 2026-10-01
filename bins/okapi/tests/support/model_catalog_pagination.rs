use super::*;

#[tokio::test]
async fn catalog_filters_mode_aliases_and_availability_before_paging() {
    let env = setup().await;
    sqlx::query("UPDATE model_pricing SET pricing_mode='per_call',per_call_price_micro=100 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&env.models[24]).execute(&env.pg).await.unwrap();
    env.publish().await;
    okapi_store::provision::create_channel(
        &env.pg,
        "paged",
        "openai",
        "http://127.0.0.1:9",
        "fixture",
        &[&env.models[3], &env.models[20]],
        false,
        None,
    )
    .await
    .unwrap();
    let page = env
        .get(&[("q", &env.prefix), ("mode", "per_call"), ("limit", "1")])
        .await;
    assert_eq!(page["total"], 1);
    assert_eq!(names(&page), vec![env.models[24].clone()]);
    let page = env
        .get(&[
            ("q", &env.prefix),
            ("vendors", r#"["openai"]"#),
            ("offset", "14"),
            ("limit", "1"),
        ])
        .await;
    assert_eq!(page["total"], 15);
    assert_eq!(names(&page), vec![env.models[14].clone()]);
    let preview = env
        .get(&[("q", &env.prefix), ("availability_group", "default")])
        .await;
    assert_eq!(
        preview["total"], 25,
        "a price preview must not hide unavailable models"
    );
    let available = env
        .get(&[
            ("q", &env.prefix),
            ("availability_group", "default"),
            ("available", "true"),
            ("limit", "1"),
            ("offset", "1"),
        ])
        .await;
    assert_eq!(available["total"], 2);
    assert_eq!(names(&available), vec![env.models[20].clone()]);
    assert_eq!(
        env.get(&[
            ("q", &env.prefix),
            ("available", "true"),
            ("availability_group", "missing")
        ])
        .await["total"],
        0
    );
}

#[tokio::test]
async fn catalog_sorts_the_full_result_before_limiting_and_keeps_unknown_prices_last() {
    let env = setup().await;
    sqlx::query(
        "UPDATE model_pricing SET model_ratio=0.01,completion_ratio=0.01 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)",
    )
    .bind(&env.models[23])
    .execute(&env.pg)
    .await
    .unwrap();
    sqlx::query("UPDATE model_pricing SET pricing_mode='per_call',per_call_price_micro=1 WHERE model_id=(SELECT id FROM models WHERE model_name=$1)")
        .bind(&env.models[24]).execute(&env.pg).await.unwrap();
    sqlx::query("UPDATE models SET context_window=200000 WHERE model_name=$1")
        .bind(&env.models[22])
        .execute(&env.pg)
        .await
        .unwrap();
    env.publish().await;
    for sort in ["input", "output"] {
        let page = env
            .get(&[("q", &env.prefix), ("sort", sort), ("limit", "1")])
            .await;
        assert_eq!(names(&page), vec![env.models[23].clone()]);
        let last = env
            .get(&[
                ("q", &env.prefix),
                ("sort", sort),
                ("limit", "1"),
                ("offset", "24"),
            ])
            .await;
        assert_eq!(names(&last), vec![env.models[24].clone()]);
    }
    assert_eq!(
        names(
            &env.get(&[("q", &env.prefix), ("sort", "context"), ("limit", "1")])
                .await
        ),
        vec![env.models[22].clone()]
    );
    let name = env
        .get(&[
            ("q", &env.prefix),
            ("sort", "name"),
            ("limit", "1"),
            ("offset", "2"),
        ])
        .await;
    assert_eq!(names(&name), vec![env.models[10].clone()]);
}

#[tokio::test]
async fn catalog_statistics_return_counts_and_facets_without_model_records() {
    let env = setup().await;
    let response = env
        .client
        .get(env.url(
            "/api/pricing/stats",
            &[("q", &env.prefix), ("vendor_limit", "1")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["total"], 25);
    assert_eq!(body["capabilities"], json!(["vision"]));
    assert_eq!(body["has_context"], false);
    assert_eq!(body["vendors_page"]["total"], 3);
    assert_eq!(body["vendors"].as_array().unwrap().len(), 1);
    for forbidden in ["models", "groups", "model_ratio", "chat_endpoints_by_group"] {
        assert!(body.get(forbidden).is_none(), "{forbidden}");
    }
    assert!(!body.to_string().contains("private_note"));
    let next: Value = env
        .client
        .get(env.url(
            "/api/pricing/stats",
            &[
                ("q", &env.prefix),
                ("vendor_limit", "1"),
                ("vendor_offset", "1"),
            ],
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(next["vendors"], json!([{"vendor":"openai","count":15}]));
    let head = env
        .client
        .head(env.url("/api/pricing/stats", &[]))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), 200);
    assert!(head.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn catalog_rejects_invalid_page_filters_and_sorts() {
    let env = setup().await;
    for (key, value) in [
        ("mode", "free"),
        ("sort", "random"),
        ("available", "yes"),
        ("vendors", "openai"),
        ("vendors", "[]"),
        ("vendors", "[null]"),
        ("availability_group", &"x".repeat(33)),
    ] {
        for path in ["/api/pricing", "/api/pricing/stats"] {
            let response = env
                .client
                .get(env.url(path, &[(key, value)]))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 400, "{path}: {key}={value}");
        }
    }
}

#[tokio::test]
async fn catalog_statistics_enforce_viewer_visibility_and_reject_invalid_credentials() {
    let env = setup().await;
    let hidden = format!("{}-hidden", env.prefix);
    sqlx::query("INSERT INTO price_groups(group_code,group_ratio,self_select) VALUES($1,0,false)")
        .bind(&hidden)
        .execute(&env.pg)
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &env.pg,
        "stats-visible",
        "openai",
        "http://127.0.0.1:9",
        "fixture",
        &[&env.models[0]],
        false,
        None,
    )
    .await
    .unwrap();
    env.publish().await;
    for (group, total) in [("default", 1), (hidden.as_str(), 0)] {
        let url = env.url(
            "/api/pricing/stats",
            &[
                ("q", &env.prefix),
                ("available", "true"),
                ("availability_group", group),
            ],
        );
        let body: Value = env
            .client
            .get(url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["total"], total);
        assert!(!body.to_string().contains(&hidden));
    }
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        let response = env
            .client
            .request(method, env.url("/api/pricing/stats", &[]))
            .bearer_auth("invalid-catalog-fixture")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
}
