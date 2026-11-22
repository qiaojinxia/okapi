//! 批次 C & D 验收：多 Key 渠道 CRUD/护栏/映射回显/批量切池解冷，
//! 以及定价快照列表/Diff/试算/回滚、模型别名 CRUD、用户专属定价 CRUD 与规则 scope 严格校验。
use okapi_console as console;
use okapi_gateway as gateway;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    state: Arc<okapi_engine::state::AppState>,
    console: SocketAddr,
    super_token: String,
    user_id: i64,
    suffix: String,
}

async fn setup() -> Bed {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let user_id = okapi_store::provision::create_user(&pg, &format!("ocp-u-{suffix}"))
        .await
        .unwrap();
    let super_id = okapi_store::provision::create_user(&pg, &format!("ocp-s-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-ocp-super-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-ocp-s")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "ocp-node", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console = listener.local_addr().unwrap();
    let app = console::router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    Bed {
        pg,
        state: Arc::new(state),
        console,
        super_token,
        user_id,
        suffix,
    }
}

async fn call(bed: &Bed, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = reqwest::Client::new()
        .request(
            method.parse().unwrap(),
            format!("http://{}{path}", bed.console),
        )
        .bearer_auth(&bed.super_token);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let val = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, val)
}

async fn create_test_channel(bed: &Bed, model: &str) -> (i64, i64) {
    let (st, created) = call(
        bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("ocp-ch-{}", bed.suffix),
            "provider": "openai",
            "api_base": "https://1.1.1.1/v1",
            "credential": "sk-key-one",
            "models": [model],
        })),
    )
    .await;
    assert_eq!(st, 200, "{created}");
    (
        created["channel_id"].as_i64().unwrap(),
        created["channel_key_id"].as_i64().unwrap(),
    )
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // End-to-end channel key lifecycle + mapping echo check.
async fn channel_multi_key_crud_caps_and_mapping_echo() {
    let bed = setup().await;
    let model = format!("ocp-m-{}", bed.suffix);
    let (cid, key1) = create_test_channel(&bed, &model).await;

    let (st, err) = call(
        &bed,
        "DELETE",
        &format!("/admin/channels/{cid}/keys/{key1}"),
        None,
    )
    .await;
    assert_eq!(st, 409, "{err}");
    assert_eq!(err["error"]["code"], "channel_last_key");

    let (st, added) = call(
        &bed,
        "POST",
        &format!("/admin/channels/{cid}/keys"),
        Some(json!({
            "credential": "sk-key-two", "weight": 3, "rpm_limit": 60,
            "daily_spend_cap_micro": 10_000_000, "model_subset": [model],
        })),
    )
    .await;
    assert_eq!(st, 200, "{added}");
    let key2 = added["channel_key_id"].as_i64().unwrap();
    assert_eq!(
        call(
            &bed,
            "PATCH",
            &format!("/admin/channels/{cid}/keys/{key2}"),
            Some(json!({ "rpm_limit": 30, "model_subset": null })),
        )
        .await
        .0,
        200
    );

    assert_eq!(
        call(
            &bed,
            "PATCH",
            &format!("/admin/channels/{cid}"),
            Some(json!({ "model_mapping": { "a": 123 } })),
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(
            &bed,
            "PATCH",
            &format!("/admin/channels/{cid}"),
            Some(json!({ "capabilities": { "tools": "yes" } })),
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(
            &bed,
            "PATCH",
            &format!("/admin/channels/{cid}"),
            Some(json!({
                "model_mapping": { model.as_str(): "upstream-v1" },
                "capabilities": { "tools": true, "vision": false },
            })),
        )
        .await
        .0,
        200
    );

    let (st, list) = call(
        &bed,
        "GET",
        &format!("/admin/channels?q=ocp-ch-{}", bed.suffix),
        None,
    )
    .await;
    assert_eq!(st, 200);
    let ch = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == cid)
        .unwrap();
    assert_eq!(ch["model_mapping"][&model], "upstream-v1");
    assert_eq!(ch["capabilities"]["tools"], true);
    let k2 = ch["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["id"] == key2)
        .unwrap();
    assert_eq!(k2["rpm_limit"], 30);
    assert_eq!(k2["daily_spend_cap_micro"], 10_000_000);
    assert!(k2["model_subset"].is_null());
    assert_eq!(
        call(
            &bed,
            "DELETE",
            &format!("/admin/channels/{cid}/keys/{key2}"),
            None,
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn channel_batch_set_pools_and_clear_cooldown() {
    let bed = setup().await;
    let model = format!("ocp-bm-{}", bed.suffix);
    let pool = format!("ocp-p-{}", bed.suffix);
    sqlx::query("INSERT INTO channel_pools (pool_code, description) VALUES ($1, 'ops')")
        .bind(&pool)
        .execute(&bed.pg)
        .await
        .unwrap();
    let (cid, key1) = create_test_channel(&bed, &model).await;

    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/channels/batch",
            Some(json!({ "ids": [cid], "action": "set_pools", "pools": [pool] })),
        )
        .await
        .0,
        200
    );
    sqlx::query(
        "UPDATE channel_keys SET status = 3, cooldown_until = now() + interval '5 minutes' WHERE id = $1",
    )
    .bind(key1)
    .execute(&bed.pg)
    .await
    .unwrap();
    bed.state.sched.model_cooldown_set(key1, &model, 120).await;
    assert!(
        bed.state
            .sched
            .model_cooldown_active(&model, &[key1])
            .await
            .contains_key(&key1)
    );
    let (st, cleared) = call(
        &bed,
        "POST",
        "/admin/channels/batch",
        Some(json!({ "ids": [cid], "action": "clear_cooldown" })),
    )
    .await;
    assert_eq!(st, 200, "{cleared}");
    assert_eq!(cleared["affected"], 1);
    assert!(
        bed.state
            .sched
            .model_cooldown_active(&model, &[key1])
            .await
            .is_empty()
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // End-to-end publish -> diff -> simulate -> rollback check.
async fn pricing_diff_simulate_rollback_and_scope() {
    let bed = setup().await;
    let model = format!("ocp-pm-{}", bed.suffix);
    let rule_code = format!("ocp-r-{}", bed.suffix);
    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/models",
            Some(json!({ "model_name": model, "model_ratio": "1", "completion_ratio": "1" })),
        )
        .await
        .0,
        200
    );
    let (st, pub1) = call(&bed, "POST", "/admin/pricing/publish", None).await;
    assert_eq!(st, 200, "{pub1}");
    let ep1 = pub1["epoch"].as_i64().unwrap();

    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/pricing/rules",
            Some(json!({
                "rule_code": rule_code, "rule_type": "discount", "multiplier": "0.8",
                "scope": { "group": ["default"] },
            })),
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/pricing/rules",
            Some(json!({
                "rule_code": rule_code, "rule_type": "discount", "multiplier": "0.8",
                "scope": { "groups": ["default"] },
            })),
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/models",
            Some(json!({ "model_name": model, "model_ratio": "3", "completion_ratio": "1" })),
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &bed,
            "POST",
            &format!("/admin/users/{}/pricing", bed.user_id),
            Some(json!({
                "model_name": model, "override_kind": "ratio", "custom_model_ratio": "0.5",
                "reason": "vip-deal",
            })),
        )
        .await
        .0,
        200
    );

    let (st, diff) = call(&bed, "GET", "/admin/pricing/diff", None).await;
    assert_eq!(st, 200, "{diff}");
    assert_eq!(diff["has_changes"], true);
    assert!(
        diff["models"]["changed"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    );
    assert!(
        diff["overrides"]["added"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    );

    let (st, sim) = call(
        &bed,
        "POST",
        "/admin/pricing/simulate",
        Some(json!({
            "model": model, "user_id": bed.user_id,
            "prompt_tokens": 1_000_000, "completion_tokens": 0,
        })),
    )
    .await;
    assert_eq!(st, 200, "{sim}");
    assert!(
        sim["draft"]["amount_micro"].as_i64().unwrap()
            < sim["published"]["amount_micro"].as_i64().unwrap()
    );

    assert_eq!(
        call(&bed, "POST", "/admin/pricing/publish", None).await.0,
        200
    );
    let (st, epochs) = call(&bed, "GET", "/admin/pricing/epochs", None).await;
    assert_eq!(st, 200);
    assert!(epochs["total"].as_i64().unwrap() >= 2);

    let (st, rb) = call(
        &bed,
        "POST",
        &format!("/admin/pricing/epochs/{ep1}/rollback"),
        None,
    )
    .await;
    assert_eq!(st, 200, "{rb}");
    assert_eq!(rb["rolled_back_to"], ep1);
    let (st, diff_after) = call(&bed, "GET", "/admin/pricing/diff", None).await;
    assert_eq!(st, 200);
    assert_eq!(diff_after["has_changes"], false, "{diff_after}");
}

#[tokio::test]
async fn model_aliases_and_user_pricing_crud() {
    let bed = setup().await;
    let model = format!("ocp-am-{}", bed.suffix);
    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/models",
            Some(json!({ "model_name": model, "model_ratio": "1" })),
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/model-aliases",
            Some(json!({ "pattern": "bad-alias", "target_model": "no-such-model" })),
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            &bed,
            "POST",
            "/admin/model-aliases",
            Some(json!({ "pattern": model, "target_model": model })),
        )
        .await
        .0,
        400
    );
    let alias = format!("alias-{}", bed.suffix);
    let (st, created) = call(
        &bed,
        "POST",
        "/admin/model-aliases",
        Some(json!({ "pattern": alias, "target_model": model, "priority": 5 })),
    )
    .await;
    assert_eq!(st, 200, "{created}");
    let alias_id = created["id"].as_i64().unwrap();
    let (st, list) = call(&bed, "GET", "/admin/model-aliases", None).await;
    assert_eq!(st, 200);
    assert!(
        list["data"]
            .as_array()
            .is_some_and(|a| a.iter().any(|item| item["id"] == alias_id))
    );
    assert_eq!(
        call(
            &bed,
            "DELETE",
            &format!("/admin/model-aliases/{alias_id}"),
            None
        )
        .await
        .0,
        200
    );

    assert_eq!(
        call(
            &bed,
            "POST",
            &format!("/admin/users/{}/pricing", bed.user_id),
            Some(json!({
                "model_name": model, "override_kind": "absolute",
                "custom_input_per_1m_micro": 1_000_000, "custom_output_per_1m_micro": 2_000_000,
            })),
        )
        .await
        .0,
        200
    );
    let (st, up) = call(
        &bed,
        "GET",
        &format!("/admin/users/{}/pricing", bed.user_id),
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(up["total"], 1);
    assert_eq!(
        call(
            &bed,
            "DELETE",
            &format!("/admin/users/{}/pricing/{model}", bed.user_id),
            None,
        )
        .await
        .0,
        200
    );
}
