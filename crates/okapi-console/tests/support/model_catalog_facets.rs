use super::*;

#[tokio::test]
async fn vendor_facets_have_independent_search_and_stable_pages() {
    let env = setup().await;
    let mut expected = Vec::new();
    for (i, model) in env.models.iter().enumerate() {
        let vendor = format!("{}-vendor-{i:02}", env.prefix);
        sqlx::query("UPDATE models SET vendor=$2 WHERE model_name=$1")
            .bind(model)
            .bind(&vendor)
            .execute(&env.pg)
            .await
            .unwrap();
        expected.push(vendor);
    }
    let mut actual = Vec::new();
    for offset in [0, 7, 14, 21] {
        let page = env
            .get(&[
                ("q", &env.prefix),
                ("vendor_limit", "7"),
                ("vendor_offset", &offset.to_string()),
            ])
            .await;
        assert_eq!(page["total"], 25);
        assert_eq!(page["vendors_page"]["total"], 25);
        assert_eq!(page["vendors_page"]["has_more"], offset != 21);
        actual.extend(
            page["vendors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["vendor"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(actual, expected);
    let selected = env
        .get(&[
            ("q", &env.prefix),
            ("vendor", &expected[0]),
            ("vendor_offset", "21"),
        ])
        .await;
    assert_eq!(selected["total"], 1);
    assert_eq!(selected["vendors_page"]["total"], 25);
    assert_eq!(selected["vendors"].as_array().unwrap().len(), 4);
    let special = format!("{}-%_\\", env.prefix);
    sqlx::query("UPDATE models SET vendor=$2 WHERE model_name=$1")
        .bind(&env.models[0])
        .bind(&special)
        .execute(&env.pg)
        .await
        .unwrap();
    let searched = env.get(&[("q", &env.prefix), ("vendor_q", &special)]).await;
    assert_eq!(searched["total"], 25);
    assert_eq!(searched["vendors"], json!([{"vendor":special,"count":1}]));
}

#[tokio::test]
async fn nested_page_caps_invalid_queries_and_empty_offsets_are_explicit() {
    let env = setup().await;
    let page = env
        .get(&[
            ("limit", "999999"),
            ("group_limit", "999999"),
            ("vendor_limit", "999999"),
        ])
        .await;
    for meta in [&page, &page["groups_page"], &page["vendors_page"]] {
        assert_eq!(meta["limit"], 100);
    }
    for field in ["models", "groups", "vendors"] {
        assert!(page[field].as_array().unwrap().len() <= 100);
    }
    for model in page["models"].as_array().unwrap() {
        assert!(model["chat_endpoints_by_group"].as_object().unwrap().len() <= 100);
    }
    let beyond = env
        .get(&[
            ("q", &env.prefix),
            ("group_offset", "9223372036854775807"),
            ("vendor_offset", "9223372036854775807"),
        ])
        .await;
    assert_eq!(beyond["total"], 25);
    for field in ["groups", "vendors"] {
        assert_eq!(beyond[field], json!([]));
    }
    for (key, value) in [
        ("group_limit", "0"),
        ("vendor_limit", "-1"),
        ("group_offset", "-1"),
        ("vendor_offset", "x"),
        ("group_q", &"x".repeat(257)),
        ("vendor_q", &"x".repeat(129)),
    ] {
        for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
            let response = env
                .client
                .request(method, env.url("/api/pricing", &[(key, value)]))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 400, "{key}");
        }
    }
}

#[tokio::test]
async fn group_lookup_is_literal_model_scoped_and_head_matches_get() {
    let env = setup().await;
    let code = format!("{}-%_\\", env.prefix);
    sqlx::query(
        "INSERT INTO price_groups(group_code,description,group_ratio,self_select) VALUES($1,'Visible label',1,true)",
    )
    .bind(&code)
    .execute(&env.pg)
    .await
    .unwrap();
    env.publish().await;
    let url = env.url("/api/pricing/groups", &[("q", &code)]);
    let response = env.client.get(url.clone()).send().await.unwrap();
    assert_eq!(response.headers()["x-total-count"], "1");
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["groups"][0]["code"], code);
    assert!(body["groups"][0].get("pool_code").is_none());
    let head = env.client.head(url).send().await.unwrap();
    assert_eq!(head.headers()["x-total-count"], "1");
    assert!(head.headers().get("content-length").is_none());
    assert!(head.bytes().await.unwrap().is_empty());
    let url = env.url(
        "/api/pricing/groups",
        &[("code", &code), ("model", &env.models[0])],
    );
    assert_eq!(
        env.client
            .get(url.clone())
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["total"],
        0
    );
    let (channel, _) = okapi_store::provision::create_channel(
        &env.pg,
        "facet",
        "openai",
        "http://127.0.0.1:9/private",
        "secret",
        &[&env.models[0]],
        false,
        None,
    )
    .await
    .unwrap();
    let visible: Value = env
        .client
        .get(url.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(visible["total"], 1);
    sqlx::query("UPDATE channels SET deleted_at=now() WHERE id=$1")
        .bind(channel)
        .execute(&env.pg)
        .await
        .unwrap();
    assert_eq!(
        env.client
            .get(url)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["total"],
        0
    );
    for (key, value) in [
        ("limit", "0"),
        ("offset", "-1"),
        ("model", &"m".repeat(257)),
        ("code", &"g".repeat(33)),
    ] {
        assert_eq!(
            env.client
                .get(env.url("/api/pricing/groups", &[(key, value)]))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
}
