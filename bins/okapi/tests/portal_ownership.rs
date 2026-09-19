//! 门户侧带 id 的端点是否校验归属（IDOR 面）。
//!
//! 管理面的越权由 `route_error_envelope::every_admin_route_rejects_an_authenticated_but_unprivileged_user`
//! 机械扫掉了——那条线是"有没有权限进这个面"。门户是另一回事：**A 和 B 都有权用
//! `/api/me/keys/{id}`，问题是 A 能不能拿 B 的 id 去用**。权限闸对此无感，得逐个端点
//! 在 SQL 的 `WHERE user_id = $me` 上兜住。
//!
//! 既有套件里没有"拿别人的 id"这类用例：`console_portal` / `console_teams` 各自用
//! 自己的资源跑通了正向流程，反向没人打。
//!
//! 覆盖 `console/mod.rs` 里全部五条带 id 的门户路由：
//! `DELETE|PATCH /api/me/keys/{id}`、`DELETE /api/me/sessions/{sid}`、
//! `POST /api/teams/{id}/members|keys`、`GET /api/teams/{id}/usage`。
//!
//! 判据：一律不得 2xx（越权），也不得 5xx（没兜住、打进业务逻辑才炸）。
//! 403 与 404 都算合规——"无权"和"当你不存在"都是正确答复，不强求哪一种。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

async fn serve(router: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

struct Actor {
    user_id: i64,
    key_id: i64,
    token: String,
}

async fn make_actor(pg: &PgPool, tag: &str) -> Actor {
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();
    let user_id = okapi_store::provision::create_user(pg, &format!("own-{tag}-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-own-{tag}-{suffix}");
    let key_id = okapi_store::provision::create_api_key(pg, user_id, &hash(&token), "sk-own")
        .await
        .unwrap();
    Actor {
        user_id,
        key_id,
        token,
    }
}

/// A 拿 B 的资源 id 去打门户端点，一条都不许过。
#[tokio::test]
async fn portal_id_routes_reject_another_users_resources() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();

    let attacker = make_actor(&pg, "a").await;
    let victim = make_actor(&pg, "b").await;
    // 受害者自己开一个团，团 id 即其 user_id（team 复用 users 主体）
    sqlx::query!(
        "UPDATE users SET kind = 'team' WHERE id = $1",
        victim.user_id
    )
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "own-node", None, None)
        .await
        .unwrap();
    let cs = serve(console::router(state.clone())).await;
    // 给受害者开一条 web 会话，好让攻击者有个真实 sid 可以试着吊销
    let victim_sid = format!("victimsid{}", Uuid::new_v4().simple());
    state
        .sched
        .web_session_set(&victim_sid, victim.user_id, None, None)
        .await;

    let client = reqwest::Client::new();
    let vk = victim.key_id;
    let vt = victim.user_id;
    let probes: Vec<(&str, String, Option<Value>)> = vec![
        ("DELETE", format!("/api/me/keys/{vk}"), None),
        (
            "PATCH",
            format!("/api/me/keys/{vk}"),
            Some(json!({"name": "stolen"})),
        ),
        ("DELETE", format!("/api/me/sessions/{victim_sid}"), None),
        (
            "POST",
            format!("/api/teams/{vt}/members"),
            Some(json!({"member_user_id": 1, "role": "admin"})),
        ),
        (
            "POST",
            format!("/api/teams/{vt}/keys"),
            Some(json!({"name": "stolen"})),
        ),
        ("GET", format!("/api/teams/{vt}/usage"), None),
    ];

    let mut problems: Vec<String> = Vec::new();
    for (method, path, body) in &probes {
        let url = format!("http://{cs}{path}");
        let mut req = match *method {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PATCH" => client.patch(&url),
            "DELETE" => client.delete(&url),
            _ => continue,
        };
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.bearer_auth(&attacker.token).send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if (200..300).contains(&status) {
            problems.push(format!(
                "{method} {path} → {status}：A 操作了 B 的资源\n    {}",
                text.chars().take(160).collect::<String>()
            ));
        } else if status >= 500 {
            problems.push(format!(
                "{method} {path} → {status}：归属校验没兜住，打进业务逻辑才炸\n    {}",
                text.chars().take(160).collect::<String>()
            ));
        }
    }

    // 反向对照：受害者操作自己的资源必须仍然通，否则上面的"全拒"可能只是端点整个坏了
    let own = client
        .delete(format!("http://{cs}/api/me/keys/{vk}"))
        .bearer_auth(&victim.token)
        .send()
        .await
        .unwrap();
    assert!(
        own.status().is_success(),
        "受害者删自己的 key 应当成功（否则上面的全拒是假阳）：{}",
        own.status()
    );

    assert!(
        problems.is_empty(),
        "{} 条越权未被拦住：\n{}",
        problems.len(),
        problems.join("\n")
    );
}
