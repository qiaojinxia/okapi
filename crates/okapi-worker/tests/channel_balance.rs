//! 渠道上游余额的定时刷新与低余额处置（IMPLEMENTATION §11.73）：余额按间隔查一次并回填列表；`settings.low_balance`
//! 写了阈值的渠道，转入「低」时通知一次（仍低则隔 24 小时提醒），动作为 stop 时同时停用，余额回来再启用——只启用这里停的；
//! 查不到的原样不动；退出刷新 / 没有余额接口 / 已删除的渠道不查。
//!
//! 上游是进程内的 axum 替身（new-api 的 dashboard 口径），按渠道名分路径；通知落在进程内的 webhook 接收端。
//! 用例共用一个库里的 `notify_channels` 等全局键，所以串行跑（`SERIAL`）。依赖 .env（scripts/dev-deps.sh up）。

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use okapi_gateway as gateway;
use okapi_kernel::policies::{ChannelBalancePolicy, LowBalanceAction};
use okapi_store::provision::{ChannelCreate, create_channel_configured};
use okapi_worker::channel_balance::{RoundReport, refresh_round};
use okapi_worker::notify::Notifier;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Default)]
struct Mock {
    /// 渠道名（路径段）→ 余额（micro，美元）。
    balances: Arc<Mutex<HashMap<String, i64>>>,
    failing: Arc<Mutex<HashSet<String>>>,
    /// 被问过余额的渠道名，按次序。
    calls: Arc<Mutex<Vec<String>>>,
}

