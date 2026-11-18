//! `GET /v1/models`：OpenAI 列表形状；启用的模型在列，停用的不在。
//!
//! 逐接口端到端探针（把接口的 2xx 响应体换成错误内容，看有没有用例察觉）在已提交的树上此项
//! SURVIVED——所有客户端（Claude Code、Codex、Cursor、各家 SDK）发现可用模型时第一个调的就是它，
//! 却没有任何用例核对过返回内容。并行会话在 `gateway_compat.rs` 里有一条同目的的用例尚未提交；
//! 此处先补上，二者合入后保留其一即可。
//!
//! 该路由要求数据面鉴权，并按 key 的模型白名单过滤。依赖 .env（scripts/dev-deps.sh up）。

use okapi_gateway as gateway;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[tokio::test]
async fn lists_enabled_models_and_hides_disabled_ones() {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let enabled = format!("gml-on-{suffix}");
    let disabled = format!("gml-off-{suffix}");
    for m in [&enabled, &disabled] {
        okapi_store::provision::create_model_ratio(&pg, m, "1", "1", "1")
            .await
            .unwrap();
    }
    sqlx::query("UPDATE models SET status = 2 WHERE model_name = $1")
        .bind(&disabled)
        .execute(&pg)
        .await
        .unwrap();
    // 管理员填过的规格会出现在列表里；没填的字段不出现（见下面的 `plain` 对照）
    sqlx::query(
        "UPDATE models SET display_name = 'Enabled Model', vendor = 'acme', context_window = 200000, max_output = 64000, \
         capabilities = '{\"vision\":true,\"tools\":true,\"audio\":false}', \
         catalog_config = '{\"kind\":\"chat\",\"description\":\"Fast one\",\"input_modalities\":[\"text\",\"image\"],\"output_modalities\":[\"text\"]}' \
         WHERE model_name = $1",
    )
    .bind(&enabled)
    .execute(&pg)
    .await
    .unwrap();

    let uid = okapi_store::provision::create_user(&pg, &format!("models-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-models-{suffix}");
    okapi_store::provision::create_api_key(
        &pg,
        uid,
        &hex::encode(Sha256::digest(token.as_bytes())),
        "sk-okapi-models",
    )
    .await
    .unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, gateway::router(state)).await.unwrap();
    });

    let unauth = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().expect("data 应为数组");
    let mine = data
        .iter()
        .find(|m| m["id"] == enabled.as_str())
        .unwrap_or_else(|| panic!("启用的模型必须在列：{enabled}"));
    assert_eq!(mine["object"], "model");
    assert_eq!(mine["owned_by"], "okapi");
    assert!(
        !data.iter().any(|m| m["id"] == disabled.as_str()),
        "停用的模型不得出现在 /v1/models"
    );
    assert_card_serves_both_readers(&body, mine);
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{addr}/v1beta/models?key={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == format!("models/{enabled}"))
    );
    let response = client
        .get(format!("http://{addr}/v1beta/models?key={token}"))
        .bearer_auth("invalid-header-must-win")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

/// 同一个响应，Anthropic 读者（SDK、Claude Code 的网关发现）要的字段也在；
/// 管理员填过的规格出现在条目里，厂商 / 模态 / 能力放进 `okapi` 扩展对象。
fn assert_card_serves_both_readers(body: &Value, mine: &Value) {
    let data = body["data"].as_array().expect("data 应为数组");
    assert_eq!(mine["type"], "model");
    assert_eq!(mine["display_name"], "Enabled Model");
    assert_eq!(mine["description"], "Fast one");
    assert!(
        mine["created_at"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')),
        "created_at 是 RFC 3339 UTC：{mine}"
    );
    assert!(mine["created"].as_i64().is_some_and(|t| t > 0), "{mine}");
    assert_eq!(mine["context_window"], 200_000);
    assert_eq!(mine["max_input_tokens"], 200_000);
    assert_eq!(mine["max_output_tokens"], 64_000);
    assert_eq!(mine["max_tokens"], 64_000);
    assert_eq!(
        mine["okapi"],
        serde_json::json!({
            "vendor": "acme",
            "kind": "chat",
            "input_modalities": ["text", "image"],
            "output_modalities": ["text"],
            "capabilities": ["tools", "vision"],
        })
    );
    assert_eq!(body["has_more"], false);
    assert_eq!(body["first_id"], data[0]["id"], "{body}");
    assert_eq!(body["last_id"], data[data.len() - 1]["id"], "{body}");
}
