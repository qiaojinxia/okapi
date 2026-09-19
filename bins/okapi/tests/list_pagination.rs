//! 列表端点的翻页准确性:**不重、不漏、total 一致**。
//!
//! 分页机制本身有单测（`listing::page_params_are_clamped`），逐端点的集成覆盖此前
//! 只有一条（`console_manage::price_group_pagination_matches_database_pages`）。
//! 其余列表端点只验过"能调用、回了 200"——而这一类最典型的缺陷恰恰是 200 下的错：
//! `limit` 生效但 `offset` 没接进 SQL（第二页和第一页一模一样），
//! 或排序键不唯一导致两页在边界上重叠 / 漏行。
//!
//! 判据用的是不依赖数据库内容的恒等式，不写死行数：
//!
//! ```text
//! 取 A = ?limit=2N&offset=0
//!    B = ?limit=N&offset=0
//!    C = ?limit=N&offset=N
//! 必须  B ++ C == A（逐个 id 等且同序）
//!       B ∩ C == ∅
//!       三次的 total 相同
//! ```
//!
//! 这样既不必把全表拉下来（`/admin/users` 在开发库里上千行），也对并发写入不敏感——
//! 三次取的是同一个窗口的不同切法。
//!
//! 第二个用例走另一条轴：**越界参数必须被夹取或拒绝，不能被静默当真**。
//! 这是"参数组合约束"里唯一机械可判的那部分——不必知道每个端点的业务语义，
//! 只要求 `limit=999999` 不能真回 99 万行（未夹的 limit 就是个 DoS 面）、
//! 负数与 0 不能把 handler 打崩。`listing::MAX_PAGE` 是全局上限，用它当判据。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use okapi::{console, gateway};
use serde_json::Value;
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

/// 每页行数。取 2 是为了让"第二页"一定落在有数据的区间里——
/// 开发库里这几张表都远超 4 行，而新建的种子行也够。
const N: usize = 2;

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

