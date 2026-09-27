use super::*;

async fn seed_configuration(b: &Bed) {
    // More than the 200-row ceiling, even against a brand-new database.
    for i in 0..205 {
        let code = format!("pgcfg-{}-{i:02}", b.suffix);
        sqlx::query("INSERT INTO channel_pools(pool_code) VALUES($1)")
            .bind(&code)
            .execute(&b.pg)
            .await
            .unwrap();
        if i >= 25 {
            okapi_store::provision::create_api_key(
                &b.pg,
                b.user_id,
                &hash(&format!("key-{code}")),
                "sk-page",
            )
            .await
            .unwrap();
        }
        okapi_store::provision::create_model_ratio(&b.pg, &code, "1", "1", "1")
            .await
            .unwrap();
        sqlx::query("INSERT INTO price_groups(group_code,pool_code) VALUES($1,'default')")
            .bind(&code)
            .execute(&b.pg)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO plans(plan_code,display_name,grant_micro,status) VALUES($1,$1,1,2)",
        )
        .bind(&code)
        .execute(&b.pg)
        .await
        .unwrap();
        sqlx::query("INSERT INTO pricing_rules(rule_code,rule_type,params,enabled) VALUES($1,'discount','{}',false)")
            .bind(&code).execute(&b.pg).await.unwrap();
        sqlx::query("INSERT INTO admin_roles(role_code,display_name) VALUES($1,$1)")
            .bind(&code)
            .execute(&b.pg)
            .await
            .unwrap();
        sqlx::query("INSERT INTO channels(name,provider,status) VALUES($1,'openai',2)")
            .bind(&code)
            .execute(&b.pg)
            .await
            .unwrap();
        let team = okapi_store::provision::create_user(&b.pg, &code)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET kind='team' WHERE id=$1")
            .bind(team)
            .execute(&b.pg)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO team_members(team_user_id,member_user_id,role) VALUES($1,$2,'owner')",
        )
        .bind(team)
        .bind(b.user_id)
        .execute(&b.pg)
        .await
        .unwrap();
    }
}

// Independent SQL counts prevent two identically wrong HTTP pages from passing.
const ADMIN_CASES: [(&str, &str); 9] = [
    (
        "/admin/keys",
        "SELECT count(*) FROM api_keys WHERE deleted_at IS NULL",
    ),
    ("/admin/pools", "SELECT count(*) FROM channel_pools"),
    (
        "/admin/users",
        "SELECT count(*) FROM users WHERE deleted_at IS NULL",
    ),
    ("/admin/models", "SELECT count(*) FROM models"),
    ("/admin/groups", "SELECT count(*) FROM price_groups"),
    ("/admin/plans", "SELECT count(*) FROM plans"),
    ("/admin/pricing/rules", "SELECT count(*) FROM pricing_rules"),
    ("/admin/roles", "SELECT count(*) FROM admin_roles"),
    (
        "/admin/channels",
        "SELECT count(*) FROM channels WHERE deleted_at IS NULL",
    ),
];

async fn check_pages(b: &Bed, path: &str, token: &str, total: i64) {
    assert!(total > 20, "{path}: fixture must cross a page");
    let separator = if path.contains('?') { '&' } else { '?' };
    let checks = [
        (String::new(), 20, 0),
        (format!("{separator}offset=20"), 20, 20),
        (format!("{separator}offset=-1"), 20, 0),
        (format!("{separator}limit=99999"), 200, 0),
        (format!("{separator}limit=0"), 1, 0),
        (format!("{separator}limit=-1"), 1, 0),
        (
            format!("{separator}offset={total}"),
            20,
            usize::try_from(total).unwrap(),
        ),
    ];
    for (query, limit, offset) in checks {
        let url = format!("http://{}{path}{query}", b.console);
        let response = authenticated(b, path, token, &url).send().await.unwrap();
        assert_eq!(response.status(), 200, "{path}{query}");
        let actual: Value = response.json().await.unwrap();
        assert_eq!(
            actual["total"], total,
            "{path}{query}: total is not page length"
        );
        let count = usize::try_from(total)
            .unwrap()
            .saturating_sub(offset)
            .min(limit);
        assert_eq!(
            actual["data"].as_array().unwrap().len(),
            count,
            "{path}{query}"
        );
        let (expected, _) = fetch(b, path, token, limit, offset).await;
        assert_eq!(
            actual, expected,
            "{path}{query}: defaults/clamps must use the same slice"
        );
    }
    let (_, first) = fetch(b, path, token, 20, 0).await;
    let (_, next) = fetch(b, path, token, 20, 20).await;
    let (_, together) = fetch(b, path, token, 40, 0).await;
    assert_eq!(
        first.iter().chain(&next).cloned().collect::<Vec<_>>(),
        together,
        "{path}"
    );
    assert!(
        first.iter().all(|id| !next.contains(id)),
        "{path}: overlapping pages"
    );
}

#[tokio::test]
async fn defaults_are_twenty_for_every_resource_and_offset_only() {
    let b = setup().await;
    seed_configuration(&b).await;
    for (path, sql) in ADMIN_CASES {
        let total: i64 = sqlx::query_scalar(sql).fetch_one(&b.pg).await.unwrap();
        check_pages(&b, path, &b.admin_token, total).await;
    }
    // Both private lists must count only this user's resources, despite the
    // other test users, keys and teams already in the same database.
    check_pages(&b, "/api/me/keys", &b.user_token, 205).await;
    check_pages(&b, "/api/teams", &b.user_token, 205).await;
    // Search and user filters must run before pagination and before COUNT.
    let query = format!("q=pgcfg-{}-", b.suffix);
    for resource in ["models", "channels"] {
        check_pages(
            &b,
            &format!("/admin/{resource}?{query}"),
            &b.admin_token,
            205,
        )
        .await;
    }
    check_pages(
        &b,
        &format!("/admin/keys?user_id={}", b.user_id),
        &b.admin_token,
        205,
    )
    .await;
}