async fn subscription(State(mock): State<Mock>, Path(name): Path<String>) -> Response {
    mock.calls.lock().unwrap().push(name.clone());
    if mock.failing.lock().unwrap().contains(&name) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let micro = mock
        .balances
        .lock()
        .unwrap()
        .get(&name)
        .copied()
        .unwrap_or_default();
    // 从整数微元直接拼出 JSON 数字的字面量（最多 6 位小数），不经浮点
    let body = format!(
        r#"{{"hard_limit_usd": {}.{:06}}}"#,
        micro / 1_000_000,
        micro % 1_000_000
    );
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn usage() -> Json<Value> {
    Json(json!({ "total_usage": 0 }))
}

struct Chan {
    id: i64,
    name: String,
}

struct Env {
    state: gateway::state::AppState,
    redis: fred::clients::Client,
    notifier: Notifier,
    pg: PgPool,
    mock: Mock,
    base: String,
    hooks: Arc<Mutex<Vec<Value>>>,
    suffix: String,
    channels: Mutex<Vec<i64>>,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Env {
    async fn start() -> Self {
        let serial = SERIAL.lock().await;
        okapi_store::test_support::assert_isolated();
        let database_url = std::env::var("DATABASE_URL").expect("需要 DATABASE_URL");
        let redis_url = std::env::var("OKAPI_REDIS_URL").expect("需要 OKAPI_REDIS_URL");
        let state = gateway::build_state(&database_url, &redis_url, "balance-node", None, None)
            .await
            .unwrap();
        let redis = okapi_store::connect_redis(&redis_url).await.unwrap();
        let notifier = Notifier::new(state.pg.clone(), redis.clone());

        let mock = Mock::default();
        let upstream = Router::new()
            .route(
                "/{name}/v1/dashboard/billing/subscription",
                get(subscription),
            )
            .route("/{name}/v1/dashboard/billing/usage", get(usage))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let hooks = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&hooks);
        let receiver = Router::new().route(
            "/hook",
            post(move |Json(body): Json<Value>| {
                let sink = Arc::clone(&sink);
                async move {
                    sink.lock().unwrap().push(body);
                    Json(json!({"ok": true}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hook_url = format!("http://{}/hook", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, receiver).await.unwrap();
        });
        for (key, value) in [
            (
                "notify_channels",
                json!([{"type": "webhook", "url": hook_url,
                        "events": ["channel_balance_low"], "min_interval_secs": 1}]),
            ),
            (
                "ssrf_policy",
                json!({"allow_http": true, "allow_private": true}),
            ),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES ($1, $2) \
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
            )
            .bind(key)
            .bind(value)
            .execute(&state.pg)
            .await
            .unwrap();
        }
        let env = Self {
            pg: state.pg.clone(),
            state,
            redis,
            notifier,
            mock,
            base,
            hooks,
            suffix: Uuid::new_v4().simple().to_string()[..8].to_owned(),
            channels: Mutex::new(Vec::new()),
            _serial: serial,
        };
        env.clear_mute().await;
        env
    }

    /// 一个 openai_compat 渠道，上游余额 `balance_micro`；`settings` 里可带 `low_balance` / `balance_refresh`。
    async fn channel(&self, tag: &str, balance_micro: i64, settings: Option<Value>) -> Chan {
        self.channel_of("openai_compat", tag, balance_micro, settings)
            .await
    }

    async fn channel_of(
        &self,
        provider: &str,
        tag: &str,
        balance_micro: i64,
        settings: Option<Value>,
    ) -> Chan {
        let name = format!("bal-{tag}-{}", self.suffix);
        self.mock
            .balances
            .lock()
            .unwrap()
            .insert(name.clone(), balance_micro);
        let api_base = format!("{}/{name}/v1", self.base);
        let model = format!("model-{name}");
        let (id, _key) = create_channel_configured(
            &self.pg,
            ChannelCreate {
                name: &name,
                provider,
                api_base: &api_base,
                credential: "sk-balance",
                models: &[model.as_str()],
                trust_upstream_usage: false,
                owner_id: None,
                settings: settings.as_ref(),
                priority: 0,
                max_concurrency: None,
                cost_milli: None,
                pools: None,
                egress: None,
                egress_preassigned: None,
            },
            None,
        )
        .await
        .unwrap();
        self.channels.lock().unwrap().push(id);
        Chan { id, name }
    }

    fn set_balance(&self, chan: &Chan, micro: i64) {
        self.mock
            .balances
            .lock()
            .unwrap()
            .insert(chan.name.clone(), micro);
    }

    fn calls_for(&self, chan: &Chan) -> usize {
        self.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|name| **name == chan.name)
            .count()
    }

    fn hooks(&self) -> Vec<Value> {
        self.hooks.lock().unwrap().clone()
    }

    async fn status(&self, chan: &Chan) -> i16 {
        sqlx::query_scalar("SELECT status FROM channels WHERE id = $1")
            .bind(chan.id)
            .fetch_one(&self.pg)
            .await
            .unwrap()
    }

    async fn set_status(&self, chan: &Chan, status: i16) {
        sqlx::query("UPDATE channels SET status = $2 WHERE id = $1")
            .bind(chan.id)
            .bind(status)
            .execute(&self.pg)
            .await
            .unwrap();
    }

    async fn audits(&self, action: &str, chan: &Chan) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE action = $1 AND target = $2")
            .bind(action)
            .bind(chan.id.to_string())
            .fetch_one(&self.pg)
            .await
            .unwrap()
    }

    /// 让同一个渠道马上能再被查一次（占位的 TTL 是刷新间隔，测试里不等它）。
    async fn release(&self, chan: &Chan) {
        let _: Option<i64> =
            fred::interfaces::KeysInterface::del(&self.redis, format!("balance:ch:{}", chan.id))
                .await
                .unwrap();
    }

    async fn clear_mute(&self) {
        let _: Option<i64> =
            fred::interfaces::KeysInterface::del(&self.redis, "notify:mute:0:channel_balance_low")
                .await
                .unwrap();
    }

    async fn round(&self, policy: &ChannelBalancePolicy) -> RoundReport {
        self.clear_mute().await;
        refresh_round(&self.state, &self.notifier, policy, &mut 0)
            .await
            .unwrap()
    }

    async fn flag(&self, chan: &Chan) -> Option<Value> {
        self.state.sched.channel_low_balance_get(chan.id).await
    }

    /// 用完把渠道软删除、清掉标记与全局键：同一个库里后面的用例不再碰到它们。
    async fn finish(self) {
        let ids = self.channels.lock().unwrap().clone();
        sqlx::query("UPDATE channels SET deleted_at = now() WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&self.pg)
            .await
            .unwrap();
        for id in ids {
            self.state.sched.channel_low_balance_clear(id).await;
        }
        sqlx::query("DELETE FROM settings WHERE key IN ('notify_channels', 'ssrf_policy')")
            .execute(&self.pg)
            .await
            .unwrap();
    }
}

fn policy() -> ChannelBalancePolicy {
    ChannelBalancePolicy {
        enabled: true,
        batch_size: 200,
        concurrency: 4,
        ..ChannelBalancePolicy::default()
    }
}

fn stop_by_default() -> ChannelBalancePolicy {
    ChannelBalancePolicy {
        low_action: LowBalanceAction::Stop,
        ..policy()
    }
}

#[tokio::test]
async fn the_balance_is_read_on_a_schedule_and_filled_into_the_channel_list() {
    let env = Env::start().await;
    let chan = env.channel("plain", 12_500_000, None).await;

    let report = env.round(&policy()).await;
    assert_eq!(report.refreshed, 1, "{report:?}");
    let record = env
        .state
        .sched
        .channel_balance_get_many(&[chan.id])
        .await
        .remove(&chan.id)
        .expect("the list reads the same note the button writes");
    assert_eq!(record["balance_micro"], 12_500_000);
    assert_eq!(record["currency"], "USD");
    assert_eq!(record["probe"], "openai_dashboard");
    assert_eq!(record["auto"], true, "marked as a scheduled reading");

    let again = env.round(&policy()).await;
    assert_eq!(
        again.refreshed, 0,
        "the same channel is not asked again within the interval"
    );
    assert_eq!(env.calls_for(&chan), 1);
    assert_eq!(
        env.status(&chan).await,
        1,
        "no low_balance rule: nothing else happens"
    );
    assert!(
        env.hooks().is_empty(),
        "no low_balance rule: no notification"
    );
    env.finish().await;
}

#[tokio::test]
async fn a_low_balance_is_announced_once_and_a_notify_only_channel_keeps_running() {
    let env = Env::start().await;
    // 全局缺省是 stop，这个渠道自己写了 notify：渠道自己的优先
    let chan = env
        .channel(
            "notify",
            3_000_000,
            Some(json!({"low_balance": {"threshold_micro": 5_000_000, "action": "notify"}})),
        )
        .await;

    let report = env.round(&stop_by_default()).await;
    assert_eq!((report.became_low, report.stopped), (1, 0), "{report:?}");
    assert_eq!(env.status(&chan).await, 1);
    let hooks = env.hooks();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0]["event"], "channel_balance_low");
    let entry = &hooks[0]["payload"]["channels"][0];
    assert_eq!(entry["channel_id"], chan.id);
    assert_eq!(entry["name"], chan.name);
    assert_eq!(entry["balance_micro"], 3_000_000);
    assert_eq!(entry["threshold_micro"], 5_000_000);
    assert_eq!(entry["action"], "notify");
    assert_eq!(entry["stopped"], false);

    // 仍然低：这个间隔里不再吵
    env.release(&chan).await;
    let report = env.round(&stop_by_default()).await;
    assert_eq!(report.became_low, 0, "{report:?}");
    assert_eq!(env.hooks().len(), 1);

    // 一天之后提醒一次
    let mut flag = env.flag(&chan).await.expect("the low flag is kept");
    flag["announced_at"] = json!((chrono::Utc::now() - chrono::Duration::hours(25)).to_rfc3339());
    env.state
        .sched
        .channel_low_balance_set(chan.id, &flag)
        .await;
    env.release(&chan).await;
    let report = env.round(&stop_by_default()).await;
    assert_eq!(report.became_low, 1, "{report:?}");
    assert_eq!(env.hooks().len(), 2);
    env.finish().await;
}

#[tokio::test]
async fn stop_takes_the_channel_out_and_a_top_up_puts_it_back() {
    let env = Env::start().await;
    // 渠道没写动作，用全局缺省 stop
    let chan = env
        .channel(
            "stop",
            3_000_000,
            Some(json!({"low_balance": {"threshold_micro": 5_000_000}})),
        )
        .await;

    let report = env.round(&stop_by_default()).await;
    assert_eq!((report.became_low, report.stopped), (1, 1), "{report:?}");
    assert_eq!(env.status(&chan).await, 2, "stopped");
    assert_eq!(env.audits("channel.low_balance_stop", &chan).await, 1);
    let hooks = env.hooks();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0]["payload"]["channels"][0]["stopped"], true);
    assert_eq!(
        env.flag(&chan).await.expect("flag")["stopped"],
        true,
        "the flag remembers that this job holds the channel stopped"
    );

    // 仍然低：什么都不再发生（停着的渠道照样被刷新）
    env.release(&chan).await;
    let report = env.round(&stop_by_default()).await;
    assert_eq!(
        (
            report.refreshed,
            report.became_low,
            report.stopped,
            report.resumed
        ),
        (1, 0, 0, 0),
        "{report:?}"
    );
    assert_eq!(env.status(&chan).await, 2);
    assert_eq!(env.audits("channel.low_balance_stop", &chan).await, 1);

    // 充值之后：自动启用，标记清掉；启用不发通知
    env.set_balance(&chan, 20_000_000);
    env.release(&chan).await;
    let report = env.round(&stop_by_default()).await;
    assert_eq!(report.resumed, 1, "{report:?}");
    assert_eq!(env.status(&chan).await, 1, "back in rotation");
    assert_eq!(env.audits("channel.low_balance_resume", &chan).await, 1);
    assert!(env.flag(&chan).await.is_none());
    assert_eq!(env.hooks().len(), 1, "resuming is silent");
    env.finish().await;
}

