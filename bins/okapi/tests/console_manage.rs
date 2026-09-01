//! 管理面接口清单验收（IMPLEMENTATION §11.6）：目标要求的六类接口面 + 权限分级。
//!
//! 覆盖：模型配置/分组/套餐/兑换码/令牌/设置列表读、渠道批量与复制、
//! 用户统一管理动作（含 super_admin 保护与令牌吊销）、占用冲突 409、
//! 设置敏感键脱敏、权限点清单、统计端点（CH 启用查 MV / 未启用 fail-closed 501）。

use axum::Router;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

struct Env {
    pg: PgPool,
    console: SocketAddr,
    suffix: String,
    admin_token: String,
    plain_token: String,
    victim_id: i64,
    victim_token: String,
    channel_id: i64,
    model: String,
    group: String,
    ch_enabled: bool,
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let ch_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let admin_id = okapi_store::provision::create_user(&pg, &format!("mg-a-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-mg-a-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-mg-a")
        .await
        .unwrap();

    // 普通用户：验证权限分级（一律 403）
    let plain_id = okapi_store::provision::create_user(&pg, &format!("mg-p-{suffix}"))
        .await
        .unwrap();
    let plain_token = format!("sk-okapi-mg-p-{suffix}");
    okapi_store::provision::create_api_key(&pg, plain_id, &hash(&plain_token), "sk-mg-p")
        .await
        .unwrap();

    // 被管理对象：验证封禁连带吊销令牌
    let victim_id = okapi_store::provision::create_user(&pg, &format!("mg-v-{suffix}"))
        .await
        .unwrap();
    let victim_token = format!("sk-okapi-mg-v-{suffix}");
    okapi_store::provision::create_api_key(&pg, victim_id, &hash(&victim_token), "sk-mg-v")
        .await
        .unwrap();

    let model = format!("m-mg-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1.5", "2", "0.1")
        .await
        .unwrap();
    let group = format!("g-mg-{suffix}");
    okapi_store::admin::upsert_price_group(&pg, &group, "0.9", "测试组", None)
        .await
        .unwrap();

    let (channel_id, _key_id) = okapi_store::provision::create_channel(
        &pg,
        &format!("mg-ch-{suffix}"),
        "openai",
        "https://api.openai.com/v1",
        "mock-credential",
        &[model.as_str()],
        false,
        None,
    )
    .await
    .unwrap();

    let state = gateway::build_state(
        &database_url,
        &redis_url,
        "test-node",
        ch_url.as_deref(),
        None,
    )
    .await
    .unwrap();
    let ch_enabled = state.ch.is_some();
    let console = serve(console::router(state)).await;

    Env {
        pg,
        console,
        suffix,
        admin_token,
        plain_token,
        victim_id,
        victim_token,
        channel_id,
        model,
        group,
        ch_enabled,
    }
}

async fn req(
    method: reqwest::Method,
    addr: SocketAddr,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let client = reqwest::Client::new();
    let mut b = client
        .request(method, format!("http://{addr}{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        b = b.json(&body);
    }
    let resp = b.send().await.unwrap();
    let status = resp.status().as_u16();
    let value: Value = resp.json().await.unwrap_or(Value::Null);
    (status, value)
}

async fn get(addr: SocketAddr, path: &str, token: &str) -> (u16, Value) {
    req(reqwest::Method::GET, addr, path, token, None).await
}

#[tokio::test]
// 接口清单逐资源验收，一体断言便于对照 §11.6
#[allow(clippy::too_many_lines)]
async fn admin_list_surface_covers_every_resource() {
    let env = setup().await;
    let t = &env.admin_token;

    // ---- 模型配置列表（四轴倍率 text 精确出库）----
    let (status, body) = get(env.console, "/admin/models", t).await;
    assert_eq!(status, 200);
    let mine = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model_name"] == env.model.as_str())
        .expect("新建模型必须出现在列表");
    assert_eq!(mine["pricing_mode"], "ratio");
    assert_eq!(mine["model_ratio"], "1.500000");
    assert_eq!(mine["completion_ratio"], "2.000000");
    assert_eq!(mine["cache_ratio"], "0.1000");
    assert_eq!(
        mine["cache_write_ratio"], "1.0000",
        "缓存写入轴缺省 1.0 必须可见"
    );

    // ---- 分组列表（含占用计数，供删除前检查）----
    let (status, body) = get(env.console, "/admin/groups", t).await;
    assert_eq!(status, 200);
    let g = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group_code"] == env.group.as_str())
        .expect("分组必须在列表");
    assert_eq!(g["group_ratio"], "0.9000");
    assert_eq!(g["user_count"], 0);
    assert_eq!(g["channel_count"], 0);

    // ---- 令牌列表：管理员跨用户 + 按用户过滤 + 只回前缀 ----
    let (status, body) = get(
        env.console,
        &format!("/admin/keys?user_id={}", env.victim_id),
        t,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["total"], 1);
    let key = &body["data"][0];
    assert_eq!(key["username"], format!("mg-v-{}", env.suffix));
    assert_eq!(key["key_prefix"], "sk-mg-v");
    assert!(
        !key.as_object().unwrap().contains_key("key_hash"),
        "列表不得暴露哈希"
    );
    // limit 上限钳制：传超大值不得放大扫描
    let (status, body) = get(env.console, "/admin/keys?limit=99999", t).await;
    assert_eq!(status, 200);
    assert!(
        body["data"].as_array().unwrap().len() <= 200,
        "limit 必须被钳制到 MAX_PAGE"
    );

    // ---- 套餐 / 兑换码列表 ----
    let (status, body) = get(env.console, "/admin/plans", t).await;
    assert_eq!(status, 200);
    assert!(body["data"].is_array());
    let (status, body) = get(env.console, "/admin/redemptions?limit=10", t).await;
    assert_eq!(status, 200);
    assert!(body["data"].is_array() && body["total"].is_i64());

    // ---- 权限点清单（前端角色编辑器数据源）----
    let (status, body) = get(env.console, "/admin/permissions", t).await;
    assert_eq!(status, 200);
    let perms: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for expect in [
        "channel.write",
        "pricing.read",
        "pricing.write",
        "user.read",
        "user.manage",
        "billing.read",
        "settings.read",
        "role.manage",
    ] {
        assert!(perms.contains(&expect), "权限点清单缺 {expect}");
    }

    // ---- 系统设置：敏感键脱敏 ----
    let secret_key = format!("epay_key_{}", env.suffix);
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ($1, $2)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        secret_key,
        json!("super-secret-value")
    )
    .execute(&env.pg)
    .await
    .unwrap();
    let (status, body) = get(env.console, "/admin/settings", t).await;
    assert_eq!(status, 200);
    let s = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == secret_key.as_str())
        .expect("设置项必须在列表");
    assert_eq!(s["is_secret"], true);
    assert_eq!(s["value"], Value::Null, "敏感值必须脱敏");
    assert_eq!(s["configured"], true, "但要告知已配置");
    assert!(
        !serde_json::to_string(&body)
            .unwrap()
            .contains("super-secret-value"),
        "明文绝不可出现在响应中"
    );

