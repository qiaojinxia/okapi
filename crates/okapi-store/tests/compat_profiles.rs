//! 适配规则 P2 的存储层：草稿与审批、版本历史与回滚、适配档案、档案层的装配、能力事实合并。
//! sqlx 为每个用例建立并销毁隔离的测试库。
use okapi_compat::Context;
use okapi_store::{
    StoreError, channels,
    compat::{self, DraftInput, ProfileInput, Scope},
    provision,
};
use serde_json::{Value, json};
use sqlx::PgPool;

async fn channel(pool: &PgPool, name: &str) -> i64 {
    provision::create_channel(
        pool,
        name,
        "openai_compat",
        "https://api.deepseek.com/v1",
        "sk-test",
        &["m"],
        false,
        None,
    )
    .await
    .unwrap()
    .0
}

async fn candidate(pool: &PgPool, channel_id: i64) -> channels::ChannelCandidate {
    channels::candidates_for_model(pool, "m", &["default"], None)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.channel_id == channel_id)
        .expect("candidate")
}

async fn publish(pool: &PgPool, scope: Scope, rules: &Value, base: Option<i32>) -> i32 {
    let mut conn = pool.acquire().await.unwrap();
    compat::publish(&mut conn, scope, rules, Some("test"), Some(1), base)
        .await
        .unwrap()
}

async fn save_draft(
    pool: &PgPool,
    scope: Scope,
    rules: &Value,
    base: i32,
) -> Result<i32, StoreError> {
    let mut conn = pool.acquire().await.unwrap();
    compat::save_draft(
        &mut conn,
        scope,
        DraftInput {
            rules,
            note: Some("draft note"),
            rationale: Some("because the upstream 400s"),
            actor: Some(7),
            source: "mcp",
            base_version: base,
        },
    )
    .await
}

fn rules_setting(user: &str) -> Value {
    json!([{"id": "u", "do": [{"op": "set", "path": "/user", "value": user}]}])
}

async fn profile(pool: &PgPool, slug: &str, extends: Option<&str>) -> compat::ProfileRow {
    compat::create_profile(
        pool,
        ProfileInput {
            slug,
            name: slug,
            description: Some("test profile"),
            extends,
            hosts: &["relay.example.com".to_owned()],
        },
        Some(1),
    )
    .await
    .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn a_draft_is_single_mutable_and_guarded_by_the_base_version(pool: PgPool) {
    let id = channel(&pool, "drafts").await;
    let scope = Scope::Channel(id);
    assert!(compat::draft(&pool, scope).await.unwrap().is_none());

    let first = save_draft(&pool, scope, &rules_setting("a"), 0)
        .await
        .unwrap();
    // 再存是覆盖同一个草稿，不是新增
    let second = save_draft(&pool, scope, &rules_setting("b"), 0)
        .await
        .unwrap();
    assert_eq!(first, second);
    let draft = compat::draft(&pool, scope).await.unwrap().expect("draft");
    assert_eq!(draft.status, "draft");
    assert_eq!(draft.rules, rules_setting("b"));
    assert_eq!(draft.source, "mcp");
    assert_eq!(
        draft.rationale.as_deref(),
        Some("because the upstream 400s")
    );
    assert_eq!(draft.created_by, Some(7));
    assert_eq!(draft.base_version, Some(0));
    assert!(draft.published_at.is_none());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM rule_sets WHERE scope_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    // 起草时的已发布版本对不上：拒绝
    assert!(matches!(
        save_draft(&pool, scope, &rules_setting("x"), 3).await,
        Err(StoreError::Conflict("rules_version_conflict"))
    ));

    // 草稿不影响线上：没发布前候选里没有这层规则
    assert!(candidate(&pool, id).await.compat.is_empty());

    // 期间有人直接发布了：草稿过期，发布草稿被拒，草稿还在
    let direct = publish(&pool, scope, &rules_setting("direct"), Some(0)).await;
    let mut conn = pool.acquire().await.unwrap();
    assert!(matches!(
        compat::publish_draft(&mut conn, scope, Some(2)).await,
        Err(StoreError::Conflict("rules_version_conflict"))
    ));
    drop(conn);
    assert!(compat::draft(&pool, scope).await.unwrap().is_some());

    // 在新基础上重新起草，再发布：旧版本归档，草稿成为新的已发布版本
    save_draft(&pool, scope, &rules_setting("rebased"), direct)
        .await
        .unwrap();
    assert_eq!(
        compat::draft(&pool, scope)
            .await
            .unwrap()
            .unwrap()
            .base_version,
        Some(direct)
    );
    let mut conn = pool.acquire().await.unwrap();
    let published = compat::publish_draft(&mut conn, scope, Some(2))
        .await
        .unwrap()
        .expect("published");
    drop(conn);
    assert!(published > direct);
    let live = compat::published(&pool, scope)
        .await
        .unwrap()
        .expect("live");
    assert_eq!(
        (live.version, live.rules),
        (published, rules_setting("rebased"))
    );
    assert_eq!(live.published_by, Some(2));
    assert!(live.published_at.is_some());
    assert!(
        compat::draft(&pool, scope).await.unwrap().is_none(),
        "发布后草稿没了"
    );
    let archived = compat::version(&pool, scope, direct)
        .await
        .unwrap()
        .expect("old");
    assert_eq!(archived.status, "archived");

    // 没有草稿：发布返回 None；丢弃只有第一次返回 true
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        compat::publish_draft(&mut conn, scope, Some(2))
            .await
            .unwrap()
            .is_none()
    );
    save_draft(&pool, scope, &rules_setting("later"), published)
        .await
        .unwrap();
    assert!(compat::discard_draft(&mut conn, scope).await.unwrap());
    assert!(!compat::discard_draft(&mut conn, scope).await.unwrap());
}