#[tokio::test]
async fn a_channel_an_operator_stopped_is_never_switched_back_on() {
    let env = Env::start().await;
    let rule = json!({"low_balance": {"threshold_micro": 5_000_000, "action": "stop"}});
    // 没有标记：管理员停的，余额再多也不动
    let manual = env.channel("manual", 50_000_000, Some(rule.clone())).await;
    env.set_status(&manual, 2).await;
    let report = env.round(&policy()).await;
    assert_eq!((report.refreshed, report.resumed), (1, 0), "{report:?}");
    assert_eq!(env.status(&manual).await, 2);

    // 有标记但记着不是我们停的（低的时候管理员自己停了）：标记清掉，状态不动
    env.state
        .sched
        .channel_low_balance_set(
            manual.id,
            &json!({"announced_at": chrono::Utc::now().to_rfc3339(), "stopped": false}),
        )
        .await;
    env.release(&manual).await;
    let report = env.round(&policy()).await;
    assert_eq!(report.resumed, 0, "{report:?}");
    assert_eq!(env.status(&manual).await, 2);
    assert!(
        env.flag(&manual).await.is_none(),
        "the stale flag is cleared"
    );
    env.finish().await;
}

#[tokio::test]
async fn an_unreadable_balance_changes_nothing() {
    let env = Env::start().await;
    let chan = env
        .channel(
            "flaky",
            50_000_000,
            Some(json!({"low_balance": {"threshold_micro": 5_000_000, "action": "stop"}})),
        )
        .await;
    env.round(&policy()).await;
    env.mock.failing.lock().unwrap().insert(chan.name.clone());
    env.set_balance(&chan, 0);

    env.release(&chan).await;
    let report = env.round(&policy()).await;
    assert_eq!((report.refreshed, report.failed), (0, 1), "{report:?}");
    let record = env
        .state
        .sched
        .channel_balance_get_many(&[chan.id])
        .await
        .remove(&chan.id)
        .unwrap();
    assert_eq!(
        record["balance_micro"], 50_000_000,
        "the last good reading stays"
    );
    assert_eq!(
        env.status(&chan).await,
        1,
        "an unreadable balance never stops a channel"
    );
    assert!(env.flag(&chan).await.is_none());
    assert!(env.hooks().is_empty(), "nothing was announced");
    env.finish().await;
}

