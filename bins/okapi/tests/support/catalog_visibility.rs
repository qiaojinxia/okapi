use super::*;

async fn user(env: &Env) -> (i64, String) {
    use sha2::{Digest, Sha256};
    let name = format!("viewer-{}", Uuid::new_v4().simple());
    let id = okapi_store::provision::create_user(&env.pg, &name)
        .await
        .unwrap();
    let token = format!("sk-okapi-{name}");
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    okapi_store::provision::create_api_key(&env.pg, id, &hash, "viewer")
        .await
        .unwrap();
    (id, token)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn catalogs_only_expose_selectable_groups_for_each_viewer() {
    let env = setup().await;
    let open = format!("{}-open", env.prefix);
    let assigned = format!("{}-assigned", env.prefix);
    let hidden = format!("{}-hidden", env.prefix);
    for (code, public) in [(&open, true), (&assigned, false), (&hidden, false)] {
        sqlx::query("INSERT INTO price_groups(group_code,group_ratio,description,self_select) VALUES($1,0.5,$1,$2)")
            .bind(code).bind(public).execute(&env.pg).await.unwrap();
    }
    let (alice, alice_key) = user(&env).await;
    let (_, bob_key) = user(&env).await;
    sqlx::query("INSERT INTO user_groups(user_id,group_code,priority) VALUES($1,$2,10)")
        .bind(alice)
        .bind(&assigned)
        .execute(&env.pg)
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &env.pg,
        &env.prefix,
        "openai",
        "http://127.0.0.1:9/v1",
        "fixture",
        &[&env.models[0]],
        false,
        None,
    )
    .await
    .unwrap();

    for (token, allowed) in [
        (None, vec![&open]),
        (Some(&bob_key), vec![&open]),
        (Some(&alice_key), vec![&assigned, &open]),
    ] {
        for path in ["/api/pricing", "/api/pricing/models", "/api/pricing/groups"] {
            let args = if path.ends_with("groups") {
                vec![("q", env.prefix.as_str()), ("limit", "1")]
            } else {
                vec![
                    ("model", env.models[0].as_str()),
                    ("group_q", env.prefix.as_str()),
                    ("group_limit", "1"),
                ]
            };
            let mut request = env.client.get(env.url(path, &args));
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "private, no-store");
            assert!(
                response.headers()["vary"]
                    .to_str()
                    .unwrap()
                    .contains("Authorization")
            );
            let body: Value = response.json().await.unwrap();
            let meta = if path.ends_with("groups") {
                &body
            } else {
                &body["groups_page"]
            };
            assert_eq!(meta["total"], allowed.len());
            assert_eq!(body["groups"].as_array().unwrap().len(), 1);
            assert_eq!(body["groups"][0]["code"], *allowed[0]);
            assert!(!body.to_string().contains(&hidden));
            if token != Some(&alice_key) {
                assert!(!body.to_string().contains(&assigned));
            }
            if !path.ends_with("groups") {
                assert_eq!(body["models"][0]["groups"], json!([allowed[0]]));
                assert_eq!(
                    body["models"][0]["chat_endpoints_by_group"]
                        .as_object()
                        .unwrap()
                        .len(),
                    1
                );
            }
            let mut head = env.client.head(env.url(path, &args));
            if let Some(token) = token {
                head = head.bearer_auth(token);
            }
            let head = head.send().await.unwrap();
            assert_eq!(head.status(), 200);
            assert_eq!(head.headers()["cache-control"], "private, no-store");
            assert!(head.bytes().await.unwrap().is_empty());
        }
    }

    // Direct codes, search, model details and pagination cannot reveal hidden groups.
    for path in ["/api/pricing", "/api/pricing/models", "/api/pricing/groups"] {
        let args = if path.ends_with("groups") {
            vec![("code", hidden.as_str()), ("model", env.models[0].as_str())]
        } else {
            vec![
                ("group", hidden.as_str()),
                ("model", env.models[0].as_str()),
            ]
        };
        for token in [None, Some(&alice_key)] {
            let mut request = env.client.get(env.url(path, &args));
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            let body: Value = request.send().await.unwrap().json().await.unwrap();
            assert_eq!(body["groups"], json!([]));
            assert_eq!(body["total"], 0);
            if !path.ends_with("groups") {
                assert_eq!(body["models"], json!([]));
                assert_eq!(body["vendors"], json!([]));
            }
        }
        let invalid = env
            .client
            .get(env.url(path, &[]))
            .bearer_auth("invalid-catalog-fixture")
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), 401);
    }

    // A changed assignment is evaluated from PG on the next read, not a catalog cache.
    sqlx::query("DELETE FROM user_groups WHERE user_id=$1 AND group_code=$2")
        .bind(alice)
        .bind(&assigned)
        .execute(&env.pg)
        .await
        .unwrap();
    let body: Value = env
        .client
        .get(env.url("/api/pricing/groups", &[("q", &env.prefix)]))
        .bearer_auth(&alice_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["groups"][0]["code"], open);
    let default: Value = env
        .client
        .get(env.url("/api/pricing/groups", &[("code", "default")]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(default["groups"][0]["is_default"], true);
}

#[tokio::test]
async fn endpoint_facets_cannot_discover_routes_in_unassigned_private_pools() {
    let env = setup().await;
    let group = format!("{}-private", env.prefix);
    sqlx::query("INSERT INTO channel_pools(pool_code) VALUES($1)")
        .bind(&group)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("INSERT INTO price_groups(group_code,pool_code,group_ratio) VALUES($1,$1,0.5)")
        .bind(&group)
        .execute(&env.pg)
        .await
        .unwrap();
    let (channel, _) = okapi_store::provision::create_channel(
        &env.pg,
        &group,
        "codex",
        "http://127.0.0.1:9/v1",
        "fixture",
        &[&env.models[0]],
        false,
        None,
    )
    .await
    .unwrap();
    okapi_store::admin::set_channel_pool_codes(&env.pg, channel, std::slice::from_ref(&group))
        .await
        .unwrap();
    let (id, token) = user(&env).await;
    let args = [
        ("q", env.prefix.as_str()),
        ("endpoint", "/v1/responses/compact"),
    ];
    for path in ["/api/pricing", "/api/pricing/models"] {
        let body: Value = env
            .client
            .get(env.url(path, &args))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["total"], 0);
        assert_eq!(body["vendors"], json!([]));
    }
    sqlx::query("INSERT INTO user_groups(user_id,group_code,priority) VALUES($1,$2,1)")
        .bind(id)
        .bind(&group)
        .execute(&env.pg)
        .await
        .unwrap();
    let mut scoped = args.to_vec();
    scoped.push(("group", &group));
    let body: Value = env
        .client
        .get(env.url("/api/pricing/models", &scoped))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["models"][0]["groups"], json!([group]));
    assert_eq!(
        env.get(&scoped).await["total"],
        0,
        "anonymous requests cannot acquire an assignment"
    );
}
