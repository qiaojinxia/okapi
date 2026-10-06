use super::*;
use sqlx::Row;

#[tokio::test]
async fn api_and_oauth_creation_share_all_options_and_one_initial_credential() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let pool = format!("creation-{}", &Uuid::new_v4().simple().to_string()[..16]);
    sqlx::query("INSERT INTO channel_pools(pool_code) VALUES($1)")
        .bind(&pool)
        .execute(&env.pg)
        .await
        .unwrap();
    let owner: i64 = sqlx::query_scalar("SELECT user_id FROM api_keys WHERE key_hash=$1")
        .bind(hash(&env.admin_token))
        .fetch_one(&env.pg)
        .await
        .unwrap();
    for provider in ["openai", "anthropic_max", "codex"] {
        let mut body = json!({"name":format!("options-{provider}-{}",env.model),"models":[env.model],
            "api_base":format!("http://{}/v1",env.mock),"priority":17,"max_concurrency":4,
            "cost_milli":250,"data_retention":"none","pools":[{"pool_code":pool,"priority_override":9,"weight_override":2}],
            "settings":{"account_control":{"refresh_mode":"external","failure_threshold":5},"extra_headers":{"x-creation-test":"preserved"}}});
        let endpoint = if provider == "openai" {
            body["provider"] = json!(provider);
            body["credential"] = json!("mock-api-key");
            "/admin/channels"
        } else {
            let started: Value = client
                .post(format!("http://{}/admin/channels/oauth/start", env.console))
                .bearer_auth(&env.admin_token)
                .json(&json!({"provider":provider}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["state"] = started["state"].clone();
            body["code"] = json!("auth-code-1");
            body["token_url"] = json!(format!("http://{}/token", env.mock));
            "/admin/channels/oauth/exchange"
        };
        let response = client
            .post(format!("http://{}{endpoint}", env.console))
            .bearer_auth(&env.admin_token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let created: Value = response.json().await.unwrap();
        assert_eq!(status, 200, "{created}");
        let channel = created["channel_id"].as_i64().unwrap();
        let row = sqlx::query("SELECT priority,owner_id,settings,upstream_unit_cost,api_base FROM channels WHERE id=$1")
            .bind(channel).fetch_one(&env.pg).await.unwrap();
        assert_eq!(row.get::<i32, _>("priority"), 17);
        assert_eq!(row.get::<Option<i64>, _>("owner_id"), Some(owner));
        assert_eq!(
            row.get::<String, _>("api_base"),
            format!("http://{}/v1", env.mock)
        );
        assert_eq!(
            row.get::<Value, _>("upstream_unit_cost")["relative_cost_milli"],
            250
        );
        let settings: Value = row.get("settings");
        assert_eq!(settings["data_retention"], "none");
        assert_eq!(settings["extra_headers"]["x-creation-test"], "preserved");
        assert_eq!(settings["account_control"]["failure_threshold"], 5);
        let keys: Vec<(i16, Option<i32>)> = sqlx::query_as(
            "SELECT credential_kind,max_concurrency FROM channel_keys WHERE channel_id=$1",
        )
        .bind(channel)
        .fetch_all(&env.pg)
        .await
        .unwrap();
        assert_eq!(keys, vec![(i16::from(provider != "openai"), Some(4))]);
        let members: Vec<(String,Option<i32>,Option<i32>)> = sqlx::query_as("SELECT pool_code,priority_override,weight_override FROM pool_channels WHERE channel_id=$1")
            .bind(channel).fetch_all(&env.pg).await.unwrap();
        assert_eq!(members, vec![(pool.clone(), Some(9), Some(2))]);
    }
    // A folded default endpoint is resolved from the registration, never stored empty.
    let response = client.post(format!("http://{}/admin/channels",env.console)).bearer_auth(&env.admin_token)
        .json(&json!({"name":"default-endpoint","provider":"openai","api_base":"","credential":"mock","models":[env.model]}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200);
    let created: Value = response.json().await.unwrap();
    let base: String = sqlx::query_scalar("SELECT api_base FROM channels WHERE id=$1")
        .bind(created["channel_id"].as_i64().unwrap())
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(
        base,
        okapi_providers::registry::lookup("openai")
            .unwrap()
            .default_base
            .unwrap()
    );
}

#[tokio::test]
async fn invalid_creation_options_do_not_exchange_or_leave_partial_channels() {
    let env = setup().await;
    let client = reqwest::Client::new();
    let name = format!("rejected-{}", env.model);
    for bad in [
        json!({"settings":[]}),
        json!({"pools":["missing-pool"]}),
        json!({"max_concurrency":0}),
    ] {
        let started: Value = client
            .post(format!("http://{}/admin/channels/oauth/start", env.console))
            .bearer_auth(&env.admin_token)
            .json(&json!({"provider":"anthropic_max"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut body = json!({"state":started["state"],"code":"auth-code-1","name":name,"models":[env.model],
            "token_url":format!("http://{}/token",env.mock)});
        body.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        let response = client
            .post(format!(
                "http://{}/admin/channels/oauth/exchange",
                env.console
            ))
            .bearer_auth(&env.admin_token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
    }
    assert_eq!(env.mock_state.token_calls.load(Ordering::SeqCst), 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM channels WHERE name=$1")
        .bind(&name)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
    // A database failure after the credential insert must roll back both inserts.
    let pools = [okapi_store::admin::PoolMember {
        pool_code: "missing-pool".into(),
        priority_override: None,
        weight_override: None,
    }];
    let result = okapi_store::provision::create_channel_configured(
        &env.pg,
        okapi_store::provision::ChannelCreate {
            name: &name,
            provider: "openai",
            api_base: "https://example.test",
            credential: "mock",
            models: &[],
            trust_upstream_usage: false,
            owner_id: None,
            settings: None,
            priority: 0,
            max_concurrency: None,
            cost_milli: None,
            pools: Some(&pools),
            egress: None,
            egress_preassigned: None,
        },
        None,
    )
    .await;
    assert!(result.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM channels WHERE name=$1")
        .bind(&name)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM channel_keys k LEFT JOIN channels c ON c.id=k.channel_id WHERE c.id IS NULL")
        .fetch_one(&env.pg).await.unwrap();
    assert_eq!(orphans, 0);
}
