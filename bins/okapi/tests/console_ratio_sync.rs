//! 上游倍率在线同步（IMPLEMENTATION §11.36）：三种源形状 → 差异表三态 → 勾选应用只改选中轴。
//! 依赖 .env（scripts/dev-deps.sh up）；把 ssrf_policy 放开以允许打本机 mock。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;
use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

struct Env {
    pg: PgPool,
    console: SocketAddr,
    mock: SocketAddr,
    admin_token: String,
    /// 本地已配倍率的模型（1.25 / 4 / 0.5）。
    ratio_model: String,
    /// 本地按次模型（$0.04）。
    per_call_model: String,
    /// 源里有、本地没有的模型。
    new_model: String,
}

async fn spawn_mock(ratio_model: String, per_call_model: String, new_model: String) -> SocketAddr {
    let (r1, p1, n1) = (
        ratio_model.clone(),
        per_call_model.clone(),
        new_model.clone(),
    );
    let ratio_config = move || {
        json!({"success": true, "data": {
            // 与本地完全相同（1.25 / 4 / 0.5）
            "model_ratio": {&r1: 1.25, &n1: 2},
            "completion_ratio": {&r1: 4},
            "cache_ratio": {&r1: "0.5"},
            // 按次模型：源给 0.05（本地 0.04）
            "model_price": {&p1: 0.05}
        }})
    };
    let (r2, n2) = (ratio_model.clone(), new_model.clone());
    let newapi_pricing = move || {
        json!({"success": true, "data": [
            // model_ratio 涨到 1.5、completion 同
            {"model_name": &r2, "quota_type": 0, "model_ratio": 1.5, "completion_ratio": 4},
            {"model_name": &n2, "quota_type": 0, "model_ratio": "3", "completion_ratio": 8}
        ]})
    };
    // 第三种形状：另一台 Okapi 的 /api/pricing（`{models: [...]}`，按次价是 micro 整数）。
    // 倍率给规范化后与本地相同的字面量（1.250000 == 1.25），缓存倍率给不同值。
    let (r3, p3) = (ratio_model.clone(), per_call_model.clone());
    let okapi_pricing = move || {
        json!({"models": [
            {"model": &r3, "mode": "ratio", "model_ratio": "1.250000",
             "completion_ratio": "4", "cache_ratio": "0.75"},
            {"model": &p3, "mode": "per_call", "per_call_price_micro": 60_000}
        ], "groups": []})
    };
    let router = Router::new()
        .route(
            "/api/okapi-pricing",
            get(move || {
                let body = okapi_pricing();
                async move { axum::Json(body).into_response() }
            }),
        )
        .route(
            "/api/ratio_config",
            get(move || {
                let body = ratio_config();
                async move { axum::Json(body).into_response() }
            }),
        )
        .route(
            "/api/pricing",
            get(move || {
                let body = newapi_pricing();
                async move { axum::Json(body).into_response() }
            }),
        )
        .route(
            "/not-json",
            get(|| async { ([("content-type", "text/html")], "<html>").into_response() }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

async fn setup() -> Env {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('ssrf_policy', $1)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#,
        json!({"allow_http": true, "allow_private": true})
    )
    .execute(&pg)
    .await
    .unwrap();

    let suffix = Uuid::new_v4().simple().to_string();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("rs-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-rs-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(admin_token.as_bytes()))
    };
    okapi_store::provision::create_api_key(&pg, admin_id, &hash, "sk-okapi-rs")
        .await
        .unwrap();

    let ratio_model = format!("rs-ratio-{}", &suffix[..10]);
    let per_call_model = format!("rs-call-{}", &suffix[..10]);
    let new_model = format!("rs-new-{}", &suffix[..10]);
    okapi_store::provision::create_model_ratio(&pg, &ratio_model, "1.25", "4", "0.5")
        .await
        .unwrap();
    okapi_store::admin::upsert_model_per_call(&pg, &per_call_model, 40_000)
        .await
        .unwrap();

    let mock = spawn_mock(
        ratio_model.clone(),
        per_call_model.clone(),
        new_model.clone(),
    )
    .await;
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Env {
        pg,
        console: console_addr,
        mock,
        admin_token,
        ratio_model,
        per_call_model,
        new_model,
    }
}

async fn post(env: &Env, path: &str, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(&env.admin_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn fetch_builds_three_state_differences_and_reports_bad_sources() {
    let env = setup().await;
    let (status, body) = post(
        &env,
        "/admin/pricing/sync/fetch",
        json!({"sources": [
            {"name": "cfg", "url": format!("http://{}/api/ratio_config", env.mock)},
            {"name": "napi", "url": format!("http://{}/api/pricing", env.mock)},
            {"name": "okapi", "url": format!("http://{}/api/okapi-pricing", env.mock)},
            {"name": "html", "url": format!("http://{}/not-json", env.mock)},
            {"name": "dead", "url": "http://127.0.0.1:9/api/pricing"}
        ]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let sources = body["sources"].as_array().unwrap();
    let by_name = |n: &str| sources.iter().find(|s| s["name"] == n).unwrap().clone();
    assert_eq!(by_name("cfg")["status"], "ok");
    assert_eq!(by_name("cfg")["models"], 3);
    assert_eq!(by_name("napi")["status"], "ok");
    assert_eq!(by_name("okapi")["status"], "ok", "Okapi 自家形状要认得");
    assert_eq!(by_name("okapi")["models"], 2);
    assert_eq!(by_name("html")["status"], "error");
    assert_eq!(by_name("html")["error"], "not_json");
    assert_eq!(by_name("dead")["status"], "error");

    let diff = &body["differences"];
    // ratio_model：cfg 与本地全同（不出现）；napi 的 model_ratio 1.5 ≠ 1.25，completion 同
    let mr = &diff[&env.ratio_model]["model_ratio"];
    assert_eq!(mr["current"], "1.25", "{diff}");
    // 有一个源不同时整轴进表；与本地相同的源标 "same"，让人看见哪些源是一致的
    assert_eq!(mr["upstreams"]["cfg"], "same");
    assert_eq!(mr["upstreams"]["napi"], "1.5");
    // Okapi 源写的是 "1.250000"：规范化后等于本地 1.25，判 same 而不是"变了"（不经浮点）
    assert_eq!(mr["upstreams"]["okapi"], "same", "{mr}");
    assert!(
        diff[&env.ratio_model].get("completion_ratio").is_none(),
        "三源 completion 都与本地相同，整轴不进表"
    );
    // 缓存倍率只有 Okapi 源不同 → 进表；给了值的源标 same，没给这一轴的 napi 整个键缺席（第三态）
    let cr = &diff[&env.ratio_model]["cache_ratio"];
    assert_eq!(cr["current"], "0.5", "{cr}");
    assert_eq!(cr["upstreams"]["okapi"], "0.75");
    assert_eq!(cr["upstreams"]["cfg"], "same");
    assert!(
        cr["upstreams"].get("napi").is_none(),
        "源里没这一轴就不出现，不能糊成 same：{cr}"
    );
    // 按次模型：本地 0.04，cfg 给 0.05；不和倍率轴混比
    let pc = &diff[&env.per_call_model]["per_call_price"];
    assert_eq!(pc["current"], "0.04");
    assert_eq!(pc["upstreams"]["cfg"], "0.05");
    // Okapi 源给的是 micro 整数 60000，取回来是 USD 字面量
    assert_eq!(pc["upstreams"]["okapi"], "0.06", "{pc}");
    assert!(diff[&env.per_call_model].get("model_ratio").is_none());
    // 本地没有的模型：current null，两源各给值
    let nm = &diff[&env.new_model]["model_ratio"];
    assert!(nm["current"].is_null());
    assert_eq!(nm["upstreams"]["cfg"], "2");
    assert_eq!(nm["upstreams"]["napi"], "3");
    assert_eq!(
        diff[&env.new_model]["completion_ratio"]["upstreams"]["napi"],
        "8"
    );

    // 源数量 / 名字校验
    let (status, body) = post(&env, "/admin/pricing/sync/fetch", json!({"sources": []})).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["param"], "sources");
    let (status, body) = post(
        &env,
        "/admin/pricing/sync/fetch",
        json!({"sources": [{"name": "a", "url": "http://x/1"}, {"name": "a", "url": "http://x/2"}]}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["param"], "sources.name");
}

#[tokio::test]
async fn apply_changes_only_selected_axes_and_audits() {
    let env = setup().await;
    // 只改 ratio_model 的 model_ratio → 1.5；completion / cache 必须保持 4 / 0.5（不是回到 1）
    // 新模型 new_model：model_ratio 3 + completion 8 两轴一起进
    // 按次模型：0.05
    let (status, body) = post(
        &env,
        "/admin/pricing/sync/apply",
        json!({"changes": [
            {"model": env.ratio_model, "axis": "model_ratio", "value": "1.5"},
            {"model": env.new_model, "axis": "model_ratio", "value": "3"},
            {"model": env.new_model, "axis": "completion_ratio", "value": "8"},
            {"model": env.per_call_model, "axis": "per_call_price", "value": "0.05"}
        ]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["applied"], 4);
    assert_eq!(body["published"], false);

    let row = sqlx::query!(
        r#"SELECT p.pricing_mode, p.model_ratio::text AS model_ratio, p.completion_ratio::text AS "completion_ratio!",
                  p.cache_ratio::text AS "cache_ratio!", p.per_call_price_micro
           FROM model_pricing p JOIN models m ON m.id = p.model_id WHERE m.model_name = $1"#,
        env.ratio_model
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        row.model_ratio
            .as_deref()
            .map(|s| s.trim_end_matches('0').trim_end_matches('.')),
        Some("1.5")
    );
    assert_eq!(
        row.completion_ratio
            .trim_end_matches('0')
            .trim_end_matches('.'),
        "4"
    );
    assert_eq!(
        row.cache_ratio.trim_end_matches('0').trim_end_matches('.'),
        "0.5"
    );

    let new_row = sqlx::query!(
        r#"SELECT p.model_ratio::text AS model_ratio, p.completion_ratio::text AS "completion_ratio!"
           FROM model_pricing p JOIN models m ON m.id = p.model_id WHERE m.model_name = $1"#,
        env.new_model
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(
        new_row
            .model_ratio
            .as_deref()
            .map(|s| s.trim_end_matches('0').trim_end_matches('.')),
        Some("3")
    );
    assert_eq!(
        new_row
            .completion_ratio
            .trim_end_matches('0')
            .trim_end_matches('.'),
        "8"
    );

    let per_call = sqlx::query!(
        r#"SELECT p.pricing_mode, p.per_call_price_micro
           FROM model_pricing p JOIN models m ON m.id = p.model_id WHERE m.model_name = $1"#,
        env.per_call_model
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(per_call.pricing_mode, "per_call");
    assert_eq!(per_call.per_call_price_micro, Some(50_000));

    let audits = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint AS "c!" FROM audit_logs
           WHERE action = 'pricing.sync_apply' AND detail -> 'changes' @> $1"#,
        json!([{"model": env.ratio_model, "axis": "model_ratio", "value": "1.5"}])
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(audits, 1);

    // 非法轴 / 非法值 400
    let (status, body) = post(
        &env,
        "/admin/pricing/sync/apply",
        json!({"changes": [{"model": env.ratio_model, "axis": "bogus", "value": "1"}]}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["param"], "axis");
    let (status, body) = post(
        &env,
        "/admin/pricing/sync/apply",
        json!({"changes": [{"model": env.ratio_model, "axis": "model_ratio", "value": "-1"}]}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["param"], "value");
}