#[sqlx::test(migrations = "./migrations")]
async fn history_lists_every_state_and_rollback_republishes_old_content(pool: PgPool) {
    let id = channel(&pool, "history").await;
    let scope = Scope::Channel(id);
    let v1 = publish(&pool, scope, &rules_setting("one"), Some(0)).await;
    let v2 = publish(&pool, scope, &rules_setting("two"), Some(v1)).await;
    let v3 = save_draft(&pool, scope, &rules_setting("three"), v2)
        .await
        .unwrap();

    let history = compat::versions(&pool, scope, 50).await.unwrap();
    let shape: Vec<(i32, &str)> = history
        .iter()
        .map(|r| (r.version, r.status.as_str()))
        .collect();
    assert_eq!(shape, [(v3, "draft"), (v2, "published"), (v1, "archived")]);
    assert_eq!(compat::versions(&pool, scope, 1).await.unwrap().len(), 1);

    let mut conn = pool.acquire().await.unwrap();
    let v4 = compat::rollback(&mut conn, scope, v1, Some(9))
        .await
        .unwrap()
        .expect("rolled back");
    drop(conn);
    assert!(v4 > v3, "版本号只增");
    let live = compat::published(&pool, scope)
        .await
        .unwrap()
        .expect("live");
    assert_eq!(
        (live.version, live.rules.clone()),
        (v4, rules_setting("one"))
    );
    assert_eq!(live.note.as_deref(), Some("回滚到版本 1"));
    assert_eq!(
        compat::version(&pool, scope, v2)
            .await
            .unwrap()
            .unwrap()
            .status,
        "archived"
    );
    assert!(
        compat::draft(&pool, scope).await.unwrap().is_some(),
        "回滚不动草稿"
    );

    // 不存在的版本、草稿版本都不能回滚到
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        compat::rollback(&mut conn, scope, 99, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        compat::rollback(&mut conn, scope, v3, None)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn profiles_have_unique_slugs_and_cannot_be_deleted_while_in_use(pool: PgPool) {
    let acme = profile(&pool, "acme", Some("builtin:deepseek")).await;
    assert_eq!(acme.extends.as_deref(), Some("builtin:deepseek"));
    assert_eq!(acme.hosts, ["relay.example.com"]);
    assert!(matches!(
        compat::create_profile(
            &pool,
            ProfileInput {
                slug: "acme",
                name: "dup",
                description: None,
                extends: None,
                hosts: &[]
            },
            None
        )
        .await,
        Err(StoreError::Conflict("profile_slug_taken"))
    ));
    assert_eq!(compat::list_profiles(&pool).await.unwrap().len(), 1);
    assert_eq!(
        compat::profile_by_slug(&pool, "acme")
            .await
            .unwrap()
            .unwrap()
            .id,
        acme.id
    );
    assert!(
        compat::profile_by_slug(&pool, "nope")
            .await
            .unwrap()
            .is_none()
    );

    // 部分更新：不给的不动，Some(None) 清空
    let renamed =
        compat::update_profile(&pool, acme.id, Some("Acme Relay"), None, Some(None), None)
            .await
            .unwrap()
            .expect("updated");
    assert_eq!(renamed.name, "Acme Relay");
    assert_eq!(renamed.description.as_deref(), Some("test profile"));
    assert_eq!(renamed.extends, None);
    assert_eq!(renamed.hosts, ["relay.example.com"]);
    assert!(
        compat::update_profile(&pool, 9999, Some("x"), None, None, None)
            .await
            .unwrap()
            .is_none()
    );

    // 规则集随档案；被渠道引用时删不掉
    publish(&pool, Scope::Profile(acme.id), &rules_setting("p"), Some(0)).await;
    let id = channel(&pool, "uses-acme").await;
    sqlx::query("UPDATE channels SET vendor = 'custom:acme' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        compat::profile_usage(&pool).await.unwrap().get("acme"),
        Some(&1)
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        compat::delete_profile(&mut tx, acme.id).await,
        Err(StoreError::Conflict("profile_in_use"))
    ));
    tx.rollback().await.unwrap();
    assert!(
        compat::published(&pool, Scope::Profile(acme.id))
            .await
            .unwrap()
            .is_some()
    );

    sqlx::query("UPDATE channels SET vendor = NULL WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(compat::delete_profile(&mut tx, acme.id).await.unwrap());
    assert!(!compat::delete_profile(&mut tx, acme.id).await.unwrap());
    tx.commit().await.unwrap();
    assert!(compat::list_profiles(&pool).await.unwrap().is_empty());
    assert!(
        compat::published(&pool, Scope::Profile(acme.id))
            .await
            .unwrap()
            .is_none(),
        "规则集随档案删除"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn the_table_constraints_reject_malformed_profiles_and_drafts(pool: PgPool) {
    for (slug, extends) in [
        ("Bad Slug", None),
        ("-lead", None),
        ("ok", Some("deepseek")),
        ("ok", Some("custom:x")),
    ] {
        let result =
            sqlx::query("INSERT INTO compat_profiles (slug, name, extends) VALUES ($1, 'n', $2)")
                .bind(slug)
                .bind(extends)
                .execute(&pool)
                .await;
        assert!(result.is_err(), "{slug} {extends:?}");
    }
    let too_many: Vec<String> = (0..17).map(|i| format!("h{i}.example.com")).collect();
    assert!(
        sqlx::query("INSERT INTO compat_profiles (slug, name, hosts) VALUES ('many', 'n', $1)")
            .bind(&too_many)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, rules, source) VALUES ('compat', 'global', NULL, 1, 'draft', '[]', 'robot')")
            .execute(&pool)
            .await
            .is_err(),
        "source 只认 admin / mcp / system"
    );
    // 同一范围两个草稿：唯一索引拦下
    let insert = "INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, rules) VALUES ('compat', 'global', NULL, $1, 'draft', '[]')";
    sqlx::query(insert).bind(1).execute(&pool).await.unwrap();
    assert!(sqlx::query(insert).bind(2).execute(&pool).await.is_err());
}

#[sqlx::test(migrations = "./migrations")]
async fn a_channel_using_a_profile_gets_vendor_profile_channel_and_guard_layers(pool: PgPool) {
    let acme = profile(&pool, "acme", Some("builtin:deepseek")).await;
    publish(
        &pool,
        Scope::Profile(acme.id),
        &rules_setting("profile"),
        Some(0),
    )
    .await;
    let id = channel(&pool, "acme-channel").await;
    sqlx::query("UPDATE channels SET vendor = 'custom:acme' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    publish(
        &pool,
        Scope::Channel(id),
        &rules_setting("channel"),
        Some(0),
    )
    .await;
    publish(
        &pool,
        Scope::Global,
        &json!([{"id": "no-metadata", "do": [{"op": "remove", "paths": ["/metadata"]}]}]),
        Some(0),
    )
    .await;

    let cand = candidate(&pool, id).await;
    assert_eq!(cand.vendor.as_deref(), Some("custom:acme"));
    let labels: Vec<&str> = cand
        .compat
        .layers()
        .iter()
        .map(|l| l.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "builtin:deepseek",
            "custom:acme@v1",
            format!("channel#{id}@v1").as_str(),
            "guard@v1"
        ]
    );
    // 同 id 的规则：渠道层（300）压过档案层（200）；档案叠加的厂商层（严格 role）也在
    let ctx = Context {
        vendor: Some("custom:acme"),
        host: Some("api.deepseek.com"),
        model: "m",
        client_model: "m",
        dialect: "openai.chat",
        operation: "chat",
        stream: false,
        client: None,
    };
    let body = json!({"model": "m", "metadata": {"a": 1}, "messages": [{"role": "developer", "content": "hi"}]});
    let outcome = cand
        .compat
        .apply_request(&ctx, body.to_string().as_bytes())
        .unwrap();
    let after: Value = serde_json::from_slice(&outcome.body.expect("changed")).unwrap();
    assert_eq!(after["user"], "channel");
    assert_eq!(after["messages"][0]["role"], "system");
    assert!(after.get("metadata").is_none());
    assert!(
        cand.compat
            .shadowed()
            .iter()
            .any(|s| s.id == "u" && s.reason == "overridden")
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn a_profile_without_rules_still_brings_its_extended_vendor_and_a_missing_profile_brings_nothing(
    pool: PgPool,
) {
    let acme = profile(&pool, "acme", Some("builtin:moonshot")).await;
    let id = channel(&pool, "bare-profile").await;
    sqlx::query("UPDATE channels SET vendor = 'custom:acme' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let cand = candidate(&pool, id).await;
    let labels: Vec<&str> = cand
        .compat
        .layers()
        .iter()
        .map(|l| l.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["builtin:moonshot"],
        "档案还没发布规则，只有它叠加的厂商"
    );

    // 档案被手工删掉（绕过控制台检查）：厂商层不套，渠道层照常
    sqlx::query("DELETE FROM compat_profiles WHERE id = $1")
        .bind(acme.id)
        .execute(&pool)
        .await
        .unwrap();
    publish(
        &pool,
        Scope::Channel(id),
        &rules_setting("channel"),
        Some(0),
    )
    .await;
    let cand = candidate(&pool, id).await;
    let labels: Vec<&str> = cand
        .compat
        .layers()
        .iter()
        .map(|l| l.label.as_str())
        .collect();
    assert_eq!(labels, [format!("channel#{id}@v1").as_str()]);
}

#[sqlx::test(migrations = "./migrations")]
async fn capability_facts_from_rules_are_merged_into_the_candidate_and_the_channel_wins(
    pool: PgPool,
) {
    let acme = profile(&pool, "acme", None).await;
    publish(
        &pool,
        Scope::Profile(acme.id),
        &json!([
            {"id": "caps", "phase": "capability", "select": {"model": ["m"]},
             "do": [{"op": "capability", "facts": {"vision": false, "tools": true}}]},
            {"id": "other-model", "phase": "capability", "select": {"model": ["other"]},
             "do": [{"op": "capability", "facts": {"realtime": false}}]}
        ]),
        Some(0),
    )
    .await;
    let id = channel(&pool, "caps").await;
    sqlx::query("UPDATE channels SET vendor = 'custom:acme', capabilities = $2 WHERE id = $1")
        .bind(id)
        .bind(json!({"tools": false}))
        .execute(&pool)
        .await
        .unwrap();
    let cand = candidate(&pool, id).await;
    assert_eq!(
        cand.capabilities,
        json!({"tools": false, "vision": false}),
        "渠道声明过的 tools 优先；规则补上渠道没说的 vision；别的模型的规则不套"
    );
}

#[test]
fn merging_capabilities_keeps_what_the_channel_declared() {
    use std::collections::BTreeMap;
    let facts = BTreeMap::from([("vision".to_owned(), false), ("tools".to_owned(), true)]);
    assert_eq!(
        compat::merge_capabilities(json!({"tools": false, "x": 1}), &facts),
        json!({"tools": false, "x": 1, "vision": false})
    );
    assert_eq!(
        compat::merge_capabilities(json!({}), &facts),
        json!({"vision": false, "tools": true})
    );
    assert_eq!(
        compat::merge_capabilities(json!(null), &facts),
        json!({"vision": false, "tools": true})
    );
    assert_eq!(
        compat::merge_capabilities(json!({"a": true}), &BTreeMap::new()),
        json!({"a": true})
    );
}

#[test]
fn hosts_are_lowercased_and_garbage_has_none() {
    assert_eq!(
        compat::host_of(Some("https://API.DeepSeek.com/v1")).as_deref(),
        Some("api.deepseek.com")
    );
    assert_eq!(compat::host_of(Some("not a url")), None);
    assert_eq!(compat::host_of(None), None);
    assert_eq!(compat::profile_slug("custom:acme"), Some("acme"));
    assert_eq!(compat::profile_slug("builtin:deepseek"), None);
}
