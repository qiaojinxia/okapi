//! 渠道 key 后台探测与自动恢复（IMPLEMENTATION §11.73）：被摘出轮转的 key 探通就放回，探不通原样不动；
//! `all` 模式下正常的 key 探不通按真实失败登记；人工停用、退出探测、订阅 OAuth、非对话模型、不在池里的渠道都不碰；
//! 同一把 key 在探测间隔内只探一次。
//!
//! 上游是进程内的 axum 替身，按「它收到了哪些模型名」断言谁被探过。依赖 .env（scripts/dev-deps.sh up）。

use axum::Router;
use axum::extract::{Json as ExtractJson, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use okapi_engine as gateway;
use okapi_kernel::policies::{ChannelProbeMode, ChannelProbePolicy};
use okapi_worker::channel_probe::{RoundReport, probe_round};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone, Copy)]
enum Mode {
    Ok,
    Unauthorized,
    ServerError,
    HtmlPage,
}

#[derive(Clone)]
struct Mock {
    mode: Arc<Mutex<Mode>>,
    seen: Arc<Mutex<Vec<String>>>,
}

async fn completions(State(mock): State<Mock>, ExtractJson(body): ExtractJson<Value>) -> Response {
    mock.seen
        .lock()
        .unwrap()
        .push(body["model"].as_str().unwrap_or_default().to_owned());
    match *mock.mode.lock().unwrap() {
        Mode::Ok => ExtractJson(json!({"id": "x", "object": "chat.completion", "choices": []}))
            .into_response(),
        Mode::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            ExtractJson(json!({"error": {"code": "invalid_api_key", "message": "bad key"}})),
        )
            .into_response(),
        Mode::ServerError => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
        Mode::HtmlPage => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html")],
            "<!doctype html><html><body>welcome</body></html>",
        )
            .into_response(),
    }
}

struct Env {
    state: gateway::state::AppState,
    pg: PgPool,
    mock: Mock,
    base: String,
    suffix: String,
}

impl Env {
    async fn start() -> Self {
        okapi_store::test_support::assert_isolated();
        let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
        let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
        let state = gateway::build_state(&database_url, &redis_url, "probe-node", None, None)
            .await
            .unwrap();
        let mock = Mock {
            mode: Arc::new(Mutex::new(Mode::Ok)),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/v1/chat/completions", post(completions))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            pg: state.pg.clone(),
            state,
            mock,
            base,
            suffix: Uuid::new_v4().simple().to_string()[..8].to_owned(),
        }
    }

    fn set_mode(&self, mode: Mode) {
        *self.mock.mode.lock().unwrap() = mode;
    }

    fn seen(&self) -> Vec<String> {
        self.mock.seen.lock().unwrap().clone()
    }

    /// 一个只服务 `name` 这个对话模型的渠道（默认池），返回 (模型名, 渠道 id, key id)。
    async fn channel(&self, name: &str) -> (String, i64, i64) {
        let model = format!("probe-{name}-{}", self.suffix);
        okapi_store::provision::create_model_ratio(&self.pg, &model, "1", "1", "1")
            .await
            .unwrap();
        let (channel, key) = okapi_store::provision::create_channel(
            &self.pg,
            &format!("probe-{name}-{}", self.suffix),
            "openai_compat",
            &self.base,
            "sk-probe",
            &[model.as_str()],
            false,
            None,
        )
        .await
        .unwrap();
        (model, channel, key)
    }

    async fn key_state(&self, key: i64) -> (i16, i32, Option<String>, bool) {
        sqlx::query_as::<_, (i16, i32, Option<String>, bool)>(
            "SELECT status, failed_count, last_error, cooldown_until IS NOT NULL FROM channel_keys WHERE id = $1",
        )
        .bind(key)
        .fetch_one(&self.pg)
        .await
        .unwrap()
    }