#[tokio::test]
async fn a_broken_low_balance_rule_is_ignored_instead_of_acted_on() {
    let env = Env::start().await;
    let chan = env
        .channel(
            "broken",
            0,
            Some(json!({"low_balance": {"threshold_micro": -1, "action": "stop"}})),
        )
        .await;
    let report = env.round(&stop_by_default()).await;
    assert_eq!(
        (report.refreshed, report.became_low, report.stopped),
        (1, 0, 0),
        "{report:?}"
    );
    assert_eq!(env.status(&chan).await, 1);
    env.finish().await;
}

/// 出口不可用（绑了一个没有成员的轮换组）的渠道不查，绝不退回直连。
#[tokio::test]
async fn a_channel_whose_egress_is_unavailable_is_not_queried_directly() {
    let env = Env::start().await;
    let chan = env.channel("noegress", 10_000_000, None).await;
    let group = format!("ng-{}", env.suffix);
    sqlx::query("INSERT INTO proxy_groups (code, name, mode) VALUES ($1, 'no members', 'rotate')")
        .bind(&group)
        .execute(&env.pg)
        .await
        .unwrap();
    sqlx::query("UPDATE channels SET egress_mode = 'group', egress_group_code = $2 WHERE id = $1")
        .bind(chan.id)
        .bind(&group)
        .execute(&env.pg)
        .await
        .unwrap();
    let report = env.round(&policy()).await;
    assert_eq!((report.refreshed, report.skipped), (0, 1), "{report:?}");
    assert_eq!(
        env.calls_for(&chan),
        0,
        "no direct connection as a fallback"
    );
    env.finish().await;
}

