//! 渠道池调度策略中心（Phase 1）端到端验收测试：
//! 1. `POST /admin/pools` 策略持久化、版本号递增、无 `policy` 时保留原策略、多跳环检测、策略引用防误删；
//! 2. 成员 `role / enabled / tags / model_filter` 持久化与 `POST /admin/pools/{code}/simulate`
//!    （已保存策略 + 未保存 `draft_policy` 双轨模拟，覆盖长短上下文规则、灰度桶、角色分层、单池摘流）；
//! 3. 真实 `gateway` `/v1/chat/completions` 热路径验证：长短上下文分流、池级 `failover.max_attempts` 生效、
//!    以及 `affinity.mode = "off"` 不写会话粘性。

use axum::routing::post;
use okapi_console as console;
use okapi_gateway as gateway;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

fn hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Bed {
    pg: PgPool,
    state: okapi_engine::state::AppState,
    console: SocketAddr,
    gateway: SocketAddr,
    super_token: String,
    user_token: String,
    user_id: i64,
    user_key_id: i64,
    suffix: String,
}

async fn setup() -> Bed {
    okapi_store::test_support::assert_isolated();
    let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
    let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
    let pg = okapi_store::connect_pg(&database_url).await.unwrap();
    okapi_store::run_migrations(&pg).await.unwrap();
    let suffix = Uuid::new_v4().simple().to_string()[..10].to_owned();

    let user_id = okapi_store::provision::create_user(&pg, &format!("ppc-u-{suffix}"))
        .await
        .unwrap();
    let user_token = format!("sk-okapi-ppc-user-{suffix}");
    let user_key_id =
        okapi_store::provision::create_api_key(&pg, user_id, &hash(&user_token), "sk-ppc-u")
            .await
            .unwrap();

    let super_id = okapi_store::provision::create_user(&pg, &format!("ppc-s-{suffix}"))
        .await
        .unwrap();
    sqlx::query!("UPDATE users SET role = 100 WHERE id = $1", super_id)
        .execute(&pg)
        .await
        .unwrap();
    let super_token = format!("sk-okapi-ppc-super-{suffix}");
    okapi_store::provision::create_api_key(&pg, super_id, &hash(&super_token), "sk-ppc-s")
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO settings(key, value) VALUES('ssrf_policy', $1) \
         ON CONFLICT(key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(json!({ "allow_private": true }))
    .execute(&pg)
    .await
    .unwrap();

    let state = gateway::build_state(&database_url, &redis_url, "ppc-node", None, None)
        .await
        .unwrap();

    let console_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let console = console_listener.local_addr().unwrap();
    let console_app = console::router(state.clone());
    tokio::spawn(async move {
        axum::serve(console_listener, console_app).await.unwrap();
    });

    let gw_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_addr = gw_listener.local_addr().unwrap();
    let gw_app = gateway::router(state.clone());
    tokio::spawn(async move {
        axum::serve(gw_listener, gw_app).await.unwrap();
    });

    let bed = Bed {
        pg,
        state,
        console,
        gateway: gateway_addr,
        super_token,
        user_token,
        user_id,
        user_key_id,
        suffix,
    };
    let (st, v) = admin_call(
        &bed,
        "POST",
        &format!("/admin/users/{}/credit", bed.user_id),
        Some(json!({ "amount_micro": 50_000_000, "remark": "ppc-test" })),
    )
    .await;
    assert_eq!(st, 200, "credit test user: {v}");
    bed
}

async fn admin_call(bed: &Bed, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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

async fn spawn_mock_openai(reply_tag: &'static str, fail_500: bool) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let hits = Arc::clone(&hits_clone);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                if fail_500 {
                    return (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(json!({"error": {"message": "upstream boom"}})),
                    );
                }
                (
                    axum::http::StatusCode::OK,
                    axum::Json(json!({
                        "id": format!("chatcmpl-{reply_tag}"),
                        "object": "chat.completion",
                        "created": 1_700_000_000,
                        "model": "mock-model",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": reply_tag},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 10,
                            "completion_tokens": 5,
                            "total_tokens": 15
                        }
                    })),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), hits)
}

