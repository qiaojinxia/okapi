//! 适配规则的存储层：迁移复制旧设置、发布版本、候选装配三层、渠道复制。
//! sqlx 为每个用例建立并销毁隔离的测试库。
use okapi_compat::Context;
use okapi_store::{
    admin::{self, ChannelPatch},
    channels,
    compat::{self, Scope},
    mutate, provision,
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

fn ctx(vendor: Option<&str>) -> Context<'_> {
    Context {
        vendor,
        host: Some("api.deepseek.com"),
        model: "m",
        client_model: "m",
        dialect: "openai.chat",
        operation: "chat",
        stream: false,
        client: None,
    }
}

fn apply(cand: &channels::ChannelCandidate, body: &Value) -> Value {
    let ctx = ctx(cand.vendor.as_deref());
    let outcome = cand
        .compat
        .apply_request(&ctx, body.to_string().as_bytes())
        .unwrap();
    outcome.body.map_or_else(
        || body.clone(),
        |bytes| serde_json::from_slice(&bytes).unwrap(),
    )
}

async fn publish(pool: &PgPool, scope: Scope, rules: &Value, base: Option<i32>) -> i32 {
    let mut conn = pool.acquire().await.unwrap();
    compat::publish(&mut conn, scope, rules, Some("test"), Some(1), base)
        .await
        .unwrap()
}

/// 迁移文件里标出的「复制旧设置」那一段（测试在种好旧数据后重跑它）。
fn legacy_copy_sql() -> &'static str {
    let migration = include_str!("../migrations/0006_compat_rules.sql");
    let start = migration.find("-- >>> legacy copy").expect("start marker");
    let end = migration.find("-- <<< legacy copy").expect("end marker");
    &migration[start..end]
}