    async fn set_key(&self, key: i64, sql: &str) {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE channel_keys SET {sql} WHERE id = {key}"
        )))
        .execute(&self.pg)
        .await
        .unwrap();
    }

    /// 用完把渠道停掉：同一个库里后面的用例不再探到它。
    async fn retire(&self, channels: &[i64]) {
        sqlx::query("UPDATE channels SET status = 2 WHERE id = ANY($1)")
            .bind(channels)
            .execute(&self.pg)
            .await
            .unwrap();
    }

    async fn round(&self, mode: ChannelProbeMode) -> RoundReport {
        let policy = ChannelProbePolicy {
            enabled: true,
            mode,
            batch_size: 200,
            ..ChannelProbePolicy::default()
        };
        probe_round(&self.state, &policy, &mut 0).await.unwrap()
    }
}

#[tokio::test]
async fn keys_taken_out_of_rotation_come_back_when_the_probe_passes() {
    let env = Env::start().await;
    let (m_invalid, c_invalid, k_invalid) = env.channel("invalid").await;
    let (m_cooling, c_cooling, k_cooling) = env.channel("cooling").await;
    let (m_quota, c_quota, k_quota) = env.channel("quota").await;
    let (m_manual, c_manual, k_manual) = env.channel("manual").await;
    env.set_key(
        k_invalid,
        "status = 6, failed_count = 4, last_error = 'upstream_status_401'",
    )
    .await;
    env.set_key(k_cooling, "status = 2, failed_count = 3, last_error = 'upstream_status_500', cooldown_until = now() + interval '1 hour'").await;
    env.set_key(k_quota, "status = 4, failed_count = 2, last_error = 'quota', cooldown_until = now() + interval '5 hours'").await;
    // 控制面的「停用」也写 2，但冷却期是空的——人工决定，探测不碰
    env.set_key(k_manual, "status = 2, failed_count = 0").await;

    let report = env.round(ChannelProbeMode::Recover).await;
    assert_eq!(report.restored, 3, "{report:?}");
    let seen = env.seen();
    for model in [&m_invalid, &m_cooling, &m_quota] {
        assert!(
            seen.contains(model),
            "{model} should have been probed: {seen:?}"
        );
    }
    assert!(
        !seen.contains(&m_manual),
        "a manually stopped key is never probed: {seen:?}"
    );

    assert_eq!(
        env.key_state(k_invalid).await,
        (1, 0, None, false),
        "invalid: counters and error cleared"
    );
    assert_eq!(
        env.key_state(k_cooling).await,
        (1, 3, Some("upstream_status_500".into()), false),
        "cooling keeps its failure count (half-open), like the natural cooldown recovery"
    );
    assert_eq!(
        env.key_state(k_quota).await,
        (1, 0, Some("quota".into()), false)
    );
    assert_eq!(
        env.key_state(k_manual).await,
        (2, 0, None, false),
        "untouched"
    );
    env.retire(&[c_invalid, c_cooling, c_quota, c_manual]).await;
}

#[tokio::test]
async fn a_key_that_still_fails_stays_out_and_a_200_page_is_not_a_pass() {
    let env = Env::start().await;
    let (_, c_401, k_401) = env.channel("still401").await;
    let (_, c_page, k_page) = env.channel("page").await;
    env.set_key(
        k_401,
        "status = 6, failed_count = 4, last_error = 'upstream_status_401'",
    )
    .await;
    env.set_key(
        k_page,
        "status = 4, failed_count = 1, cooldown_until = now() + interval '5 hours'",
    )
    .await;

    env.set_mode(Mode::Unauthorized);
    let report = env.round(ChannelProbeMode::Recover).await;
    assert_eq!(
        (report.probed, report.restored, report.failed),
        (2, 0, 2),
        "{report:?}"
    );
    assert_eq!(env.key_state(k_401).await.0, 6);
    assert_eq!(env.key_state(k_page).await.0, 4);

    // 另一轮要等探测间隔：换一把占位。清掉 Redis 里的占位模拟间隔已过
    env.state.sched.channel_probe_claim(k_401, "x", 1).await;
    env.set_mode(Mode::HtmlPage);
    for key in [k_401, k_page] {
        let _ = redis_release(&env, key).await;
    }
    let report = env.round(ChannelProbeMode::Recover).await;
    assert_eq!(
        report.restored, 0,
        "an HTML page with 200 must not count as a pass: {report:?}"
    );
    assert_eq!(env.key_state(k_page).await.0, 4);
    env.retire(&[c_401, c_page]).await;
}

