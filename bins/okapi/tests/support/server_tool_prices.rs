//! Draft and publish a model's server-tool prices through the admin API, then swap the gateway onto the new epoch.
//! Test-free so both `gateway_usage_modalities` and `gateway_server_tool_statistics` can include it.
use super::{Env, report};
use serde_json::{Value, json};

pub(super) fn prices(search: &Value) -> Value {
    json!({"usage_contract":"anthropic_server_tool_use_v1","web_search":search,"web_fetch":{"billing":"included"}})
}
pub(super) async fn post(env: &Env, path: &str, body: &Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(&env.token)
        .json(body)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
pub(super) async fn write_draft(env: &Env, profile: &Value) {
    let (status,body)=post(env,"/admin/models", &json!({"model_name":env.model,"model_ratio":"1",
        "completion_ratio":"2","cache_ratio":"0.5","cache_write_ratio":"2",
        "audio_ratio":"8","audio_completion_ratio":"2","image_ratio":"3","server_tool_prices":profile})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["requires_publish"], true);
}
pub(super) async fn activate(env: &Env, profile: &Value) -> i64 {
    sqlx::query("UPDATE users SET role=100 WHERE id=$1")
        .bind(env.user)
        .execute(&env.state.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET upstream_unit_cost=$2 WHERE name=$1")
        .bind(env.model.clone())
        .bind(json!({"relative_cost_milli":1250}))
        .execute(&env.state.pg)
        .await
        .unwrap();
    let old_epoch = env.state.pricebook.epoch();
    write_draft(env, profile).await;
    let catalog = report(env, &format!("/api/pricing?model={}", env.model)).await;
    assert!(
        catalog["models"][0]["server_tool_prices"].is_null(),
        "draft leaked: {catalog}"
    );
    let old = okapi::kernel::pricing_loader::load_pricebook(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(old.epoch(), old_epoch);
    let (status, body) = post(env, "/admin/pricing/publish", &json!({})).await;
    assert_eq!(status, 200, "{body}");
    let epoch = body["epoch"].as_i64().unwrap();
    assert!(epoch > old_epoch);
    let book = okapi::kernel::pricing_loader::load_pricebook(&env.state.pg)
        .await
        .unwrap();
    assert_eq!(book.epoch(), epoch);
    assert!(env.state.pricebook.swap_if_newer(book));
    let catalog = report(env, &format!("/api/pricing?model={}", env.model)).await;
    assert_eq!(
        catalog["models"][0]["server_tool_prices"], *profile,
        "{catalog}"
    );
    epoch
}