#[sqlx::test(migrations = "./migrations")]
async fn legacy_strip_and_inject_settings_are_copied_into_a_published_channel_rule_set(
    pool: PgPool,
) {
    let id = channel(&pool, "legacy").await;
    let settings = json!({
        "thinking_to_content": true,
        "strip_request_fields": ["user", "stream_options", "model", "a/b"],
        "inject_request_fields": {"thinking": {"type": "disabled"}, "model": "evil"}
    });
    sqlx::query("UPDATE channels SET settings = $2 WHERE id = $1")
        .bind(id)
        .bind(&settings)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        compat::published(&pool, Scope::Channel(id))
            .await
            .unwrap()
            .is_none()
    );

    sqlx::raw_sql(legacy_copy_sql())
        .execute(&pool)
        .await
        .unwrap();
    // 再跑一遍必须是空操作
    sqlx::raw_sql(legacy_copy_sql())
        .execute(&pool)
        .await
        .unwrap();

    let row = compat::published(&pool, Scope::Channel(id))
        .await
        .unwrap()
        .expect("copied");
    assert_eq!(row.version, 1);
    let rules = row.rules.as_array().unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0]["id"], "strip-request-fields");
    assert_eq!(
        rules[0]["do"][0]["paths"],
        json!(["/user", "/stream_options", "/a~1b"]),
        "受保护的 model 丢掉，转义正确"
    );
    assert_eq!(
        rules[0]["billing_critical_ack"], true,
        "剥 stream_options 要带确认"
    );
    assert_eq!(rules[1]["id"], "inject-request-fields");
    assert_eq!(
        rules[1]["do"],
        json!([{"op": "set", "path": "/thinking", "value": {"type": "disabled"}}])
    );
    okapi_compat::validate(&row.rules).expect("copied rules pass the engine's validation");

    // 原键留在 settings 里（只为回滚旧镜像），新代码不读它们
    let kept: Value = sqlx::query_scalar("SELECT settings FROM channels WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, settings);

    // 候选带着规则，行为与旧的先剥后注一致
    let cand = candidate(&pool, id).await;
    let after = apply(
        &cand,
        &json!({"model": "m", "user": "u", "a/b": 1, "stream_options": {"include_usage": true}, "stream": true}),
    );
    assert_eq!(
        after,
        json!({"model": "m", "stream": true, "thinking": {"type": "disabled"}})
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn channels_without_legacy_settings_get_no_rule_set(pool: PgPool) {
    let id = channel(&pool, "plain").await;
    sqlx::query("UPDATE channels SET settings = '{\"strip_request_fields\": [\"user\", 1], \"inject_request_fields\": []}' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(legacy_copy_sql())
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        compat::published(&pool, Scope::Channel(id))
            .await
            .unwrap()
            .is_none()
    );
    let cand = candidate(&pool, id).await;
    assert!(cand.compat.is_empty());
    assert_eq!(cand.vendor, None);
}

#[sqlx::test(migrations = "./migrations")]
async fn publishing_archives_the_previous_version_and_guards_against_stale_bases(pool: PgPool) {
    let id = channel(&pool, "versions").await;
    let scope = Scope::Channel(id);
    let one = json!([{"id": "a", "do": [{"op": "remove", "paths": ["/user"]}]}]);
    let two = json!([{"id": "a", "do": [{"op": "remove", "paths": ["/user", "/metadata"]}]}]);
    assert_eq!(publish(&pool, scope, &one, Some(0)).await, 1);
    assert_eq!(publish(&pool, scope, &two, Some(1)).await, 2);

    let statuses: Vec<(i32, String)> = sqlx::query_as(
        "SELECT version, status FROM rule_sets WHERE scope_id = $1 ORDER BY version",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        statuses,
        [(1, "archived".to_owned()), (2, "published".to_owned())]
    );
    assert_eq!(
        compat::published(&pool, scope)
            .await
            .unwrap()
            .unwrap()
            .rules,
        two
    );

    let mut conn = pool.acquire().await.unwrap();
    let stale = compat::publish(&mut conn, scope, &one, None, None, Some(1)).await;
    assert!(
        matches!(
            stale,
            Err(okapi_store::StoreError::Conflict("rules_version_conflict"))
        ),
        "{stale:?}"
    );
    let still: i32 = sqlx::query_scalar(
        "SELECT version FROM rule_sets WHERE scope_id = $1 AND status = 'published'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(still, 2, "冲突不改动已发布版本");
}

#[sqlx::test(migrations = "./migrations")]
async fn candidates_assemble_vendor_channel_and_guard_layers(pool: PgPool) {
    let id = channel(&pool, "layered").await;
    let other = channel(&pool, "bystander").await;
    let patch = ChannelPatch {
        vendor: Some(Some("builtin:deepseek")),
        ..ChannelPatch::default()
    };
    assert!(admin::patch_channel(&pool, id, patch).await.unwrap());
    publish(
        &pool,
        Scope::Channel(id),
        &json!([{"id": "put-user", "do": [{"op": "set", "path": "/user", "value": "u"}]}]),
        None,
    )
    .await;
    publish(
        &pool,
        Scope::Global,
        &json!([{"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}]),
        None,
    )
    .await;

    let cand = candidate(&pool, id).await;
    assert_eq!(cand.vendor.as_deref(), Some("builtin:deepseek"));
    let labels: Vec<&str> = cand
        .compat
        .layers()
        .iter()
        .map(|l| l.label.as_str())
        .collect();
    assert_eq!(labels.len(), 3, "{labels:?}");
    assert_eq!(labels[0], "builtin:deepseek");
    assert!(
        labels[1].starts_with(&format!("channel#{id}@v")),
        "{labels:?}"
    );
    assert!(labels[2].starts_with("guard@v"), "{labels:?}");
    let body = json!({"model": "m", "messages": [{"role": "developer", "content": "x"}]});
    let after = apply(&cand, &body);
    assert_eq!(
        after,
        json!({"model": "m", "messages": [{"role": "system", "content": "x"}]}),
        "厂商层改了角色，渠道层写入的 user 被护栏删掉"
    );

    // 旁观渠道：没选厂商、没有渠道规则，但护栏照样管到它（唯一全局的一层）
    let bystander = candidate(&pool, other).await;
    let labels: Vec<&str> = bystander
        .compat
        .layers()
        .iter()
        .map(|l| l.label.as_str())
        .collect();
    assert_eq!(labels.len(), 1);
    assert!(labels[0].starts_with("guard@v"));
    assert_eq!(
        apply(&bystander, &json!({"model": "m", "user": "u"})),
        json!({"model": "m"})
    );
    assert_eq!(
        apply(&bystander, &body),
        body,
        "没选厂商的渠道不会被套上厂商层"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn an_invalid_published_set_is_skipped_and_the_other_layers_still_apply(pool: PgPool) {
    let id = channel(&pool, "poisoned").await;
    sqlx::query(
        "INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, rules, published_at)
         VALUES ('compat', 'channel', $1, 1, 'published', $2, now())",
    )
    .bind(id)
    // 写受保护路径：校验器不放行，运行期加载同样不放行
    .bind(json!([{"id": "bad", "do": [{"op": "set", "path": "/model", "value": "x"}]}]))
    .execute(&pool)
    .await
    .unwrap();
    publish(
        &pool,
        Scope::Global,
        &json!([{"id": "no-user", "do": [{"op": "remove", "paths": ["/user"]}]}]),
        None,
    )
    .await;
    let cand = candidate(&pool, id).await;
    let after = apply(&cand, &json!({"model": "m", "user": "u"}));
    assert_eq!(
        after,
        json!({"model": "m"}),
        "坏的渠道层被跳过，护栏照常生效，请求没有失败"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn rule_sets_from_a_newer_schema_are_ignored(pool: PgPool) {
    let id = channel(&pool, "future").await;
    sqlx::query(
        "INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, schema_version, rules, published_at)
         VALUES ('compat', 'channel', $1, 1, 'published', 2, $2, now())",
    )
    .bind(id)
    .bind(json!([{"id": "a", "do": [{"op": "remove", "paths": ["/user"]}]}]))
    .execute(&pool)
    .await
    .unwrap();
    let cand = candidate(&pool, id).await;
    assert!(cand.compat.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn duplicating_a_channel_clones_vendor_and_rules_but_not_retired_settings(pool: PgPool) {
    let id = channel(&pool, "original").await;
    sqlx::query("UPDATE channels SET settings = '{\"strip_request_fields\": [\"user\"], \"thinking_to_content\": true}', vendor = 'builtin:moonshot' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let rules = json!([{"id": "a", "do": [{"op": "remove", "paths": ["/user"]}]}]);
    publish(&pool, Scope::Channel(id), &rules, None).await;

    let copy = mutate::duplicate_channel(&pool, id, "copy")
        .await
        .unwrap()
        .expect("copied");
    let row = compat::published(&pool, Scope::Channel(copy))
        .await
        .unwrap()
        .expect("rules cloned");
    assert_eq!((row.version, row.rules), (1, rules));
    let (vendor, settings): (Option<String>, Value) =
        sqlx::query_as("SELECT vendor, settings FROM channels WHERE id = $1")
            .bind(copy)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(vendor.as_deref(), Some("builtin:moonshot"));
    assert_eq!(settings, json!({"thinking_to_content": true}));
}

#[sqlx::test(migrations = "./migrations")]
async fn the_channel_list_shows_the_vendor_and_hides_retired_settings(pool: PgPool) {
    let id = channel(&pool, "listed").await;
    sqlx::query("UPDATE channels SET settings = '{\"inject_request_fields\": {\"a\": 1}, \"api_version\": \"v1\"}', vendor = 'builtin:xai' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let list = admin::list_channels(
        &pool,
        admin::ChannelFilter::default(),
        okapi_store::listing::Slice::ALL,
    )
    .await
    .unwrap();
    let row = list.page.data.iter().find(|c| c.id == id).expect("row");
    assert_eq!(row.vendor.as_deref(), Some("builtin:xai"));
    assert_eq!(row.settings, json!({"api_version": "v1"}));
}

#[sqlx::test(migrations = "./migrations")]
async fn vendor_patch_has_three_states(pool: PgPool) {
    let id = channel(&pool, "patched").await;
    let vendor_of = |pool: PgPool| async move {
        sqlx::query_scalar::<_, Option<String>>("SELECT vendor FROM channels WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let set = ChannelPatch {
        vendor: Some(Some("builtin:zhipu")),
        ..ChannelPatch::default()
    };
    admin::patch_channel(&pool, id, set).await.unwrap();
    assert_eq!(
        vendor_of(pool.clone()).await.as_deref(),
        Some("builtin:zhipu")
    );
    let untouched = ChannelPatch {
        name: Some("renamed"),
        ..ChannelPatch::default()
    };
    admin::patch_channel(&pool, id, untouched).await.unwrap();
    assert_eq!(
        vendor_of(pool.clone()).await.as_deref(),
        Some("builtin:zhipu"),
        "不带 vendor 就不动"
    );
    let clear = ChannelPatch {
        vendor: Some(None),
        ..ChannelPatch::default()
    };
    admin::patch_channel(&pool, id, clear).await.unwrap();
    assert_eq!(vendor_of(pool).await, None);
}

#[sqlx::test(migrations = "./migrations")]
async fn the_table_constraints_reject_malformed_rows(pool: PgPool) {
    let id = channel(&pool, "constraints").await;
    let bad_vendor = sqlx::query("UPDATE channels SET vendor = 'deepseek' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await;
    assert!(bad_vendor.is_err(), "厂商必须带命名空间");
    let global_with_id = sqlx::query(
        "INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, rules, published_at)
         VALUES ('compat', 'global', 7, 1, 'published', '[]', now())",
    )
    .execute(&pool)
    .await;
    assert!(global_with_id.is_err(), "global 不带 scope_id");
    let not_array = sqlx::query(
        "INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, rules, published_at)
         VALUES ('compat', 'channel', $1, 1, 'published', '{}', now())",
    )
    .bind(id)
    .execute(&pool)
    .await;
    assert!(not_array.is_err(), "rules 必须是数组");
    publish(&pool, Scope::Global, &json!([]), None).await;
    let second_published = sqlx::query(
        "INSERT INTO rule_sets (kind, scope_kind, scope_id, version, status, rules, published_at)
         VALUES ('compat', 'global', NULL, 99, 'published', '[]', now())",
    )
    .execute(&pool)
    .await;
    assert!(
        second_published.is_err(),
        "global 至多一个已发布版本（NULL 也要唯一）"
    );
}