/// 删掉某把 key 的探测占位，等于「探测间隔已过」。
async fn redis_release(env: &Env, key: i64) -> bool {
    use fred::interfaces::KeysInterface;
    env.state
        .sched
        .client()
        .del::<i64, _>(format!("probe:ck:{key}"))
        .await
        .is_ok_and(|n| n > 0)
}

#[tokio::test]
async fn in_all_mode_a_healthy_key_that_fails_its_probe_is_registered_like_a_real_failure() {
    let env = Env::start().await;
    let (m_healthy, c_healthy, k_healthy) = env.channel("healthy").await;
    env.set_mode(Mode::Unauthorized);

    // recover 模式不碰正常的 key
    let report = env.round(ChannelProbeMode::Recover).await;
    assert_eq!(report.probed, 0, "{report:?}");
    assert!(!env.seen().contains(&m_healthy));
    assert_eq!(env.key_state(k_healthy).await.0, 1);

    let report = env.round(ChannelProbeMode::All).await;
    assert!(report.failed >= 1, "{report:?}");
    assert!(env.seen().contains(&m_healthy));
    let (status, _, last_error, _) = env.key_state(k_healthy).await;
    assert_eq!(
        status, 6,
        "a 401 body that says the key is rejected invalidates it, as a real request would"
    );
    assert!(last_error.is_some());

    // 服务端 5xx 走瞬态：计一次失败，key 仍在轮转（阈值之前）
    let (_, c_flaky, k_flaky) = env.channel("flaky").await;
    env.set_mode(Mode::ServerError);
    let _ = env.round(ChannelProbeMode::All).await;
    let (status, failed, ..) = env.key_state(k_flaky).await;
    assert_eq!(
        (status, failed),
        (1, 1),
        "one transient failure counts but does not cool the key yet"
    );
    env.retire(&[c_healthy, c_flaky]).await;
}

#[tokio::test]
async fn opt_outs_oauth_keys_non_chat_models_and_orphans_are_left_alone() {
    let env = Env::start().await;
    let (m_off, c_off, k_off) = env.channel("optout").await;
    let (m_oauth, c_oauth, k_oauth) = env.channel("oauth").await;
    let (m_embed, c_embed, k_embed) = env.channel("embedding").await;
    let (m_orphan, c_orphan, k_orphan) = env.channel("orphan").await;
    for key in [k_off, k_oauth, k_embed, k_orphan] {
        env.set_key(key, "status = 6").await;
    }
    sqlx::query("UPDATE channels SET settings = settings || '{\"health_probe\":\"off\"}'::jsonb WHERE id = $1")
        .bind(c_off)
        .execute(&env.pg)
        .await
        .unwrap();
    env.set_key(k_oauth, "credential_kind = 1").await;
    sqlx::query("UPDATE models SET catalog_config = '{\"kind\":\"embedding\"}'::jsonb WHERE model_name = $1")
        .bind(&m_embed)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("DELETE FROM pool_channels WHERE channel_id = $1")
        .bind(c_orphan)
        .execute(&env.pg)
        .await
        .unwrap();

    let report = env.round(ChannelProbeMode::Recover).await;
    let seen = env.seen();
    for model in [&m_off, &m_oauth, &m_embed, &m_orphan] {
        assert!(
            !seen.contains(model),
            "{model} must not be probed: {seen:?} {report:?}"
        );
    }
    for key in [k_off, k_oauth, k_embed, k_orphan] {
        assert_eq!(env.key_state(key).await.0, 6);
    }
    env.retire(&[c_off, c_oauth, c_embed, c_orphan]).await;
}

#[tokio::test]
async fn one_key_is_probed_once_per_interval() {
    let env = Env::start().await;
    let (model, channel, key) = env.channel("spacing").await;
    env.set_key(key, "status = 6").await;
    env.set_mode(Mode::Unauthorized);
    let first = env.round(ChannelProbeMode::Recover).await;
    let second = env.round(ChannelProbeMode::Recover).await;
    let hits = env.seen().iter().filter(|m| **m == model).count();
    assert_eq!(
        hits, 1,
        "the second round inside the interval must not probe again ({first:?} / {second:?})"
    );
    assert_eq!(second.probed, 0, "{second:?}");
    env.retire(&[channel]).await;
}