    // ---- 权限分级：普通用户对管理面一律 403 ----
    for path in [
        "/admin/models",
        "/admin/groups",
        "/admin/plans",
        "/admin/keys",
        "/admin/settings",
        "/admin/permissions",
        "/admin/redemptions",
        "/admin/stats/overview",
    ] {
        let (status, _) = get(env.console, path, &env.plain_token).await;
        assert_eq!(status, 403, "{path} 必须拒绝普通用户");
    }
}

#[tokio::test]
// 渠道批量/复制 + 用户与令牌管理动作 + 占用冲突
#[allow(clippy::too_many_lines)]
async fn channel_batch_and_user_actions() {
    let env = setup().await;
    let t = &env.admin_token;
    let id = env.channel_id;

    // ---- 复制渠道：连带 key，默认停用 ----
    let dup_name = format!("mg-dup-{}", env.suffix);
    let (status, body) = req(
        reqwest::Method::POST,
        env.console,
        &format!("/admin/channels/{id}/duplicate"),
        t,
        Some(json!({"name": dup_name})),
    )
    .await;
    assert_eq!(status, 200);
    let dup_id = body["id"].as_i64().unwrap();
    assert_eq!(body["status"], 2, "复制体默认停用，避免半配置进调度");
    let dup_keys = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM channel_keys WHERE channel_id = $1"#,
        dup_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(dup_keys, 1, "key 必须一并克隆");

    // ---- 批量启停 ----
    let (status, body) = req(
        reqwest::Method::POST,
        env.console,
        "/admin/channels/batch",
        t,
        Some(json!({"ids": [id, dup_id], "action": "enable"})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["affected"], 2);
    let enabled = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM channels
           WHERE id = ANY($1) AND status = 1"#,
        &[id, dup_id][..]
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(enabled, 2);

    // 非法 action 必须 400（不静默放过）
    let (status, _) = req(
        reqwest::Method::POST,
        env.console,
        "/admin/channels/batch",
        t,
        Some(json!({"ids": [id], "action": "drop-table"})),
    )
    .await;
    assert_eq!(status, 400);
    // 空 ids 同样拒绝
    let (status, _) = req(
        reqwest::Method::POST,
        env.console,
        "/admin/channels/batch",
        t,
        Some(json!({"ids": [], "action": "enable"})),
    )
    .await;
    assert_eq!(status, 400);

    // ---- 批量软删：渠道与其 key 同时停用 ----
    let (status, body) = req(
        reqwest::Method::POST,
        env.console,
        "/admin/channels/batch",
        t,
        Some(json!({"ids": [dup_id], "action": "delete"})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["affected"], 1);
    let gone = sqlx::query!(
        r#"SELECT deleted_at, status FROM channels WHERE id = $1"#,
        dup_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert!(gone.deleted_at.is_some(), "软删保留行以维持账本外键");
    assert_eq!(gone.status, 2);
    let key_status = sqlx::query_scalar!(
        r#"SELECT status FROM channel_keys WHERE channel_id = $1"#,
        dup_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(key_status, 2, "key 必须同步停用，防调度取到孤儿");

    // ---- 分组占用冲突 409（不静默解绑，避免计费口径突变）----
    okapi_store::admin::add_user_group(&env.pg, env.victim_id, &env.group)
        .await
        .unwrap();
    let (status, body) = req(
        reqwest::Method::DELETE,
        env.console,
        &format!("/admin/groups/{}", env.group),
        t,
        None,
    )
    .await;
    assert_eq!(status, 409, "被占用的分组不得删除");
    assert_eq!(body["error"]["code"], "group_in_use");

    // default 分组永不可删
    let (status, body) = req(
        reqwest::Method::DELETE,
        env.console,
        "/admin/groups/default",
        t,
        None,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["error"]["code"], "group_is_default");

    // 不存在的资源 → 404
    let (status, _) = req(
        reqwest::Method::DELETE,
        env.console,
        "/admin/groups/no-such-group-xyz",
        t,
        None,
    )
    .await;
    assert_eq!(status, 404);

    // ---- 用户统一管理动作：封禁连带吊销令牌 ----
    let (status, _) = req(
        reqwest::Method::POST,
        env.console,
        &format!("/admin/users/{}/manage", env.victim_id),
        t,
        Some(json!({"action": "ban"})),
    )
    .await;
    assert_eq!(status, 200);
    let after = sqlx::query!(
        r#"SELECT u.status AS user_status,
                  (SELECT COUNT(*)::bigint FROM api_keys k
                    WHERE k.user_id = u.id AND k.status = 1) AS "active_keys!"
           FROM users u WHERE u.id = $1"#,
        env.victim_id
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(after.user_status, 2);
    assert_eq!(
        after.active_keys, 0,
        "封禁必须吊销令牌，否则已发出的 key 仍能打数据面"
    );
    let (status, _) = get(env.console, "/api/me", &env.victim_token).await;
    assert_eq!(status, 401, "吊销后必须立刻失效（含鉴权缓存刷新）");

    // 解封后恢复可用
    let (status, _) = req(
        reqwest::Method::POST,
        env.console,
        &format!("/admin/users/{}/manage", env.victim_id),
        t,
        Some(json!({"action": "unban"})),
    )
    .await;
    assert_eq!(status, 200);

    // 非法动作 400
    let (status, _) = req(
        reqwest::Method::POST,
        env.console,
        &format!("/admin/users/{}/manage", env.victim_id),
        t,
        Some(json!({"action": "nuke"})),
    )
    .await;
    assert_eq!(status, 400);

    // 不可作用于自己
    let admin_id = sqlx::query_scalar!(
        r#"SELECT id FROM users WHERE username = $1"#,
        format!("mg-a-{}", env.suffix)
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    let (status, body) = req(
        reqwest::Method::POST,
        env.console,
        &format!("/admin/users/{admin_id}/manage"),
        t,
        Some(json!({"action": "demote"})),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["param"], "self_target");

    // super_admin 受保护（防互踢导致站点失去最高权限）
    let other_super =
        okapi_store::provision::create_user(&env.pg, &format!("mg-s2-{}", env.suffix))
            .await
            .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", other_super)
        .execute(&env.pg)
        .await
        .unwrap();
    let (status, body) = req(
        reqwest::Method::POST,
        env.console,
        &format!("/admin/users/{other_super}/manage"),
        t,
        Some(json!({"action": "ban"})),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["error"]["param"], "super_admin_protected");

    // ---- 模型删除需提示重新发布 epoch ----
    let (status, body) = req(
        reqwest::Method::DELETE,
        env.console,
        &format!("/admin/models/{}", env.model),
        t,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body["requires_publish"], true,
        "PriceBook 是编译期快照，删除后必须提示发布"
    );
}

#[tokio::test]
async fn stats_surface_exposes_clickhouse_views() {
    let env = setup().await;
    let t = &env.admin_token;
    for path in [
        "/admin/stats/overview",
        "/admin/stats/overview?days=30",
        "/admin/stats/models?days=7",
        "/admin/stats/channels?days=1&limit=5",
        "/admin/stats/margin?days=3",
    ] {
        let (status, body) = get(env.console, path, t).await;
        if env.ch_enabled {
            assert_eq!(status, 200, "{path} 应返回聚合结果：{body}");
        } else {
            // CH 未启用时 fail-closed：501 + error_code，计费与账本不受影响
            assert_eq!(status, 501, "{path} 未启用 CH 时须 501");
            assert_eq!(body["error"]["code"], "stats_disabled");
        }
    }

    if env.ch_enabled {
        let (_, body) = get(env.console, "/admin/stats/overview", t).await;
        // 站点 KPI 必备字段：毛利与活跃用户是 margin 端点未覆盖的增量价值
        for field in ["requests", "amount_micro", "margin_micro", "active_users"] {
            assert!(
                body["window"][field].is_i64(),
                "overview.window 缺字段 {field}：{body}"
            );
            assert!(
                body["today"][field].is_i64(),
                "overview.today 缺字段 {field}"
            );
        }
        assert_eq!(body["days"], 7, "缺省窗口 7 天");
        // 窗口参数钳制：超大 days 收敛到 90
        let (_, body) = get(env.console, "/admin/stats/overview?days=9999", t).await;
        assert_eq!(body["days"], 90);
    }

    // 用户自助按日统计：只看自己，无需管理权限
    let (status, body) = get(env.console, "/api/me/stats/daily?days=3", &env.plain_token).await;
    if env.ch_enabled {
        assert_eq!(status, 200);
        assert!(body["data"].is_array());
        assert_eq!(body["days"], 3);
    } else {
        assert_eq!(status, 501);
    }
}
