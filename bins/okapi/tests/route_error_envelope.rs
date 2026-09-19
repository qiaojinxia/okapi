//! 全路由错误壳机械核对（IMPLEMENTATION §8）：**每一个** HTTP 端点在无鉴权时，
//! 要么按设计公开放行，要么回一个规范的错误壳——绝不能是裸 500、HTML、
//! 空体或泄漏 Rust 错误文本的字符串。
//!
//! 与既有覆盖的分工：
//! - `guard-error-codes.py` 是**静态**的：扫源码里的 `codes::*` 常量，核对前端有没有文案；
//!   它不知道端点运行时到底回什么。
//! - 各 console / gateway 套件逐个端点验业务语义，但每个套件只管自己那几条路由，
//!   新加一条路由时没有任何机制逼它进任何一张表。
//!
//! 本用例**从源码现抽路由表**（解析 `.route("…", get(…).post(…))`），所以新增端点
//! 自动进入覆盖，漏配鉴权或错误壳当场红——这是它存在的理由，不要改成硬编码清单。
//!
//! 判定口径（只钉不该退让的那条线，不猜业务语义）：
//! - 2xx / 3xx：该端点按设计公开（healthz、公开价格页、登录注册、支付回调…），放行；
//! - 4xx / 5xx：body 必须是 `{"error":{"code","message","type"}}` 且 `code` 非空；
//! - 5xx 额外收紧：无鉴权的探测不该打到 500——那是把内部错误当门面回给匿名调用方。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;

/// 路径参数占位：用不存在但形状合法的值，走到鉴权/校验而不是路由不匹配。
fn fill_params(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let name = &rest[open + 1..open + close];
        // 形状要像真的：id 类给数字，其余给一段安全 slug
        // 顺序要紧：`request_id` 既含 "request" 也含 "id"，先判 uuid 一侧
        out.push_str(
            if name.contains("uuid") || name.contains("request") || name.contains("batch") {
                "00000000-0000-0000-0000-000000000000"
            } else if name.contains("id") {
                "1"
            } else {
                "probe"
            },
        );
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// 从源码抽 `.route("path", method(..))`，返回 路径 → (方法集, 归属角色)。
fn routes_from_source() -> BTreeMap<String, (Vec<String>, &'static str)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out: BTreeMap<String, (Vec<String>, &'static str)> = BTreeMap::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let role = if p.to_string_lossy().contains("/console/") {
                "console"
            } else {
                "gateway"
            };
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            collect_routes(&text, role, &mut out);
        }
    }
    out
}

