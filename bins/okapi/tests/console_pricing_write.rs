//! 控制面「配得出来」验收（IMPLEMENTATION §11.21）：引擎实现了、控制面却配不了的三项。
//!
//! 1. `users.price_multiplier`：计价链上与模型倍率、分组倍率并列的乘数，此前全仓只读
//!    （鉴权读它、`/admin/users` 返回它、前端还有一列在展示），没有任何写入路径。
//! 2. `PricingMode::Tiered`：引擎、价簿加载器、`TierTable` 解析器、schema 用例都在，
//!    唯独没有写入口——三种计价模式，控制面只配得出 ratio。
//! 3. `settings.record_ip_log`：docs 写着「记录与否走 settings.record_ip_log」，
//!    但此前全仓无人读它，来源 IP 一律记录，站长关不掉。
//!
//! 依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::response::IntoResponse;
use axum::routing::post;
use okapi::{console, gateway};
use okapi_domain::Money;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use uuid::Uuid;

/// 本文件的用例串行执行。
///
/// `auth_flush()` 是**全局**清空：并行用例里任何一次清空（改角色、改分组、改倍率……）都会
/// 顺带冲掉别的用例刚写进缓存的快照，于是"改完没刷缓存"这类回归会被掩盖——守护它的用例
/// 随调度时序时灵时不灵。变异测试实测：删掉 `assign_role` 里的 `auth_flush()`，默认并行跑
/// SURVIVED，单独跑、单线程跑都 CAUGHT。每个用例开头先拿这把锁。
static SERIAL: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

/// 固定 usage，便于反算：1000 prompt + 200 completion。
async fn mock_ok(body: axum::body::Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap();
    axum::Json(json!({
        "id":"cmpl","object":"chat.completion","model": req["model"],
        "choices":[{"index":0,"message":{"role":"assistant","content":"hi"}}],
        "usage":{"prompt_tokens":1000,"completion_tokens":200}
    }))
    .into_response()
}

async fn serve(router: Router) -> SocketAddr {
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

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    /// 测试用 `gateway::router` 直挂，不像 `gateway::run` 那样带 30s 轮询与 NATS 订阅——
    /// 发布 epoch 后要显式热更价簿，否则请求仍按建 state 时那本书计价。
    state: okapi::gateway::state::AppState,
    user_id: i64,
    token: String,
    admin_token: String,
    model: String,
    gateway: SocketAddr,
    console: SocketAddr,
}