struct Bed {
    console: SocketAddr,
    admin_token: String,
    user_token: String,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL（.env）");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL（.env）");
    let pg: PgPool = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let admin_id = okapi_store::provision::create_user(&pg, &format!("pg-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-pg-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-pgadm")
        .await
        .unwrap();

    // 普通用户自带 5 把 key：够翻两页且第二页非空
    let user_id = okapi_store::provision::create_user(&pg, &format!("pg-u-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-pg-u-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-pgu")
        .await
        .unwrap();
    for i in 0..4 {
        let t = format!("sk-okapi-pg-u{i}-{suffix}");
        okapi_store::provision::create_api_key(&pg, user_id, &hash(&t), "sk-pgu")
            .await
            .unwrap();
    }
    // 渠道池也种几个，保证 /admin/pools 有得翻
    for i in 0..4 {
        let code = format!("pgpool{i}{}", &suffix[..6]);
        sqlx::query!(
            "INSERT INTO channel_pools (pool_code, description) VALUES ($1, 'pagination')
             ON CONFLICT DO NOTHING",
            code
        )
        .execute(&pg)
        .await
        .unwrap();
    }

    let state = gateway::build_state(&database_url, &redis_url, "page-node", None, None)
        .await
        .unwrap();
    let console = serve(console::router(state)).await;
    Bed {
        console,
        admin_token,
        user_token,
    }
}

async fn fetch(
    bed: &Bed,
    path: &str,
    token: &str,
    limit: usize,
    offset: usize,
) -> (Value, Vec<String>) {
    let sep = if path.contains('?') { '&' } else { '?' };
    let url = format!(
        "http://{}{path}{sep}limit={limit}&offset={offset}",
        bed.console
    );
    let body = reqwest::Client::new()
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    // 行标识优先 id，退而求其次用整行的字符串形态（某些列表主键不叫 id）
    let ids = body["data"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|r| {
            r.get("id")
                .filter(|v| !v.is_null())
                .map_or_else(|| r.to_string(), std::string::ToString::to_string)
        })
        .collect();
    (body, ids)
}

/// 每个列表端点都必须满足 B ++ C == A、B ∩ C == ∅、total 恒定。
#[tokio::test]
async fn list_endpoints_paginate_without_overlap_or_gaps() {
    let bed = setup().await;

    // (路径, 用哪把 token)
    let cases: Vec<(&str, &str)> = vec![
        ("/admin/keys", "admin"),
        ("/admin/pools", "admin"),
        ("/admin/users", "admin"),
        ("/api/me/keys", "user"),
    ];

    let mut problems: Vec<String> = Vec::new();
    for (path, who) in &cases {
        let token = if *who == "admin" {
            &bed.admin_token
        } else {
            &bed.user_token
        };
        let (a_body, a) = fetch(&bed, path, token, N * 2, 0).await;
        let (b_body, b) = fetch(&bed, path, token, N, 0).await;
        let (c_body, c) = fetch(&bed, path, token, N, N).await;

        if a.len() < N * 2 {
            problems.push(format!(
                "{path}：窗口内只有 {} 行，不足 {} 行翻不出两页——本用例的前提没满足",
                a.len(),
                N * 2
            ));
            continue;
        }

        let joined: Vec<String> = b.iter().chain(c.iter()).cloned().collect();
        if joined != a {
            problems.push(format!(
                "{path}：分两页取到的顺序/内容与一次取两页不等\n    两页拼接 {joined:?}\n    一次两页 {a:?}"
            ));
        }
        let overlap: Vec<&String> = b.iter().filter(|x| c.contains(x)).collect();
        if !overlap.is_empty() {
            problems.push(format!(
                "{path}：第一页与第二页重叠 {overlap:?}（offset 没生效或排序键不唯一）"
            ));
        }
        let totals = [&a_body, &b_body, &c_body].map(|v| v["total"].clone());
        if totals[0] != totals[1] || totals[1] != totals[2] {
            problems.push(format!(
                "{path}：三次取到的 total 不一致 {totals:?}（total 不该随切片变）"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "{} 个列表端点翻页不正确：\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// 越界的分页 / 窗口参数必须被夹取或拒绝，不得 5xx、不得真回超限行数。
///
/// 与上一条用例的分工：那条验"正常取值下翻页对不对"，这条验"喂垃圾值会怎样"。
/// 后者此前只有 `listing::page_params_are_clamped` 这一个**单元**测试——
/// 它验的是 `PageParams::new` 本身，不保证每个端点真的经由它取参
/// （手写 `limit` 解析绕开夹取，单测照样绿）。
#[tokio::test]
async fn out_of_range_params_are_clamped_not_honoured() {
    /// 与 `crates/okapi-store/src/listing.rs` 的 `MAX_PAGE` 同值。
    const MAX_PAGE: usize = 200;

    let bed = setup().await;

    let cases: Vec<(&str, &str)> = vec![
        ("/admin/keys", "admin"),
        ("/admin/pools", "admin"),
        ("/admin/users", "admin"),
        ("/api/me/keys", "user"),
    ];
    // 每个都该被夹住或拒掉，不该 5xx
    let hostile = ["limit=999999", "limit=0", "limit=-1", "offset=-5"];

    let mut problems: Vec<String> = Vec::new();
    for (path, who) in &cases {
        let token = if *who == "admin" {
            &bed.admin_token
        } else {
            &bed.user_token
        };
        for q in hostile {
            let url = format!("http://{}{path}?{q}", bed.console);
            let resp = reqwest::Client::new()
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            let status = resp.status().as_u16();
            if status >= 500 {
                problems.push(format!("{path}?{q} → {status}：越界参数把 handler 打崩了"));
                continue;
            }
            if status >= 400 {
                continue; // 明确拒绝也是合规答复
            }
            // 行数上限只在**显式传了 limit** 时断言。
            // 不传 limit 的语义是 `Slice::ALL`——配置类列表（渠道池、分组这些供
            // 下拉用的）刻意回全量，只有大表走 `capped_limit`。把"没传 limit 也不许
            // 超 MAX_PAGE"当判据是错的，会把设计当成缺陷（实测 /admin/pools 回 810 行，
            // 查 listing.rs 的注释确认是刻意如此）。
            if !q.starts_with("limit=") {
                continue;
            }
            let body = resp.json::<Value>().await.unwrap_or(Value::Null);
            let n = body["data"].as_array().map_or(0, Vec::len);
            if n > MAX_PAGE {
                problems.push(format!(
                    "{path}?{q} → 回了 {n} 行，超过 MAX_PAGE={MAX_PAGE}：limit 没夹住"
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "{} 处越界参数未被夹取：\n{}",
        problems.len(),
        problems.join("\n")
    );
}