fn collect_routes(
    text: &str,
    role: &'static str,
    out: &mut BTreeMap<String, (Vec<String>, &'static str)>,
) {
    let mut rest = text;
    while let Some(i) = rest.find(".route(") {
        rest = &rest[i + 7..];
        let Some(q1) = rest.find('"') else { break };
        let Some(q2) = rest[q1 + 1..].find('"') else {
            break;
        };
        let path = &rest[q1 + 1..q1 + 1 + q2];
        if !path.starts_with('/') {
            continue;
        }
        // 方法名在路径之后、本次 .route(...) 结束之前
        let tail = &rest[q1 + 1 + q2..];
        // 截到**下一条 `.route(` 之前**：早先按固定字节开窗，会把相邻路由的方法
        // 算到本条头上，探出一堆假的 405。另按字符边界收口——源码里有中文注释。
        let mut cap = tail.find(".route(").unwrap_or(tail.len()).min(400);
        while cap > 0 && !tail.is_char_boundary(cap) {
            cap -= 1;
        }
        let window = &tail[..cap];
        let mut methods = Vec::new();
        for m in ["get", "post", "put", "patch", "delete"] {
            if window.contains(&format!("{m}(")) {
                methods.push(m.to_uppercase());
            }
        }
        if methods.is_empty() {
            continue;
        }
        let entry = out
            .entry(path.to_owned())
            .or_insert_with(|| (Vec::new(), role));
        for m in methods {
            if !entry.0.contains(&m) {
                entry.0.push(m);
            }
        }
    }
}

async fn serve(router: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

/// 无鉴权探测每一条路由：公开端点放行，其余必须回规范错误壳。
#[tokio::test]
async fn every_route_returns_a_well_formed_error_envelope_without_auth() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "envelope-node", None, None)
        .await
        .unwrap();
    let gw = serve(gateway::router(state.clone())).await;
    let cs = serve(console::router(state)).await;

    let routes = routes_from_source();
    assert!(
        routes.len() > 100,
        "路由抽取失败（只抽到 {}），说明解析跟 router 写法脱节了——先修解析再谈覆盖",
        routes.len()
    );

    let client = reqwest::Client::new();
    let mut problems: Vec<String> = Vec::new();
    let mut probed = 0usize;

    for (path, (methods, role)) in &routes {
        // WebSocket 升级路由用 HTTP 探测没有意义（回 400 是协议层拒绝，不是业务错误壳）
        if path.contains("realtime") {
            continue;
        }
        let addr = if *role == "console" { cs } else { gw };
        let url = format!("http://{addr}{}", fill_params(path));
        for method in methods {
            let req = match method.as_str() {
                "GET" => client.get(&url),
                "POST" => client.post(&url).json(&serde_json::json!({})),
                "PUT" => client.put(&url).json(&serde_json::json!({})),
                "PATCH" => client.patch(&url).json(&serde_json::json!({})),
                "DELETE" => client.delete(&url),
                _ => continue,
            };
            let Ok(resp) = req.send().await else {
                problems.push(format!("{method} {path}：请求发不出去"));
                continue;
            };
            probed += 1;
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            if (200..400).contains(&status) {
                continue; // 按设计公开
            }
            if status == 405 {
                continue; // 该路由没有这个方法：axum 框架级 405，非业务错误壳
            }
            let Ok(v) = serde_json::from_str::<Value>(&body) else {
                problems.push(format!(
                    "{method} {path} → {status} 体不是 JSON：{}",
                    body.chars().take(120).collect::<String>()
                ));
                continue;
            };
            // 三种方言各有自己的错误壳，都是对外承诺的一部分：
            //   Okapi / OpenAI : {"error":{"code":"<字符串码>",…}}
            //   Anthropic      : {"type":"error","error":{"type":"<码>",…}}（无 code 键）
            //   Gemini         : {"error":{"code":<数字>,"status":"NOT_FOUND",…}}
            // 底线是"有一个机器可读的分类标识"，按方言取到任意一个即算合规。
            let err = &v["error"];
            let tag = err["code"]
                .as_str()
                .filter(|c| !c.is_empty())
                .or_else(|| err["type"].as_str().filter(|t| !t.is_empty()))
                .or_else(|| err["status"].as_str().filter(|s| !s.is_empty()))
                .map(str::to_owned)
                .or_else(|| err["code"].as_i64().map(|n| n.to_string()));
            let Some(code) = tag else {
                problems.push(format!(
                    "{method} {path} → {status} 错误壳里没有机器可读的分类标识：{v}"
                ));
                continue;
            };
            if status >= 500 {
                problems.push(format!(
                    "{method} {path} → {status}（code={code}）：匿名探测打到 5xx，\
                     说明内部错误被当门面回给了未鉴权调用方"
                ));
            }
        }
    }

    assert!(probed > 100, "实际只探了 {probed} 条，覆盖不足");
    assert!(
        problems.is_empty(),
        "共探 {probed} 条路由，{} 条不合规：\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// 建一个普通用户（role=1，无 admin 角色）并返回其 API key。
async fn plain_user_token(pg: &sqlx::PgPool) -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..10].to_owned();
    let user_id = okapi_store::provision::create_user(pg, &format!("probe-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-probe-{suffix}");
    let hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    okapi_store::provision::create_api_key(pg, user_id, &hash, "sk-probe")
        .await
        .unwrap();
    token
}

/// 每一条管理面路由都必须真的挂着权限闸。
///
/// 与 [`every_route_returns_a_well_formed_error_envelope_without_auth`] 的分工：
/// 那个探**匿名**，一律止步于 `authenticate`，所以它**区分不出** handler 里到底有没有
/// `guard()`——漏挂权限闸的端点在匿名探测下同样是 401，看着很安全。
///
/// 这里换成**已登录但无权限**的普通用户（role=1、无 admin 角色）：能过 `authenticate`、
/// 必须倒在 `guard()`。既有的 `console_m2::permission_point_matrix` 验的是机制本身
/// （角色→权限点→放行/拒绝），只在 `/admin/channels` 两个端点上验；哪条新路由忘了挂闸
/// 它照样绿。本用例从源码现抽路由表，新增端点自动进覆盖。
///
/// 判据：`/admin/*` 一律不得回 2xx，也不得回 5xx（前者是越权，后者是闸没拦住、
/// 打进了业务逻辑才炸）。
#[tokio::test]
async fn every_admin_route_rejects_an_authenticated_but_unprivileged_user() {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let token = plain_user_token(&pg).await;
    let state = gateway::build_state(&database_url, &redis_url, "rbac-probe-node", None, None)
        .await
        .unwrap();
    let cs = serve(console::router(state)).await;

    let client = reqwest::Client::new();
    let mut problems: Vec<String> = Vec::new();
    let mut probed = 0usize;

    for (path, (methods, role)) in &routes_from_source() {
        if *role != "console" || !path.starts_with("/admin/") {
            continue;
        }
        let url = format!("http://{cs}{}", fill_params(path));
        for method in methods {
            let req = match method.as_str() {
                "GET" => client.get(&url),
                "POST" => client.post(&url).json(&serde_json::json!({})),
                "PUT" => client.put(&url).json(&serde_json::json!({})),
                "PATCH" => client.patch(&url).json(&serde_json::json!({})),
                "DELETE" => client.delete(&url),
                _ => continue,
            };
            let Ok(resp) = req.bearer_auth(&token).send().await else {
                problems.push(format!("{method} {path}：请求发不出去"));
                continue;
            };
            probed += 1;
            let status = resp.status().as_u16();
            if status == 405 {
                continue; // 该路由没有这个方法
            }
            let body = resp.text().await.unwrap_or_default();
            if (200..300).contains(&status) {
                problems.push(format!(
                    "{method} {path} → {status}：普通用户拿到了管理面响应，权限闸没挂上\n    {}",
                    body.chars().take(160).collect::<String>()
                ));
            } else if status >= 500 {
                problems.push(format!(
                    "{method} {path} → {status}：权限闸没拦住，打进业务逻辑才炸\n    {}",
                    body.chars().take(160).collect::<String>()
                ));
            }
        }
    }

    assert!(probed > 50, "只探了 {probed} 条管理面路由，覆盖不足");
    assert!(
        problems.is_empty(),
        "共探 {probed} 条管理面路由，{} 条未被权限闸拦住：\n{}",
        problems.len(),
        problems.join("\n")
    );
}
