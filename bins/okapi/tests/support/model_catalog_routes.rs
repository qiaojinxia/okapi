use super::{Env, names, setup};
use serde_json::json;

struct Groups {
    primary: String,
    codex: String,
    fallback: String,
    empty: String,
    codex_channel: i64,
}

async fn routing(env: &Env) -> Groups {
    let groups: Vec<_> = ["a", "b", "f", "e"]
        .map(|s| format!("{}-{s}", env.prefix))
        .into();
    for code in &groups {
        sqlx::query("INSERT INTO channel_pools(pool_code) VALUES($1)")
            .bind(code)
            .execute(&env.pg)
            .await
            .unwrap();
        sqlx::query("INSERT INTO price_groups(group_code,group_ratio,pool_code,self_select) VALUES($1,1,$1,true)")
            .bind(code)
            .execute(&env.pg)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE channel_pools SET fallback_pool_code=$2 WHERE pool_code=$1")
        .bind(&groups[2])
        .bind(&groups[1])
        .execute(&env.pg)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (provider, pool, models) in [
        ("openai", &groups[0], &env.models[..15]),
        ("codex", &groups[1], &env.models[10..]),
    ] {
        let names: Vec<_> = models.iter().map(String::as_str).collect();
        let (id, _) = okapi_store::provision::create_channel(
            &env.pg,
            pool,
            provider,
            "http://127.0.0.1:9/private-upstream",
            "catalog-private-credential",
            &names,
            false,
            None,
        )
        .await
        .unwrap();
        okapi_store::admin::set_channel_pool_codes(&env.pg, id, std::slice::from_ref(pool))
            .await
            .unwrap();
        if provider == "openai" {
            sqlx::query(
                "UPDATE channels SET settings='{\"responses_native\":false}'::jsonb WHERE id=$1",
            )
            .bind(id)
            .execute(&env.pg)
            .await
            .unwrap();
        }
        ids.push(id);
    }
    Groups {
        primary: groups[0].clone(),
        codex: groups[1].clone(),
        fallback: groups[2].clone(),
        empty: groups[3].clone(),
        codex_channel: ids[1],
    }
}

#[tokio::test]
async fn catalog_group_and_endpoint_filters_respect_pool_boundaries() {
    let env = setup().await;
    let groups = routing(&env).await;
    let base = [("q", env.prefix.as_str()), ("limit", "100")];
    for group in [&groups.codex, &groups.fallback] {
        let args = [
            base[0],
            base[1],
            ("group", group),
            ("endpoint", "/v1/responses/compact"),
        ];
        let page = env.get(&args).await;
        assert_eq!(names(&page), env.models[10..]);
        for model in page["models"].as_array().unwrap() {
            assert_eq!(
                model["chat_endpoints_by_group"][group],
                json!(["/v1/responses", "/v1/responses/compact"])
            );
        }
        assert!(!page.to_string().contains("private-upstream"));
        assert!(!page.to_string().contains("catalog-private-credential"));
    }
    assert_eq!(
        env.get(&[
            base[0],
            ("group", &groups.primary),
            ("endpoint", "/v1/responses/compact")
        ])
        .await["total"],
        0
    );
    assert_eq!(
        env.get(&[
            base[0],
            ("group", &groups.codex),
            ("endpoint", "/v1/chat/completions")
        ])
        .await["total"],
        0
    );
    assert_eq!(
        env.get(&[base[0], ("group", &groups.empty)]).await["total"],
        0
    );
    assert_eq!(
        env.get(&[base[0], ("group", "missing-catalog-group")])
            .await["total"],
        0
    );
    let any_group = env
        .get(&[base[0], base[1], ("endpoint", "/v1/chat/completions")])
        .await;
    assert_eq!(names(&any_group), env.models[..15]);
    let scoped = env
        .get(&[
            base[0],
            ("group", &groups.primary),
            ("capability", "vision"),
            ("limit", "2"),
        ])
        .await;
    assert_eq!(scoped["total"], 8);
    assert_eq!(scoped["models"].as_array().unwrap().len(), 2);

    // Disabled and soft-deleted channels cannot supply capabilities to this group.
    for update in [
        "UPDATE channels SET status=2 WHERE id=$1",
        "UPDATE channels SET status=1,deleted_at=now() WHERE id=$1",
    ] {
        sqlx::query(update)
            .bind(groups.codex_channel)
            .execute(&env.pg)
            .await
            .unwrap();
        assert_eq!(
            env.get(&[base[0], ("group", &groups.codex)]).await["total"],
            0
        );
        assert_eq!(
            env.get(&[base[0], ("group", &groups.fallback)]).await["total"],
            0
        );
    }
}
