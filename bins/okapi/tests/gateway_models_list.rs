//! `GET /v1/models`：OpenAI 列表形状；启用的模型在列，停用的不在。
//!
//! 逐接口端到端探针（把接口的 2xx 响应体换成错误内容，看有没有用例察觉）在已提交的树上此项
//! SURVIVED——所有客户端（Claude Code、Codex、Cursor、各家 SDK）发现可用模型时第一个调的就是它，
//! 却没有任何用例核对过返回内容。并行会话在 `gateway_compat.rs` 里有一条同目的的用例尚未提交；
//! 此处先补上，二者合入后保留其一即可。
//!
//! 该路由不做数据面鉴权（供探测可用模型），有无 Bearer 都回 200。依赖 .env（scripts/dev-deps.sh up）。

use okapi::gateway;
use serde_json::Value;
use uuid::Uuid;

#[tokio::test]
async fn lists_enabled_models_and_hides_disabled_ones() {
    dotenvy::dotenv().ok();
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

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, gateway::router(state)).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
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
}
