//! SPA 导航内容协商：SPA 路由与管理 API 同挂 /admin/* 时，浏览器导航必须拿到应用，
//! 而 API 客户端行为不得改变（此前 `/admin/channels` 刷新会返回 401 JSON）。
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use std::net::SocketAddr;
use std::path::Path;

async fn setup() -> SocketAddr {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    let app = console::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

/// 本地可不构建前端；CI 显式提供构建产物目录，缺失时必须失败。
fn dist_ready() -> bool {
    if let Ok(dir) = std::env::var("OKAPI_WEB_DIR") {
        assert!(
            Path::new(&dir).join("index.html").is_file(),
            "OKAPI_WEB_DIR 已配置但缺少 index.html"
        );
        return true;
    }
    Path::new("frontend/dist/index.html").is_file()
}

/// API 客户端不受影响：与 SPA 同名的路径仍按 API 语义响应（鉴权/方法校验照常）。
#[tokio::test]
async fn api_clients_keep_json_semantics() {
    let addr = setup().await;
    let client = reqwest::Client::new();

    // 无凭证 GET /admin/channels：API 语义 = 401，绝不能被导航兜底改成 200 HTML
    let resp = client
        .get(format!("http://{addr}/admin/channels"))
        .header("accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "API 客户端必须仍拿到 401");

    // fetch 的缺省 Accept 是 */*，同样走 API
    let resp = client
        .get(format!("http://{addr}/admin/channels"))
        .header("accept", "*/*")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "Accept: */* 不构成浏览器导航");

    // 方法不匹配的路径同理（/admin/cache/flush 仅 POST）：405 而非被 SPA fallback 吞成 200
    let resp = client
        .get(format!("http://{addr}/admin/cache/flush"))
        .header("accept", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);
}

/// 浏览器导航拿到应用：包括与 API 同名的路径。
#[tokio::test]
async fn browser_navigation_gets_the_app() {
    if !dist_ready() {
        eprintln!("跳过：frontend/dist/index.html 不存在（需先 pnpm build）");
        return;
    }
    let addr = setup().await;
    let client = reqwest::Client::new();
    let html_accept = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

    for path in [
        "/admin/channels",
        "/admin/pricing",
        "/portal/topup",
        "/admin/codes",
    ] {
        let resp = client
            .get(format!("http://{addr}{path}"))
            .header("accept", html_accept)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path} 浏览器导航应返回应用");
        let body = resp.text().await.unwrap();
        assert!(body.contains("id=\"root\""), "{path} 应返回 SPA 宿主页");
    }
}

/// 豁免路径仍由服务端处理：OAuth 起跳是浏览器导航，但必须走后端重定向而非 SPA。
#[tokio::test]
async fn server_handled_paths_are_exempt() {
    let addr = setup().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let html_accept = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

    // 未配置 oauth_providers 时后端回错误码；关键是不能被 SPA 兜底吞掉
    let resp = client
        .get(format!("http://{addr}/auth/oauth/github"))
        .header("accept", html_accept)
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status(), 200, "OAuth 起跳不得被 SPA 兜底接管");

    let resp = client
        .get(format!("http://{addr}/healthz"))
        .header("accept", html_accept)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok", "探针不得返回 HTML");
}