/// 停用 / 启用都是条件更新：读状态到写状态之间管理员已经动过渠道，就让位给人，什么也不改。
#[tokio::test]
async fn a_status_change_only_applies_when_the_channel_is_still_in_the_state_that_was_read() {
    let env = Env::start().await;
    let chan = env.channel("race", 10_000_000, None).await;
    env.set_status(&chan, 2).await;
    let stopped_again = okapi_store::channels::transition_channel_status(&env.pg, chan.id, 1, 2)
        .await
        .unwrap();
    assert!(!stopped_again, "it was no longer enabled");
    let enabled = okapi_store::channels::transition_channel_status(&env.pg, chan.id, 2, 1)
        .await
        .unwrap();
    assert!(enabled);
    assert_eq!(env.status(&chan).await, 1);
    let enabled_twice = okapi_store::channels::transition_channel_status(&env.pg, chan.id, 2, 1)
        .await
        .unwrap();
    assert!(!enabled_twice, "it was no longer stopped");
    env.finish().await;
}

#[tokio::test]
async fn only_channels_with_a_balance_interface_and_no_opt_out_are_queried() {
    let env = Env::start().await;
    let queried = env.channel("queried", 10_000_000, None).await;
    let opted_out = env
        .channel(
            "optout",
            10_000_000,
            Some(json!({"balance_refresh": "off"})),
        )
        .await;
    let anthropic = env
        .channel_of("anthropic", "claude", 10_000_000, None)
        .await;
    let deleted = env.channel("deleted", 10_000_000, None).await;
    sqlx::query("UPDATE channels SET deleted_at = now() WHERE id = $1")
        .bind(deleted.id)
        .execute(&env.pg)
        .await
        .unwrap();

    let report = env.round(&policy()).await;
    assert_eq!(report.refreshed, 1, "{report:?}");
    assert_eq!(env.calls_for(&queried), 1);
    for other in [&opted_out, &anthropic, &deleted] {
        assert_eq!(
            env.calls_for(other),
            0,
            "{} must not be queried",
            other.name
        );
    }
    env.finish().await;
}
