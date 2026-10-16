//! 渠道账号测试输出：钉住指定渠道 / 账号，经真实数据面路径流式返回，按管理员令牌计费。
use super::*;

async fn try_channel(env: &Env, channel: i64, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "http://{}/admin/channels/{channel}/try",
            env.console
        ))
        .bearer_auth(&env.admin_token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn admin_id(env: &Env) -> i64 {
    sqlx::query_scalar("SELECT user_id FROM api_keys WHERE key_hash = $1")
        .bind(hash(&env.admin_token))
        .fetch_one(&env.pg)
        .await
        .unwrap()
}

#[tokio::test]
async fn account_test_streams_output_from_the_pinned_channel_and_bills_the_admin() {
    let env = setup().await;
    env.mock_state.cli_mode.store(true, Ordering::SeqCst);
    // 两条同模型订阅渠道，第二条优先级更高：钉住第一条时调度不能换走
    let (tested, tested_key) = login_channel(&env, "anthropic_max").await;
    let (preferred, _) = login_channel(&env, "anthropic_max").await;
    sqlx::query("UPDATE channels SET priority = 100 WHERE id = $1")
        .bind(preferred)
        .execute(&env.pg)
        .await
        .unwrap();
    // 仅限真机的渠道也能由管理员点名测试
    sqlx::query("UPDATE channels SET settings = settings || $2 WHERE id = $1")
        .bind(tested)
        .bind(json!({"extensions":{"client_profile":{"name":"claude-code","mode":"auto","native_only":true}}}))
        .execute(&env.pg)
        .await
        .unwrap();
    env.state.invalidate_routing_caches();
    let admin = admin_id(&env).await;
    env.state
        .ledger
        .credit(admin, Money::from_micros(10_000_000))
        .await
        .unwrap();

    let resp = try_channel(
        &env,
        tested,
        json!({"model": env.model, "prompt": "say hi", "key_id": tested_key}),
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let text = resp.text().await.unwrap();
    assert!(text.contains("Hello max"), "{text}");
    let seen = env.mock_state.last_body().unwrap();
    assert_eq!(seen["max_tokens"], 512);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let billed: Option<i64> = sqlx::query_scalar(
                "SELECT channel_id FROM billing_records WHERE user_id = $1 AND status = 20 AND log_type = 2",
            )
            .bind(admin)
            .fetch_optional(&env.pg)
            .await
            .unwrap();
            if let Some(channel) = billed {
                assert_eq!(channel, tested, "测试必须打在钉住的渠道上");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();

    // 渠道没声明的模型、别的渠道的账号：直接 400，不发请求
    let resp = try_channel(&env, tested, json!({"model": "not-declared"})).await;
    assert_eq!(resp.status(), 400);
    let resp = try_channel(
        &env,
        preferred,
        json!({"model": env.model, "key_id": tested_key}),
    )
    .await;
    assert_eq!(resp.status(), 400);
    // 普通用户没有渠道写权限
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{}/admin/channels/{tested}/try",
            env.console
        ))
        .bearer_auth(&env.user_token)
        .json(&json!({"model": env.model}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn codex_account_test_uses_the_responses_entry() {
    let env = setup().await;
    let (channel, _) = login_channel(&env, "codex").await;
    let admin = admin_id(&env).await;
    env.state
        .ledger
        .credit(admin, Money::from_micros(10_000_000))
        .await
        .unwrap();
    let resp = try_channel(&env, channel, json!({"model": env.model})).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("response.output_text.delta"), "{text}");
    assert_eq!(env.mock_state.codex_calls.load(Ordering::SeqCst), 1);
}