#[tokio::test]
async fn pool_policy_crud_versioning_cycle_guard_and_delete_protection() {
    let bed = setup().await;
    let p1 = format!("p1-{}", &bed.suffix[..6]);
    let p2 = format!("p2-{}", &bed.suffix[..6]);
    let p3 = format!("p3-{}", &bed.suffix[..6]);

    for code in [&p1, &p2, &p3] {
        let (st, v) = admin_call(
            &bed,
            "POST",
            "/admin/pools",
            Some(json!({
                "pool_code": code,
                "description": "init",
                "routing_strategy": "priority_weighted"
            })),
        )
        .await;
        assert_eq!(st, 200, "create pool {code}: {v}");
        assert_eq!(v["policy_version"], 1);
    }

    // 1. 写入多跳策略文档，版本号递增至 2
    let (st, v) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": p1,
            "description": "primary pool",
            "routing_strategy": "priority_weighted",
            "policy": {
                "fallback_pools": [p2, p3],
                "canary_percent": 25,
                "failover": { "max_attempts": 3 }
            }
        })),
    )
    .await;
    assert_eq!(st, 200, "upsert policy: {v}");
    assert_eq!(v["policy_version"], 2);

    // 2. 不传 policy 字段更新描述：应保留原有 policy 与 policy_version
    let (st, v) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": p1,
            "description": "updated desc only",
            "routing_strategy": "priority_weighted"
        })),
    )
    .await;
    assert_eq!(st, 200, "desc-only update: {v}");
    assert_eq!(v["policy_version"], 2);

    let (st, detail) = admin_call(&bed, "GET", &format!("/admin/pools/{p1}"), None).await;
    assert_eq!(st, 200);
    assert_eq!(detail["policy_version"], 2);
    assert_eq!(detail["policy"]["canary_percent"], 25);
    assert_eq!(detail["policy"]["fallback_pools"], json!([p2, p3]));

    // 3. 环检测：p3 -> p1 会与 p1 -> [p2, p3] 成环，必须 400 拒绝
    let (st, err) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": p3,
            "description": "cycle attempt",
            "routing_strategy": "priority_weighted",
            "policy": { "fallback_pools": [p1] }
        })),
    )
    .await;
    assert_eq!(st, 400, "cycle must be rejected: {err}");

    // 4. 删除被 policy.fallback_pools 引用的 p3 必须回 409 pool_in_use
    let (st, err) = admin_call(&bed, "DELETE", &format!("/admin/pools/{p3}"), None).await;
    assert_eq!(
        st, 409,
        "pool referenced by policy must not be deleted: {err}"
    );

    // 5. 移除 p1 对 p3 的引用后可正常删除 p3
    let (st, v) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": p1,
            "description": "remove p3",
            "routing_strategy": "priority_weighted",
            "policy": { "fallback_pools": [p2] }
        })),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(v["policy_version"], 3);

    let (st, _) = admin_call(&bed, "DELETE", &format!("/admin/pools/{p3}"), None).await;
    assert_eq!(st, 200);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn pool_policy_simulate_and_member_metadata_end_to_end() {
    let bed = setup().await;
    let main_pool = format!("pm-{}", &bed.suffix[..6]);
    let fb_pool = format!("pf-{}", &bed.suffix[..6]);
    let claude_model = format!("claude-sim-{}", bed.suffix);
    let gpt_model = format!("gpt-sim-{}", bed.suffix);

    for code in [&main_pool, &fb_pool] {
        let (st, _) = admin_call(
            &bed,
            "POST",
            "/admin/pools",
            Some(json!({ "pool_code": code, "description": code })),
        )
        .await;
        assert_eq!(st, 200);
    }

    // 创建 5 个成员渠道，覆盖 primary / backup / canary / tags / model_filter / per-pool enabled=false
    let mk_ch = |name: &str, cost_milli: i64, pools: Value| {
        json!({
            "name": name,
            "provider": "openai",
            "api_base": "https://api.example.invalid/v1",
            "credential": "sk-test-cred",
            "models": [claude_model, gpt_model],
            "cost_milli": cost_milli,
            "pools": pools,
        })
    };

    let (st, ch_short) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(mk_ch(
            &format!("short-{}", bed.suffix),
            1000,
            json!([{
                "pool_code": main_pool,
                "priority_override": 20,
                "role": "primary",
                "tags": ["fast"]
            }]),
        )),
    )
    .await;
    assert_eq!(st, 200, "ch_short: {ch_short}");
    let ch_short_id = ch_short["channel_id"].as_i64().unwrap();

    let (st, ch_long) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(mk_ch(
            &format!("long-{}", bed.suffix),
            1200,
            json!([{
                "pool_code": main_pool,
                "priority_override": 10,
                "role": "primary",
                "tags": ["long_ctx"],
                "model_filter": ["claude-*"]
            }]),
        )),
    )
    .await;
    assert_eq!(st, 200, "ch_long: {ch_long}");
    let ch_long_id = ch_long["channel_id"].as_i64().unwrap();

    let (st, ch_canary) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(mk_ch(
            &format!("canary-{}", bed.suffix),
            1000,
            json!([{
                "pool_code": main_pool,
                "priority_override": 1,
                "role": "canary",
                "tags": ["fast", "long_ctx"]
            }]),
        )),
    )
    .await;
    assert_eq!(st, 200, "ch_canary: {ch_canary}");
    let ch_canary_id = ch_canary["channel_id"].as_i64().unwrap();

    let (st, ch_backup) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(mk_ch(
            &format!("backup-{}", bed.suffix),
            1000,
            json!([{
                "pool_code": main_pool,
                "priority_override": 999,
                "role": "backup",
                "tags": ["fast"]
            }]),
        )),
    )
    .await;
    assert_eq!(st, 200, "ch_backup: {ch_backup}");
    let ch_backup_id = ch_backup["channel_id"].as_i64().unwrap();

    // 在主池摘流（enabled=false），在降级池启用（enabled=true）
    let (st, ch_drained) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(mk_ch(
            &format!("drained-{}", bed.suffix),
            1000,
            json!([
                { "pool_code": main_pool, "priority_override": 500, "role": "primary", "enabled": false, "tags": ["fast"] },
                { "pool_code": fb_pool, "priority_override": 10, "role": "primary", "enabled": true, "tags": ["fast", "long_ctx"] }
            ]),
        )),
    )
    .await;
    assert_eq!(st, 200, "ch_drained: {ch_drained}");
    let ch_drained_id = ch_drained["channel_id"].as_i64().unwrap();

    // 配置主池策略：fallback 到 fb_pool，长上下文（>=32000）走 required_tags=["long_ctx"]
    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": main_pool,
            "description": "sim policy",
            "routing_strategy": "priority_weighted",
            "policy": {
                "fallback_pools": [fb_pool],
                "canary_percent": 0,
                "rules": [{
                    "name": "long_context",
                    "when": { "min_prompt_tokens": 32000 },
                    "apply": { "guardrails": { "required_tags": ["long_ctx"] } }
                }]
            }
        })),
    )
    .await;
    assert_eq!(st, 200);

    // 校验池详情完整回显成员元数据
    let (st, detail) = admin_call(&bed, "GET", &format!("/admin/pools/{main_pool}"), None).await;
    assert_eq!(st, 200);
    let drained_row = detail["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["channel_id"] == ch_drained_id)
        .unwrap();
    assert_eq!(drained_row["enabled"], false);

    // 1. 短上下文模拟 + draft_policy 打开 100% 灰度对比
    let (st, sim) = admin_call(
        &bed,
        "POST",
        &format!("/admin/pools/{main_pool}/simulate"),
        Some(json!({
            "model": claude_model,
            "prompt_tokens": 1000,
            "session_key": "sess-alpha",
            "draft_policy": {
                "fallback_pools": [fb_pool],
                "canary_percent": 100
            }
        })),
    )
    .await;
    assert_eq!(st, 200, "simulate short: {sim}");
    assert_eq!(sim["saved"]["canary_selected"], false);
    let saved_ids: Vec<i64> = sim["saved"]["ordered_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["channel_id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        saved_ids,
        vec![ch_short_id, ch_long_id, ch_backup_id, ch_drained_id],
        "主池 primary(prio 20 -> 10) -> backup(即使 prio 999) -> 降级池 drained(pool_rank=1)"
    );
    assert_eq!(
        sim["saved"]["ordered_candidates"][3]["pool_rank"], 1,
        "主池摘流的渠道从降级池进入，pool_rank 必须是 1"
    );
    assert!(
        sim["saved"]["rejected_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["channel_id"] == ch_canary_id && r["reason"] == "canary_not_selected")
    );
    // draft_policy 100% 灰度：canary 跃升首位
    assert_eq!(sim["draft"]["canary_selected"], true);
    assert_eq!(
        sim["draft"]["ordered_candidates"][0]["channel_id"],
        ch_canary_id
    );

    // 2. 长上下文模拟：命中 long_context 规则，仅留带 long_ctx 标签的候选
    let (st, sim_long) = admin_call(
        &bed,
        "POST",
        &format!("/admin/pools/{main_pool}/simulate"),
        Some(json!({
            "model": claude_model,
            "prompt_tokens": 40000
        })),
    )
    .await;
    assert_eq!(st, 200, "simulate long: {sim_long}");
    assert_eq!(
        sim_long["saved"]["effective_policy"]["matched_rules"],
        json!(["long_context"])
    );
    let long_ids: Vec<i64> = sim_long["saved"]["ordered_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["channel_id"].as_i64().unwrap())
        .collect();
    assert_eq!(long_ids, vec![ch_long_id, ch_drained_id]);

    // 3. gpt-* 模型模拟：ch_long 的 model_filter=["claude-*"] 应将其过滤
    let (st, sim_gpt) = admin_call(
        &bed,
        "POST",
        &format!("/admin/pools/{main_pool}/simulate"),
        Some(json!({
            "model": gpt_model,
            "prompt_tokens": 500
        })),
    )
    .await;
    assert_eq!(st, 200);
    assert!(
        sim_gpt["saved"]["rejected_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["channel_id"] == ch_long_id && r["reason"] == "member_model_filter")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn pool_policy_live_gateway_routing_context_split_and_max_attempts() {
    let bed = setup().await;
    let pool_code = format!("pl-{}", &bed.suffix[..6]);
    let model = format!("live-ppc-{}", bed.suffix);

    // 注册模型并发布定价，确保真实 gateway 链路通过准入与结算
    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/models",
        Some(json!({
            "model_name": model,
            "display_name": model,
            "vendor": "openai",
            "pricing_mode": "ratio",
            "model_ratio": "1",
            "completion_ratio": "1"
        })),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = admin_call(&bed, "POST", "/admin/pricing/publish", None).await;
    assert_eq!(st, 200);
    okapi_engine::refresh_pricebook_if_newer(&bed.state)
        .await
        .unwrap();

    // 建池并绑定到测试用户 key
    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": pool_code,
            "description": "live test pool",
            "policy": {
                "affinity": { "mode": "off" },
                "rules": [{
                    "name": "long_prompt_route",
                    "when": { "min_prompt_tokens": 80 },
                    "apply": { "guardrails": { "required_tags": ["long_ctx"] } }
                }]
            }
        })),
    )
    .await;
    assert_eq!(st, 200);
    sqlx::query("UPDATE api_keys SET pool_override = $1 WHERE id = $2")
        .bind(&pool_code)
        .bind(bed.user_key_id)
        .execute(&bed.pg)
        .await
        .unwrap();
    bed.state.sched.auth_flush().await;
    bed.state.invalidate_routing_caches();

    let (short_base, short_hits) = spawn_mock_openai("SHORT_UPSTREAM", false).await;
    let (long_base, long_hits) = spawn_mock_openai("LONG_UPSTREAM", false).await;

    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("live-short-{}", bed.suffix),
            "provider": "openai",
            "api_base": short_base,
            "credential": "sk-short",
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 20, "tags": ["short_ctx"] }]
        })),
    )
    .await;
    assert_eq!(st, 200);

    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("live-long-{}", bed.suffix),
            "provider": "openai",
            "api_base": long_base,
            "credential": "sk-long",
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 10, "tags": ["long_ctx"] }]
        })),
    )
    .await;
    assert_eq!(st, 200);

    let client = reqwest::Client::new();
    let gw_url = format!("http://{}/v1/chat/completions", bed.gateway);

    // 1. 短上下文请求 -> 走 priority 20 的 SHORT_UPSTREAM，且 affinity=off 不写粘性
    let resp = client
        .post(&gw_url)
        .bearer_auth(&bed.user_token)
        .header("x-session-id", "sess-no-sticky")
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "SHORT_UPSTREAM");
    assert_eq!(short_hits.load(Ordering::SeqCst), 1);
    assert!(
        bed.state
            .sched
            .sticky_get_for_model(bed.user_id, &model, "sess-no-sticky")
            .await
            .is_none(),
        "affinity.mode = off 不应写入粘性映射"
    );

    // 2. 长上下文请求（超过 80 tokens）-> 命中 long_prompt_route 规则，直切 LONG_UPSTREAM
    let long_text = "architecture scheduling policy ".repeat(40);
    let resp = client
        .post(&gw_url)
        .bearer_auth(&bed.user_token)
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": long_text}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "LONG_UPSTREAM");
    assert_eq!(long_hits.load(Ordering::SeqCst), 1);

    // 3. 池级 failover.max_attempts = 1：加入最高优先级必败渠道（500），
    //    限制只尝试 1 个候选且同渠道重试 0 次 -> SHORT_UPSTREAM 不应被改投命中
    let (fail_base, fail_hits) = spawn_mock_openai("FAIL_UPSTREAM", true).await;
    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("live-fail-{}", bed.suffix),
            "provider": "openai",
            "api_base": fail_base,
            "credential": "sk-fail",
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 100, "tags": ["short_ctx"] }]
        })),
    )
    .await;
    assert_eq!(st, 200);

    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": pool_code,
            "description": "strict max_attempts=1",
            "policy": {
                "failover": { "max_attempts": 1, "max_same_channel_retries": 0 }
            }
        })),
    )
    .await;
    assert_eq!(st, 200);

    let before_short = short_hits.load(Ordering::SeqCst);
    let resp = client
        .post(&gw_url)
        .bearer_auth(&bed.user_token)
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error(),
        "max_attempts=1 命中必败渠道后应直接失败，实际 status={}",
        resp.status()
    );
    assert_eq!(fail_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        short_hits.load(Ordering::SeqCst),
        before_short,
        "max_attempts=1 不应改投到第二个候选"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn pool_expiry_urgency_strategy_crud_validation_and_dry_run_scoring() {
    use fred::interfaces::KeysInterface as _;
    use sha2::{Digest, Sha256};

    let bed = setup().await;
    let pool_code = format!("pu-{}", &bed.suffix[..6]);
    let model = format!("claude-urg-{}", bed.suffix);

    // 1. 参数越界校验（long_window_weight > 20000 应 400）
    let (st, err) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": pool_code,
            "routing_strategy": "expiry_urgency",
            "policy": {
                "selection": {
                    "urgency": { "long_window_weight": 99999 }
                }
            }
        })),
    )
    .await;
    assert_eq!(st, 400, "invalid urgency weight must fail: {err}");

    // 2. 合法创建 expiry_urgency 策略池
    let (st, created) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": pool_code,
            "description": "subscription multi-window urgency pool",
            "routing_strategy": "expiry_urgency",
            "policy": {
                "selection": {
                    "urgency": {
                        "short_window_weight": 1000,
                        "long_window_weight": 3500,
                        "heavy_prompt_tokens": 32000,
                        "heavy_min_headroom_pct": 20
                    }
                }
            }
        })),
    )
    .await;
    assert_eq!(st, 200, "create expiry_urgency pool: {created}");

    let now = chrono::Utc::now().timestamp();
    // 创建两条同优先级 anthropic_max 订阅渠道并种入 Redis 额度观测
    let cred_a = json!({
        "kind": "oauth",
        "access_token": "tok-a",
        "refresh_token": "ref-a",
        "expires_at": now + 7200,
        "account_id": "acct-a"
    })
    .to_string();
    let cred_b = json!({
        "kind": "oauth",
        "access_token": "tok-b",
        "refresh_token": "ref-b",
        "expires_at": now + 7200,
        "account_id": "acct-b"
    })
    .to_string();

    let (st, ch_a) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("urg-a-{}", bed.suffix),
            "provider": "anthropic_max",
            "api_base": "https://api.anthropic.com",
            "credential": cred_a,
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 10 }]
        })),
    )
    .await;
    assert_eq!(st, 200, "ch_a: {ch_a}");
    let five_hour_ch_id = ch_a["channel_id"].as_i64().unwrap();
    let five_hour_key_id = ch_a["channel_key_id"].as_i64().unwrap();

    let (st, ch_b) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("urg-b-{}", bed.suffix),
            "provider": "anthropic_max",
            "api_base": "https://api.anthropic.com",
            "credential": cred_b,
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 10 }]
        })),
    )
    .await;
    assert_eq!(st, 200, "ch_b: {ch_b}");
    let weekly_ch_id = ch_b["channel_id"].as_i64().unwrap();
    let weekly_key_id = ch_b["channel_key_id"].as_i64().unwrap();

    let identity_of =
        |acct: &str| hex::encode(Sha256::digest(format!("anthropic_max:{acct}").as_bytes()));

    // A：5h 剩 25m 但只剩 10% 余量（低余量，低于大任务 20% 门槛）；7d 刚刷新还剩 6 天
    let obs_five_hour = json!({
        "identity": identity_of("acct-a"),
        "snapshot": {
            "observed_at": now,
            "allowed": true,
            "windows": [
                { "name": "five_hour", "used_percent": 90, "resets_at": now + 1500, "window_secs": 18000 },
                { "name": "seven_day", "used_percent": 5, "resets_at": now + 6 * 86400, "window_secs": 7 * 86400 }
            ]
        }
    });
    // B：5h 剩 2h（余量 80%）；7d 还剩 8h 且剩 60% 没用完 -> 临期大块周额度，应优先清仓！
    let obs_weekly = json!({
        "identity": identity_of("acct-b"),
        "snapshot": {
            "observed_at": now,
            "allowed": true,
            "windows": [
                { "name": "five_hour", "used_percent": 20, "resets_at": now + 7200, "window_secs": 18000 },
                { "name": "seven_day", "used_percent": 40, "resets_at": now + 8 * 3600, "window_secs": 7 * 86400 }
            ]
        }
    });
    let _: () = bed
        .state
        .sched
        .client()
        .set(
            format!("quota:ck:{five_hour_key_id}"),
            obs_five_hour.to_string(),
            None,
            None,
            false,
        )
        .await
        .unwrap();
    let _: () = bed
        .state
        .sched
        .client()
        .set(
            format!("quota:ck:{weekly_key_id}"),
            obs_weekly.to_string(),
            None,
            None,
            false,
        )
        .await
        .unwrap();

    // 3. Dry-Run 模拟：B（临期大块周额度）必须排在 A 前面，并带出 urgency_score 与 dominant_window = seven_day
    let (st, sim) = admin_call(
        &bed,
        "POST",
        &format!("/admin/pools/{pool_code}/simulate"),
        Some(json!({
            "model": model,
            "prompt_tokens": 2000
        })),
    )
    .await;
    assert_eq!(st, 200, "simulate urgency: {sim}");
    let ordered = sim["saved"]["ordered_candidates"].as_array().unwrap();
    assert_eq!(ordered.len(), 2);
    assert_eq!(
        ordered[0]["channel_id"], weekly_ch_id,
        "临期大块周额度 B 应排首位: {sim}"
    );
    assert_eq!(ordered[0]["dominant_window"], "seven_day");
    assert!(
        ordered[0]["urgency_score"].as_u64().unwrap()
            > ordered[1]["urgency_score"].as_u64().unwrap()
    );
    assert_eq!(ordered[1]["channel_id"], five_hour_ch_id);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn pool_quality_auto_demotion_end_to_end() {
    let bed = setup().await;
    let pool_code = format!("pq-{}", &bed.suffix[..6]);
    let model = format!("live-p2-{}", bed.suffix);

    let (st, _) = admin_call(
        &bed,
        "POST",
        "/admin/models",
        Some(json!({
            "model_name": model,
            "display_name": model,
            "vendor": "openai",
            "pricing_mode": "ratio",
            "model_ratio": "1",
            "completion_ratio": "1"
        })),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = admin_call(&bed, "POST", "/admin/pricing/publish", None).await;
    assert_eq!(st, 200);
    okapi_engine::refresh_pricebook_if_newer(&bed.state)
        .await
        .unwrap();

    // 1. 建池：启用低质量自动降级（15% 降权、60% 硬摘）
    let (st, v) = admin_call(
        &bed,
        "POST",
        "/admin/pools",
        Some(json!({
            "pool_code": pool_code,
            "description": "phase2 quality pool",
            "routing_strategy": "priority_weighted",
            "policy": {
                "quality": {
                    "enabled": true,
                    "min_samples": 10,
                    "demote_error_rate_milli": 150,
                    "eject_error_rate_milli": 600
                },
                "failover": { "max_same_channel_retries": 0 }
            }
        })),
    )
    .await;
    assert_eq!(st, 200, "create phase2 pool: {v}");

    let (ok_base, _) = spawn_mock_openai("OK", false).await;
    let (fail_base, _) = spawn_mock_openai("FAIL_500", true).await;

    // 3 条同优先级渠道：healthy（0% 错）、noisy（30% 错 -> 降权）、broken（80% 错 -> 硬摘）
    let (st, ch_healthy) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("p2-healthy-{}", bed.suffix),
            "provider": "openai",
            "api_base": ok_base,
            "credential": "sk-h",
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 10 }]
        })),
    )
    .await;
    assert_eq!(st, 200);
    let healthy_ch_id = ch_healthy["channel_id"].as_i64().unwrap();
    let healthy_key_id = ch_healthy["channel_key_id"].as_i64().unwrap();

    let (st, ch_noisy) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("p2-noisy-{}", bed.suffix),
            "provider": "openai",
            "api_base": ok_base,
            "credential": "sk-n",
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 10 }]
        })),
    )
    .await;
    assert_eq!(st, 200);
    let noisy_ch_id = ch_noisy["channel_id"].as_i64().unwrap();
    let noisy_key_id = ch_noisy["channel_key_id"].as_i64().unwrap();

    let (st, ch_broken) = admin_call(
        &bed,
        "POST",
        "/admin/channels",
        Some(json!({
            "name": format!("p2-broken-{}", bed.suffix),
            "provider": "openai",
            "api_base": fail_base,
            "credential": "sk-b",
            "models": [model],
            "pools": [{ "pool_code": pool_code, "priority_override": 10 }]
        })),
    )
    .await;
    assert_eq!(st, 200);
    let broken_ch_id = ch_broken["channel_id"].as_i64().unwrap();
    let broken_key_id = ch_broken["channel_key_id"].as_i64().unwrap();

    // 写入滚动质量样本
    for _ in 0..10 {
        bed.state
            .sched
            .channel_key_quality_record(healthy_key_id, true)
            .await;
    }
    for _ in 0..7 {
        bed.state
            .sched
            .channel_key_quality_record(noisy_key_id, true)
            .await;
    }
    for _ in 0..3 {
        bed.state
            .sched
            .channel_key_quality_record(noisy_key_id, false)
            .await;
    }
    for _ in 0..2 {
        bed.state
            .sched
            .channel_key_quality_record(broken_key_id, true)
            .await;
    }
    for _ in 0..8 {
        bed.state
            .sched
            .channel_key_quality_record(broken_key_id, false)
            .await;
    }

    // 2. 验证 Dry-Run：healthy 第一、noisy 降权第二、broken 硬摘进 rejected_candidates
    let (st, sim) = admin_call(
        &bed,
        "POST",
        &format!("/admin/pools/{pool_code}/simulate"),
        Some(json!({ "model": model, "prompt_tokens": 2000 })),
    )
    .await;
    assert_eq!(st, 200, "simulate quality: {sim}");
    let ordered = sim["saved"]["ordered_candidates"].as_array().unwrap();
    assert_eq!(ordered.len(), 2, "broken 应被硬摘: {sim}");
    assert_eq!(ordered[0]["channel_id"], healthy_ch_id);
    assert_eq!(ordered[0]["quality_status"], "healthy");
    assert_eq!(ordered[1]["channel_id"], noisy_ch_id);
    assert_eq!(ordered[1]["quality_status"], "demoted_error_rate");
    assert_eq!(ordered[1]["error_rate_milli"], 300);
    assert!(
        sim["saved"]["rejected_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["channel_id"] == broken_ch_id && r["reason"] == "quality_ejected_error_rate")
    );
}