async fn setup() -> Bed {
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let mock = serve(Router::new().route("/v1/chat/completions", post(mock_ok))).await;
    // model_ratio 1.0 / completion 1.0：基线 = (1000 + 200) × 1.0 × $2/1M = 2400 micro
    let model = format!("cpw-m-{suffix}");
    okapi_store::provision::create_model_ratio(&pg, &model, "1.0", "1.0", "1.0")
        .await
        .unwrap();
    okapi_store::provision::create_channel(
        &pg,
        &format!("cpw-ch-{suffix}"),
        "openai",
        &format!("http://{mock}/v1"),
        "cred",
        &[model.as_str()],
        true,
        None,
    )
    .await
    .unwrap();

    let user_id = okapi_store::provision::create_user(&pg, &format!("cpw-u-{suffix}"))
        .await
        .unwrap();
    let token = format!("sk-okapi-cpw-{suffix}");
    okapi_store::provision::create_api_key(&pg, user_id, &hash(&token), "sk-cpw")
        .await
        .unwrap();
    let admin_id = okapi_store::provision::create_user(&pg, &format!("cpw-adm-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", admin_id)
        .execute(&pg)
        .await
        .unwrap();
    let admin_token = format!("sk-okapi-cpw-adm-{suffix}");
    okapi_store::provision::create_api_key(&pg, admin_id, &hash(&admin_token), "sk-cpw-adm")
        .await
        .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    state
        .ledger
        .credit(user_id, Money::from_micros(100_000_000))
        .await
        .unwrap();
    let gateway_addr = serve(gateway::router(state.clone())).await;
    let console_addr = serve(console::router(state.clone())).await;

    Bed {
        pg,
        state,
        user_id,
        token,
        admin_token,
        model,
        gateway: gateway_addr,
        console: console_addr,
    }
}

async fn chat(bed: &Bed) -> u16 {
    reqwest::Client::new()
        .post(format!("http://{}/v1/chat/completions", bed.gateway))
        .bearer_auth(&bed.token)
        .header("x-real-ip", "203.0.113.9")
        .json(&json!({"model": bed.model, "stream": false,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// 等到该用户攒够 n 笔结算，返回**按金额排序**的全部记录。
///
/// 不按 `created_at` 取第 n 笔：结算是响应返回后的后台任务，还要过 `settle_gate` 信号量，
/// 并发下落库顺序不等于调用顺序——按位置断言会偶发读到另一笔（全量回归里就这么挂过一次）。
/// 断言集合而不是序列，与调用顺序解耦。
async fn settlements(pg: &PgPool, user_id: i64, n: usize) -> Vec<(i64, Value, Option<String>)> {
    for _ in 0..120 {
        let rows = sqlx::query!(
            r#"SELECT amount_micro, pricing_snapshot, host(client_ip) AS ip
               FROM billing_records WHERE user_id = $1 AND status = 20
               ORDER BY amount_micro"#,
            user_id
        )
        .fetch_all(pg)
        .await
        .unwrap();
        if rows.len() >= n {
            return rows
                .into_iter()
                .map(|r| {
                    (
                        r.amount_micro,
                        r.pricing_snapshot.unwrap_or(Value::Null),
                        r.ip,
                    )
                })
                .collect();
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    panic!("{n} 笔结算未在期限内落库");
}

/// 用户个人系数：写得进去、进快照、参与连乘、改完立刻生效（鉴权缓存要刷）。
#[tokio::test]
async fn user_multiplier_is_writable_and_billed() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let client = reqwest::Client::new();
    let url = format!(
        "http://{}/admin/users/{}/multiplier",
        bed.console, bed.user_id
    );

    // 基线：系数 1.0 → (1000 + 200) × $2/1M = 2400 micro
    assert_eq!(chat(&bed).await, 200);
    let one = settlements(&bed.pg, bed.user_id, 1).await;
    assert_eq!(one[0].0, 2400);

    // 非法值挡下
    for bad in ["abc", "-1", "1e9999"] {
        let r = client
            .post(&url)
            .bearer_auth(&bed.admin_token)
            .json(&json!({"multiplier": bad}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "非法系数 {bad} 应 400");
    }

    // 设 0.25 → 下一笔立刻按新系数计（不刷鉴权缓存的话最长一分钟仍按旧价）
    let ok = client
        .post(&url)
        .bearer_auth(&bed.admin_token)
        .json(&json!({"multiplier": "0.25"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(chat(&bed).await, 200);
    let two = settlements(&bed.pg, bed.user_id, 2).await;
    let amounts: Vec<i64> = two.iter().map(|r| r.0).collect();
    assert_eq!(amounts, vec![600, 2400], "两笔分别是 2400 与 2400×0.25");
    let (_, snap, _) = &two[0];
    assert_eq!(snap["user_multiplier"], 0.25, "系数进快照，账单可解释");

    // 不存在的用户 → 404
    let missing = client
        .post(format!(
            "http://{}/admin/users/99999999/multiplier",
            bed.console
        ))
        .bearer_auth(&bed.admin_token)
        .json(&json!({"multiplier": "1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

/// 阶梯计价：写得进去、切得回来、非法表在写入这一步就拦下（而不是等编译价簿时炸）。
#[tokio::test]
async fn tiered_pricing_is_writable_and_billed() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let client = reqwest::Client::new();
    let url = format!("http://{}/admin/models", bed.console);
    let put = |body: Value| {
        let client = client.clone();
        let url = url.clone();
        let token = bed.admin_token.clone();
        async move {
            client
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };

    // 非法阶梯表：首档不从 0 起 / 乱序 / 解析不了 —— 一律 400，param 带原因
    for bad in ["100:2", "0:2,50:3,10:4", "not-a-table"] {
        let r = put(json!({"model_name": bed.model, "model_ratio": "1.0", "tier_expr": bad})).await;
        assert_eq!(r.status(), 400, "非法阶梯表 {bad} 应 400");
        let body: Value = r.json().await.unwrap();
        assert!(
            body["error"]["param"]
                .as_str()
                .unwrap_or_default()
                .starts_with("tier_expr:"),
            "param 要指明是阶梯表的问题：{body}"
        );
    }

    // 合法阶梯表：0 起 $5/1M，1000 token 起 $1/1M。本次 usage 合计 1200 raw token
    // → 命中第二档 $1/1M → 等效 model_ratio 0.5 → (1000 + 200) × 0.5 × $2/1M = 1200 micro
    let r = put(json!({"model_name": bed.model, "model_ratio": "1.0",
                       "tier_expr": "0:5,1000:1"}))
    .await;
    assert_eq!(r.status(), 200);
    let published = client
        .post(format!("http://{}/admin/pricing/publish", bed.console))
        .bearer_auth(&bed.admin_token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), 200);
    assert!(
        gateway::refresh_pricebook_if_newer(&bed.state)
            .await
            .unwrap(),
        "发布后价簿应热更到新 epoch"
    );

    assert_eq!(chat(&bed).await, 200);
    let one = settlements(&bed.pg, bed.user_id, 1).await;
    let (amount, snap, _) = &one[0];
    assert_eq!(snap["mode"], "tiered", "快照要标明是阶梯模式：{snap}");
    assert_eq!(*amount, 1200, "1200 raw token 命中 $1/1M 档");

    // 切回 ratio：阶梯表必须被清掉，否则下次误切 tiered 会拿到一张过期旧表
    let back = put(json!({"model_name": bed.model, "model_ratio": "1.0", "tier_expr": ""})).await;
    assert_eq!(back.status(), 200);
    let row = sqlx::query!(
        r#"SELECT p.pricing_mode, p.tier_expr FROM model_pricing p
           JOIN models m ON m.id = p.model_id WHERE m.model_name = $1"#,
        bed.model
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    assert_eq!(row.pricing_mode, "ratio");
    assert!(row.tier_expr.is_none(), "切回 ratio 要清空阶梯表");
}

/// 来源 IP 记录开关：关掉后 PG 列与 outbox 载荷都不落 IP。
///
/// 注：`record_ip_log` 是站点级设置（docs 如此定义），本用例会短暂改全局值。窗口压到
/// 「一次请求 + 立即复原」，且改的是缺省缺失的键——跑完即删，不留状态。
#[tokio::test]
async fn record_ip_log_switch_suppresses_client_ip() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;

    // 缺省（键不存在）= 记录
    assert_eq!(chat(&bed).await, 200);
    let one = settlements(&bed.pg, bed.user_id, 1).await;
    assert_eq!(one[0].2.as_deref(), Some("203.0.113.9"), "缺省仍要记 IP");

    // 关掉：新建 state 让 settings 进程缓存是冷的（缓存 60s，复用旧 state 读不到新值）
    sqlx::query!(
        r#"INSERT INTO settings (key, value) VALUES ('record_ip_log', 'false'::jsonb)
           ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"#
    )
    .execute(&bed.pg)
    .await
    .unwrap();
    let database_url = std::env::var("DATABASE_URL").unwrap();
    let redis_url = std::env::var("OKAPI_REDIS_URL").unwrap();
    let state = gateway::build_state(&database_url, &redis_url, "test-node", None, None)
        .await
        .unwrap();
    assert!(!state.record_ip_log().await, "设置为 false 时开关应关闭");
    let gw = serve(gateway::router(state)).await;
    let status = reqwest::Client::new()
        .post(format!("http://{gw}/v1/chat/completions"))
        .bearer_auth(&bed.token)
        .header("x-real-ip", "203.0.113.9")
        .json(&json!({"model": bed.model, "stream": false,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    // 复原立刻做，尽量缩短全局设置被改动的窗口
    sqlx::query!(r#"DELETE FROM settings WHERE key = 'record_ip_log'"#)
        .execute(&bed.pg)
        .await
        .unwrap();
    assert_eq!(status, 200);

    let two = settlements(&bed.pg, bed.user_id, 2).await;
    let without_ip = two.iter().filter(|r| r.2.is_none()).count();
    assert_eq!(without_ip, 1, "开关关闭后那一笔不得落 IP：{two:?}");
    let payload_ip = sqlx::query_scalar!(
        r#"SELECT payload->>'client_ip' FROM billing_outbox
           WHERE payload->>'user_id' = $1 ORDER BY id DESC LIMIT 1"#,
        bed.user_id.to_string()
    )
    .fetch_optional(&bed.pg)
    .await
    .unwrap()
    .flatten();
    assert!(
        payload_ip.is_none(),
        "outbox 载荷（→ CH）同样不得带 IP，实际 {payload_ip:?}"
    );
}

/// 改用户分组：写入前校验、写入后立刻按新分组计价。
///
/// 四件事此前都没被钉住，前三件都会回 500：
/// - 分组写进 `price_groups` 要等发布才进价簿。放进一个**未发布**的分组，写入照样 200，
///   而该用户之后每笔请求都撞 `UnknownGroup`（fail-closed 回 500），直到有人发布——
///   现在写入这一步就回 400 `group_not_published`，用户不受影响；
/// - 放进**不存在**的分组撞 `user_groups` 外键，被当成内部错误回 500——现在回 400 `group_code`；
/// - 给**不存在**的用户设分组撞 `user_id` 外键回 500——现在回 404，与改角色 / 改倍率一致；
/// - 生效分组随 `AuthedKey` 缓存，写完调 `auth_flush()`；变异测试删掉这一行全量无一变红。
///
/// 发布后**不显式刷新价簿**就去改分组：`set_user_groups` 自己会按需追最新 epoch——
/// 未配 NATS 的 console 靠这一步才不会把刚发布的分组误拒。
/// 新分组沿用当前生效分组的渠道池，两笔请求路由相同、只有倍率不同。
#[tokio::test]
async fn group_assignment_is_validated_and_billed_immediately() {
    let _serial = SERIAL.lock().await;
    let bed = setup().await;
    let client = reqwest::Client::new();
    let post = |path: String, body: Value| {
        let client = client.clone();
        let url = format!("http://{}{path}", bed.console);
        let token = bed.admin_token.clone();
        async move {
            let r = client
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = r.status().as_u16();
            (status, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let groups_path = format!("/admin/users/{}/groups", bed.user_id);

    // 基线：默认分组 → (1000 + 200) × $2/1M = 2400 micro
    assert_eq!(chat(&bed).await, 200);
    assert_eq!(settlements(&bed.pg, bed.user_id, 1).await[0].0, 2400);

    let group = format!("cpwg{}", &Uuid::new_v4().simple().to_string()[..10]);
    // 运行期检查的查询：只在本用例里用，不值得为它进 .sqlx 离线缓存（CI 以 SQLX_OFFLINE=true 编译）
    let pool: String = sqlx::query_scalar(
        "SELECT COALESCE((SELECT pool_code FROM price_groups WHERE is_default LIMIT 1), 'default')",
    )
    .fetch_one(&bed.pg)
    .await
    .unwrap();
    let (status, body) = post(
        "/admin/groups".to_owned(),
        json!({"group_code": group, "group_ratio": "0.5", "pool_code": pool}),
    )
    .await;
    assert_eq!(status, 200, "建分组应成功：{body}");

    // 未发布：拒绝，且用户照常按旧分组计价
    let (status, body) = post(
        groups_path.clone(),
        json!({"groups": [{"group_code": group, "priority": 10}]}),
    )
    .await;
    assert_eq!(status, 400, "未发布的分组不得分配给用户：{body}");
    assert_eq!(body["error"]["param"], "group_not_published", "{body}");
    assert_eq!(chat(&bed).await, 200, "被拒的分配不得影响用户的请求");

    // 不存在：拒绝（而不是撞外键回 500）
    let (status, body) = post(
        groups_path.clone(),
        json!({"groups": [{"group_code": "no-such-group-cpw", "priority": 1}]}),
    )
    .await;
    assert_eq!(status, 400, "不存在的分组应 400 而不是 500：{body}");
    assert_eq!(body["error"]["param"], "group_code", "{body}");

    // 发布后分配：不显式刷新价簿，由写入端按需追 epoch
    let (status, body) = post("/admin/pricing/publish".to_owned(), json!({})).await;
    assert_eq!(status, 200, "发布应成功：{body}");
    let (status, body) = post(
        groups_path,
        json!({"groups": [{"group_code": group, "priority": 10}]}),
    )
    .await;
    assert_eq!(status, 200, "已发布的分组应能分配：{body}");

    // 不存在的用户：404，与改角色 / 改倍率一致——此前非空分组会撞 user_id 外键回 500
    let (status, body) = post(
        "/admin/users/999999999/groups".to_owned(),
        json!({"groups": [{"group_code": group, "priority": 10}]}),
    )
    .await;
    assert_eq!(status, 404, "不存在的用户应 404 而不是 500：{body}");

    // 下一笔立刻按新分组计价
    assert_eq!(chat(&bed).await, 200);
    let three = settlements(&bed.pg, bed.user_id, 3).await;
    let amounts: Vec<i64> = three.iter().map(|r| r.0).collect();
    assert_eq!(
        amounts,
        vec![1200, 2400, 2400],
        "改分组后的下一笔应按新组倍率 0.5 计价——鉴权缓存没有随改分组刷新"
    );
    let (_, snap, _) = &three[0];
    assert_eq!(snap["group"], group.as_str(), "生效分组进快照，账单可解释");
    assert_eq!(snap["group_ratio"], 0.5);
}
