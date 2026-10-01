use super::*;

async fn groups(env: &Env) -> Vec<String> {
    let mut codes = Vec::new();
    for i in 0..25 {
        let code = format!("{}-g{i:02}", env.prefix);
        sqlx::query("INSERT INTO price_groups(group_code,description,group_ratio,pool_code,self_select) VALUES($1,$2,1,'default',true)")
            .bind(&code).bind(format!("Group {} {i}", env.prefix)).execute(&env.pg).await.unwrap();
        codes.push(code);
    }
    env.publish().await;
    codes
}

#[tokio::test]
async fn old_pricing_entry_is_bounded_without_an_opt_in() {
    let env = setup().await;
    groups(&env).await;
    for query in [vec![], vec![("paged", "false")]] {
        let url = env.url("/api/pricing", &query);
        let head = env.client.head(url.clone()).send().await.unwrap();
        assert_eq!(head.status(), 200);
        assert_eq!(
            head.headers()
                .get("x-page-limit")
                .and_then(|v| v.to_str().ok()),
            Some("20")
        );
        let response = env.client.get(url).send().await.unwrap();
        let bytes = response.bytes().await.unwrap();
        assert!(
            bytes.len() < 256 * 1024,
            "default catalog must not grow with the full inventory"
        );
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["models"].as_array().unwrap().len(), 20);
        assert_eq!(body["groups"].as_array().unwrap().len(), 20);
        assert!(body["vendors"].as_array().unwrap().len() <= 20);
        assert_eq!(body["groups_page"]["limit"], 20);
        assert_eq!(body["vendors_page"]["limit"], 20);
    }
}

#[tokio::test]
async fn model_group_expansion_only_uses_the_requested_group_page() {
    let env = setup().await;
    let codes = groups(&env).await;
    let args = [("q", env.prefix.as_str()), ("group_q", env.prefix.as_str())];
    let page = env.get(&args).await;
    assert_eq!(page["groups"].as_array().unwrap().len(), 20);
    assert_eq!(page["groups_page"]["total"], 25);
    assert_eq!(page["groups_page"]["next_offset"], 20);
    let next = env.get(&[args[0], args[1], ("group_offset", "20")]).await;
    assert_eq!(next["groups"].as_array().unwrap().len(), 5);
    assert_eq!(next["groups"][0]["code"], codes[20]);
    assert_eq!(
        next["total"], 25,
        "group navigation must not change the model filter"
    );
    for model in next["models"].as_array().unwrap() {
        assert_eq!(
            model["chat_endpoints_by_group"].as_object().unwrap().len(),
            5
        );
        assert_eq!(
            model["groups"],
            json!([]),
            "orphan models do not acquire group access"
        );
    }
}

#[tokio::test]
async fn public_group_search_pages_before_returning_results() {
    let env = setup().await;
    let codes = groups(&env).await;
    let mut collected = Vec::new();
    for offset in [0, 7, 14, 21] {
        let response = env
            .client
            .get(env.url(
                "/api/pricing/groups",
                &[
                    ("q", &env.prefix),
                    ("limit", "7"),
                    ("offset", &offset.to_string()),
                ],
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()["content-type"], "application/json");
        let page: Value = response.json().await.unwrap();
        assert_eq!(page["total"], 25);
        assert_eq!(page["has_more"], offset != 21);
        collected.extend(
            page["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g["code"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(collected, codes);
}
