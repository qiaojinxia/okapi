//! 价目目录建议（§11.48）：权限、查询校验与响应形状。
//! 匹配与换算规则由 `okapi-pricing` 的单测覆盖；这里只验证接口面：未登录 401、
//! 没有 pricing.read 403、参数校验 400、目录里有 / 没有的名字、联想封顶。

use okapi::{console, gateway};
use serde_json::Value;
use std::net::SocketAddr;
use uuid::Uuid;

struct Env {
    addr: SocketAddr,
    admin: String,
    plain: String,
    client: reqwest::Client,
}

async fn token_for(pg: &sqlx::PgPool, admin: bool) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    let user = okapi_store::provision::create_user(pg, &format!("pc-{suffix}"))
        .await
        .unwrap();
    if admin {
        sqlx::query("UPDATE users SET role = 100 WHERE id = $1")
            .bind(user)
            .execute(pg)
            .await
            .unwrap();
    }
    let token = format!("sk-okapi-pc-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(pg, user, &hash, "sk-okapi-pc")
        .await
        .unwrap();
    token
}

async fn setup() -> Env {
    okapi_store::test_support::assert_isolated();
    let database = std::env::var("DATABASE_URL").unwrap();
    let redis = std::env::var("OKAPI_REDIS_URL").unwrap();
    let pg = okapi_store::connect_pg(&database).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let admin = token_for(&pg, true).await;
    let plain = token_for(&pg, false).await;
    let state = gateway::build_state(&database, &redis, "catalog-suggest", None, None)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, console::router(state)).await.unwrap() });
    Env {
        addr,
        admin,
        plain,
        client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap(),
    }
}

impl Env {
    async fn get(&self, path: &str, query: &[(&str, &str)], token: Option<&str>) -> (u16, Value) {
        let mut url = reqwest::Url::parse(&format!("http://{}{path}", self.addr)).unwrap();
        url.query_pairs_mut().extend_pairs(query.iter().copied());
        let mut request = self.client.get(url);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

const LOOKUP: &str = "/admin/pricing/catalog/lookup";
const SEARCH: &str = "/admin/pricing/catalog/search";

#[tokio::test]
async fn the_catalog_needs_a_login_and_the_pricing_read_permission() {
    let env = setup().await;
    for (path, query) in [(LOOKUP, [("model", "gpt-4o")]), (SEARCH, [("q", "gpt")])] {
        assert_eq!(env.get(path, &query, None).await.0, 401, "{path}");
        let (status, body) = env.get(path, &query, Some(&env.plain)).await;
        assert_eq!(status, 403, "{path}");
        assert_eq!(body["error"]["param"], "pricing.read", "{path}");
        assert_eq!(
            env.get(path, &query, Some(&env.admin)).await.0,
            200,
            "{path}"
        );
    }
}

#[tokio::test]
async fn lookup_returns_the_suggestion_with_its_provenance_or_null() {
    let env = setup().await;
    let (status, body) = env
        .get(LOOKUP, &[("model", " GPT-4o ")], Some(&env.admin))
        .await;
    assert_eq!(status, 200);
    let suggestion = &body["suggestion"];
    assert_eq!(suggestion["key"], "gpt-4o");
    assert_eq!(suggestion["provider"], "openai");
    assert_eq!(suggestion["matched"], "exact");
    assert_eq!(suggestion["kind"], "chat");
    assert_eq!(suggestion["billing"], "ratio");
    assert_eq!(suggestion["input_per_1m"], "2.5");
    assert_eq!(suggestion["output_per_1m"], "10");
    assert_eq!(suggestion["cache_read_ratio"], "0.5");
    assert!(suggestion["notes"].is_array());
    assert_eq!(body["snapshot"]["dataset"], "litellm");
    assert_eq!(body["snapshot"]["commit"].as_str().map(str::len), Some(40));

    // 目录里没有的名字：不猜最接近的，直接 null
    for unknown in ["gpt-4o-my-own-finetune", "no-such-model-anywhere"] {
        let (status, body) = env
            .get(LOOKUP, &[("model", unknown)], Some(&env.admin))
            .await;
        assert_eq!(status, 200, "{unknown}");
        assert!(body["suggestion"].is_null(), "{unknown}");
    }
}

#[tokio::test]
async fn lookup_rejects_a_missing_blank_or_oversized_name() {
    let env = setup().await;
    let too_long = "m".repeat(129);
    for query in [
        vec![],
        vec![("model", "")],
        vec![("model", "   ")],
        vec![("model", too_long.as_str())],
    ] {
        let (status, body) = env.get(LOOKUP, &query, Some(&env.admin)).await;
        assert_eq!(status, 400, "{query:?}");
        assert!(body["error"].is_object(), "{query:?}: {body}");
    }
}

#[tokio::test]
async fn search_returns_one_row_per_id_capped_by_the_limit() {
    let env = setup().await;
    let (status, body) = env
        .get(SEARCH, &[("q", "claude-opus-5")], Some(&env.admin))
        .await;
    assert_eq!(status, 200);
    let data = body["data"].as_array().unwrap();
    assert!(!data.is_empty(), "claude-opus-5 在目录里有联想");
    let ids: Vec<&str> = data.iter().filter_map(|hit| hit["id"].as_str()).collect();
    assert!(ids.contains(&"claude-opus-5-5"), "{ids:?}");
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "同一个 id 只出现一次：{ids:?}");
    assert!(
        data.iter()
            .all(|hit| hit["provider"].is_string() && hit["mode"].is_string())
    );

    let (_, capped) = env
        .get(SEARCH, &[("q", "gpt"), ("limit", "3")], Some(&env.admin))
        .await;
    assert_eq!(capped["data"].as_array().unwrap().len(), 3);
    // 0 与过大的 limit 被夹到 1..=50
    let (_, floor) = env
        .get(SEARCH, &[("q", "gpt"), ("limit", "0")], Some(&env.admin))
        .await;
    assert_eq!(floor["data"].as_array().unwrap().len(), 1);
    let (_, ceiling) = env
        .get(SEARCH, &[("q", "a"), ("limit", "5000")], Some(&env.admin))
        .await;
    assert_eq!(ceiling["data"].as_array().unwrap().len(), 50);

    // 空关键词：什么都不联想（不是整本目录）
    let (status, empty) = env.get(SEARCH, &[("q", "  ")], Some(&env.admin)).await;
    assert_eq!(status, 200);
    assert!(
        empty["data"].as_array().unwrap().is_empty(),
        "空关键词什么都不联想（不是整本目录）"
    );
    let (status, _) = env
        .get(SEARCH, &[("q", &"q".repeat(129))], Some(&env.admin))
        .await;
    assert_eq!(status, 400);
}
