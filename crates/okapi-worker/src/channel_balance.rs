//! 渠道上游余额的定时刷新与低余额处置（IMPLEMENTATION §11.73；对照 new-api 的「更新所有渠道余额」与「余额不足自动禁用」）。
//!
//! 控制台的「查询余额」只在人点的时候查一次，列表上的余额很快过时，渠道用光了也没人知道，直到客户端开始 4xx。
//! 这里定时对有余额接口的渠道（openai / openai_compat，口径同按钮：`channel_balance::Probe`）查一次，
//! 结果写进和按钮同一处留痕（列表的 `last_balance` 随之更新）。渠道 `settings.low_balance` 写了阈值的，
//! 余额小于等于它就发 `channel_balance_low` 通知（转入「低」时一次，仍低则每 24 小时提醒一次）；动作是 `stop` 的同时把渠道停用，
//! 余额回到阈值之上、渠道又仍停着，就自动再启用——只启用**这里**停的：停用的那一刻在 Redis 留了标记（`ch:lowbal:<id>`），
//! 管理员自己停的渠道没有标记，不会被动。
//!
//! 范围：只查渠道里第一把静态 key（同按钮）；渠道 `settings.balance_refresh = "off"` 可单独退出；出口不可用就不查
//! （绝不退回直连）；查不到（上游报错、形状认不出）时留痕和渠道状态都原样不动。默认关，要站长在 `channel_balance_policy` 里打开。
//! 多副本：每个渠道的刷新由 Redis 占位（SET NX EX，TTL = 刷新间隔）选出一个副本。

use crate::notify::Notifier;
use futures::{StreamExt, stream};
use okapi_gateway::channel_balance::{self, Balance, Probe};
use okapi_gateway::state::AppState;
use okapi_kernel::policies::{
    CHANNEL_BALANCE_SETTING, ChannelBalancePolicy, LowBalance, LowBalanceAction,
};
use okapi_store::channels::{self, BalanceTarget};
use serde_json::{Value, json};
use std::time::Duration;

/// 开着时隔多久扫一轮（每个渠道的最小间隔由占位的 TTL 保证，不靠这个）。
const TICK: Duration = Duration::from_secs(60);
/// 关着时隔多久回来看一眼设置。
const IDLE: Duration = Duration::from_secs(60);
/// 一次从库里取多少个候选渠道。
const PAGE: i64 = 100;
/// 有余额接口的协议；具体口径由 `Probe::for_channel` 按主机选。
const BALANCE_PROVIDERS: [&str; 2] = ["openai", "openai_compat"];
/// 渠道状态：启用 / 手动停用。
const ENABLED: i16 = 1;
const STOPPED: i16 = 2;
/// 官方 OpenAI 没有给 API key 用的余额接口（dashboard 口径要浏览器会话），定时去问只会每个间隔白失败一次。
fn has_no_balance_interface(base: &str) -> bool {
    reqwest::Url::parse(base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .is_some_and(|host| host == "api.openai.com")
}

/// 余额一直低时，隔多久再提醒一次。
const REMIND_AFTER: chrono::Duration = chrono::Duration::hours(24);
/// 通知事件名。
pub const LOW_BALANCE_EVENT: &str = "channel_balance_low";

pub async fn run(
    state: AppState,
    notifier: Notifier,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut after = 0;
    loop {
        if *stop.borrow() {
            return;
        }
        let policy = read_policy(&state).await;
        if policy.enabled {
            tokio::select! {
                _ = stop.changed() => return,
                result = refresh_round(&state, &notifier, &policy, &mut after) => match result {
                    Ok(report) if report.refreshed + report.failed > 0 => tracing::info!(
                        refreshed = report.refreshed,
                        failed = report.failed,
                        skipped = report.skipped,
                        became_low = report.became_low,
                        stopped = report.stopped,
                        resumed = report.resumed,
                        "channel balance refresh round"
                    ),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "channel balance refresh round failed"),
                },
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            () = tokio::time::sleep(if policy.enabled { TICK } else { IDLE }) => {}
        }
    }
}

/// 没配 = 默认（关）；配错 = 关并留日志，不让一个写坏的值把刷新悄悄变成别的样子。
async fn read_policy(state: &AppState) -> ChannelBalancePolicy {
    let setting = state.setting_cached(CHANNEL_BALANCE_SETTING).await;
    match setting.as_ref() {
        None => ChannelBalancePolicy::default(),
        Some(value) => ChannelBalancePolicy::parse(value).unwrap_or_else(|| {
            tracing::warn!("invalid channel_balance_policy; channel balance refresh disabled");
            ChannelBalancePolicy::default()
        }),
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RoundReport {
    /// 查到余额的渠道数。
    pub refreshed: usize,
    /// 发了请求但没查成（上游报错、形状认不出）。
    pub failed: usize,
    /// 没发请求（凭证读不出、出口不可用、没有对应探针）。
    pub skipped: usize,
    /// 这一轮刚转入「低」的渠道数（发了通知的）。
    pub became_low: usize,
    pub stopped: usize,
    pub resumed: usize,
}

/// 扫一轮：从游标往后分页取候选，逐个抢占位，抢到的凑满一批后有界并发地查。游标走到头就回到 0。
pub async fn refresh_round(
    state: &AppState,
    notifier: &Notifier,
    policy: &ChannelBalancePolicy,
    after: &mut i64,
) -> Result<RoundReport, okapi_store::StoreError> {
    let lease = i64::try_from(policy.interval_secs).unwrap_or(3600);
    let batch = usize::from(policy.batch_size);
    let mut claimed: Vec<BalanceTarget> = Vec::new();
    'pages: loop {
        let page = channels::balance_targets(&state.pg, &BALANCE_PROVIDERS, *after, PAGE).await?;
        if page.is_empty() {
            *after = 0;
            break;
        }
        for target in page {
            *after = target.channel_id;
            if state
                .sched
                .channel_balance_claim(target.channel_id, state.node.as_ref(), lease)
                .await
            {
                claimed.push(target);
                if claimed.len() >= batch {
                    break 'pages;
                }
            }
        }
    }
    let outcomes: Vec<Outcome> = stream::iter(claimed)
        .map(|target| refresh_one(state, policy, target))
        .buffer_unordered(usize::from(policy.concurrency.max(1)))
        .collect()
        .await;
    let mut report = RoundReport::default();
    let mut announce: Vec<Value> = Vec::new();
    for outcome in outcomes {
        match outcome {
            Outcome::Skipped(reason) => {
                tracing::debug!(reason, "channel balance refresh skipped");
                report.skipped += 1;
            }
            Outcome::Failed => report.failed += 1,
            Outcome::Done(handled) => {
                report.refreshed += 1;
                report.stopped += usize::from(handled.stopped);
                report.resumed += usize::from(handled.resumed);
                if let Some(entry) = handled.announce {
                    report.became_low += 1;
                    announce.push(entry);
                }
            }
        }
    }
    if !announce.is_empty() {
        notifier
            .dispatch(LOW_BALANCE_EVENT, &json!({ "channels": announce }))
            .await;
    }
    Ok(report)
}

#[derive(Debug)]
enum Outcome {
    /// 没发请求（凭证读不出、出口不可用、没有探针）：不是上游的错，什么都不动。
    Skipped(&'static str),
    /// 请求了但没查成：留痕和渠道状态原样不动。
    Failed,
    Done(Handled),
}

#[derive(Debug, Default)]
struct Handled {
    /// 要进通知的一条（转入「低」或到了提醒时间）。
    announce: Option<Value>,
    stopped: bool,
    resumed: bool,
}

async fn refresh_one(
    state: &AppState,
    policy: &ChannelBalancePolicy,
    target: BalanceTarget,
) -> Outcome {
    let channel_id = target.channel_id;
    let base = target
        .api_base
        .as_deref()
        .unwrap_or_default()
        .trim_end_matches('/');
    let Some(probe) = Probe::for_channel(&target.provider, base) else {
        return Outcome::Skipped("balance_unsupported");
    };
    if has_no_balance_interface(base) {
        return Outcome::Skipped("official_openai");
    }
    let credential = match okapi_store::credential::open(
        state.master_key.as_deref(),
        &target.credential_ciphertext,
    ) {
        Ok(credential) => credential,
        Err(error) => {
            tracing::warn!(channel_id, %error, "channel credential unreadable; balance refresh skipped");
            return Outcome::Skipped("credential_unreadable");
        }
    };
    // 查询走的出口和真实流量是同一个；出口不可用就不查——绝不退回直连换一个 IP 去试
    let proxy_url = match okapi_store::egress::resolve_for_key(
        &state.pg,
        target.key_id,
        state.master_key.as_deref(),
    )
    .await
    {
        Ok(Some(resolved)) => match resolved.proxy_url() {
            Ok(proxy_url) => proxy_url,
            Err(_) => return Outcome::Skipped("egress_unavailable"),
        },
        Ok(None) => return Outcome::Skipped("key_gone"),
        Err(error) => {
            tracing::warn!(channel_id, %error, "egress lookup failed; balance refresh skipped");
            return Outcome::Skipped("egress_lookup_failed");
        }
    };
    let outbound = okapi_providers::Outbound::from_settings(&target.settings, proxy_url);
    let balance = match channel_balance::fetch(state, probe, base, &credential, &outbound).await {
        Ok(balance) => balance,
        Err(error) => {
            tracing::warn!(channel_id, ?error, "channel balance could not be read");
            return Outcome::Failed;
        }
    };
    let mut record = channel_balance::record(channel_id, probe, &balance);
    record["auto"] = json!(true);
    state
        .sched
        .channel_balance_record(channel_id, &record)
        .await;
    let rule = LowBalance::from_channel_settings(&target.settings).unwrap_or_else(|_| {
        tracing::warn!(
            channel_id,
            "channel settings.low_balance is invalid; ignored"
        );
        None
    });
    Outcome::Done(settle(state, policy, &target, &balance, rule).await)
}

/// 读到余额之后该做的事（纯判断，不碰 IO）。
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Nothing,
    /// 余额低。`announce`：要进通知（转入低，或到了提醒时间）；`stop`：要把渠道停掉。
    Low {
        announce: bool,
        stop: bool,
    },
    /// 余额回到阈值之上，标记要清掉；`resume`：渠道是这里停的、现在还停着，要重新启用。
    Recovered {
        resume: bool,
    },
}

/// `flag`：渠道此刻有没有「余额低」标记；`held`：标记上记着渠道是这里停的；`remind_due`：标记上的上次通知已经过了提醒间隔。
fn decide(
    rule: &LowBalance,
    action: LowBalanceAction,
    balance_micro: i64,
    flag: bool,
    held: bool,
    remind_due: bool,
    status: i16,
) -> Decision {
    if rule.is_low(balance_micro) {
        Decision::Low {
            announce: !flag || remind_due,
            stop: action == LowBalanceAction::Stop && status == ENABLED,
        }
    } else if flag {
        Decision::Recovered {
            resume: held && status == STOPPED,
        }
    } else {
        Decision::Nothing
    }
}

/// 一次读到的余额，连同处置它所需的上下文。
struct Reading<'a> {
    state: &'a AppState,
    target: &'a BalanceTarget,
    balance: &'a Balance,
    rule: LowBalance,
}

/// 渠道的「余额低」标记（Redis `ch:lowbal:<id>`）读出来的样子。
struct Flag {
    present: bool,
    /// 标记上记着：渠道是这里停的。
    held: bool,
    announced_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Flag {
    async fn load(state: &AppState, channel_id: i64) -> Self {
        let flag = state.sched.channel_low_balance_get(channel_id).await;
        Self {
            present: flag.is_some(),
            held: flag
                .as_ref()
                .and_then(|f| f.get("stopped"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            announced_at: flag
                .as_ref()
                .and_then(|f| f.get("announced_at"))
                .and_then(Value::as_str)
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map(|at| at.with_timezone(&chrono::Utc)),
        }
    }

    fn remind_due(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.announced_at.is_none_or(|at| now - at >= REMIND_AFTER)
    }
}

async fn settle(
    state: &AppState,
    policy: &ChannelBalancePolicy,
    target: &BalanceTarget,
    balance: &Balance,
    rule: Option<LowBalance>,
) -> Handled {
    let Some(rule) = rule else {
        return Handled::default();
    };
    let reading = Reading {
        state,
        target,
        balance,
        rule,
    };
    let flag = Flag::load(state, target.channel_id).await;
    let now = chrono::Utc::now();
    let action = rule.action_under(policy);
    match decide(
        &rule,
        action,
        balance.remaining_micro,
        flag.present,
        flag.held,
        flag.remind_due(now),
        target.status,
    ) {
        Decision::Nothing => Handled::default(),
        Decision::Low { announce, stop } => reading.on_low(action, &flag, announce, stop).await,
        Decision::Recovered { resume } => reading.on_recovered(resume).await,
    }
}

impl Reading<'_> {
    async fn on_low(
        &self,
        action: LowBalanceAction,
        flag: &Flag,
        announce: bool,
        stop: bool,
    ) -> Handled {
        let channel_id = self.target.channel_id;
        let stopped_now = stop && self.stop_channel().await;
        // 标记记着「渠道现在是不是被这里停着」：这一轮停的，或者更早停的、现在仍停着
        let holding = stopped_now || (flag.held && self.target.status == STOPPED);
        let now = chrono::Utc::now();
        let announced_at = if announce {
            now
        } else {
            flag.announced_at.unwrap_or(now)
        };
        self.state
            .sched
            .channel_low_balance_set(
                channel_id,
                &json!({
                    "at": now.to_rfc3339(),
                    "announced_at": announced_at.to_rfc3339(),
                    "balance_micro": self.balance.remaining_micro,
                    "threshold_micro": self.rule.threshold_micro,
                    "currency": self.balance.currency,
                    "stopped": holding,
                }),
            )
            .await;
        Handled {
            announce: announce.then(|| {
                json!({
                    "channel_id": channel_id,
                    "name": self.target.name,
                    "currency": self.balance.currency,
                    "balance_micro": self.balance.remaining_micro,
                    "threshold_micro": self.rule.threshold_micro,
                    "action": match action {
                        LowBalanceAction::Notify => "notify",
                        LowBalanceAction::Stop => "stop",
                    },
                    "stopped": holding,
                })
            }),
            stopped: stopped_now,
            resumed: false,
        }
    }

    async fn on_recovered(&self, resume: bool) -> Handled {
        let channel_id = self.target.channel_id;
        let resumed = if resume {
            match self.resume_channel().await {
                Ok(resumed) => resumed,
                // 没启用成功就不清标记，下一轮接着试
                Err(()) => return Handled::default(),
            }
        } else {
            false
        };
        self.state.sched.channel_low_balance_clear(channel_id).await;
        Handled {
            announce: None,
            stopped: false,
            resumed,
        }
    }

    /// 把渠道停掉（只在它此刻仍是启用时）。返回是不是真停了。
    async fn stop_channel(&self) -> bool {
        let channel_id = self.target.channel_id;
        match channels::transition_channel_status(&self.state.pg, channel_id, ENABLED, STOPPED)
            .await
        {
            Ok(true) => {
                self.state.invalidate_routing_caches();
                self.audit("channel.low_balance_stop").await;
                tracing::warn!(
                    channel_id,
                    balance_micro = self.balance.remaining_micro,
                    threshold_micro = self.rule.threshold_micro,
                    "channel stopped: upstream balance is low"
                );
                true
            }
            // 读状态到写状态之间管理员已经动过它：人的决定优先，这一轮不再动
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(channel_id, %error, "channel could not be stopped for a low balance");
                false
            }
        }
    }

    /// 把渠道重新启用（只在它此刻仍是停用时）。`Ok(false)` = 管理员已经自己动过它，不是我们要做的事了。
    async fn resume_channel(&self) -> Result<bool, ()> {
        let channel_id = self.target.channel_id;
        match channels::transition_channel_status(&self.state.pg, channel_id, STOPPED, ENABLED)
            .await
        {
            Ok(true) => {
                self.state.invalidate_routing_caches();
                self.audit("channel.low_balance_resume").await;
                tracing::info!(
                    channel_id,
                    balance_micro = self.balance.remaining_micro,
                    "channel re-enabled: upstream balance is back above its threshold"
                );
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) => {
                tracing::warn!(channel_id, %error, "channel could not be re-enabled");
                Err(())
            }
        }
    }

    async fn audit(&self, action: &str) {
        let detail = json!({
            "balance_micro": self.balance.remaining_micro,
            "threshold_micro": self.rule.threshold_micro,
            "currency": self.balance.currency,
            "name": self.target.name,
        });
        if let Err(error) = okapi_store::admin::record_audit(
            &self.state.pg,
            "worker",
            action,
            &self.target.channel_id.to_string(),
            detail,
        )
        .await
        {
            tracing::warn!(
                channel_id = self.target.channel_id,
                %error,
                "balance action audit could not be persisted"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(threshold_micro: i64) -> LowBalance {
        LowBalance {
            threshold_micro,
            action: None,
        }
    }

    #[test]
    fn official_openai_is_not_asked_for_a_balance() {
        assert!(has_no_balance_interface("https://api.openai.com/v1"));
        assert!(has_no_balance_interface("https://API.OpenAI.com/v1/"));
        assert!(!has_no_balance_interface("https://relay.example.com/v1"));
        assert!(!has_no_balance_interface(
            "https://api.openai.com.relay.example.com/v1"
        ));
        assert!(!has_no_balance_interface("not a url"));
    }

    #[test]
    fn a_healthy_channel_without_a_flag_is_left_alone() {
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                101,
                false,
                false,
                false,
                ENABLED
            ),
            Decision::Nothing
        );
    }

    #[test]
    fn crossing_the_threshold_announces_once_and_stops_only_when_asked_to() {
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Notify,
                100,
                false,
                false,
                false,
                ENABLED
            ),
            Decision::Low {
                announce: true,
                stop: false
            },
            "equal to the threshold is low; notify-only never stops"
        );
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                50,
                false,
                false,
                false,
                ENABLED
            ),
            Decision::Low {
                announce: true,
                stop: true
            }
        );
    }

    #[test]
    fn staying_low_is_quiet_until_the_reminder_is_due() {
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Notify,
                10,
                true,
                false,
                false,
                ENABLED
            ),
            Decision::Low {
                announce: false,
                stop: false
            }
        );
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Notify,
                10,
                true,
                false,
                true,
                ENABLED
            ),
            Decision::Low {
                announce: true,
                stop: false
            }
        );
    }

    #[test]
    fn an_already_stopped_channel_is_not_stopped_again() {
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                10,
                true,
                true,
                false,
                STOPPED
            ),
            Decision::Low {
                announce: false,
                stop: false
            }
        );
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                10,
                true,
                false,
                false,
                STOPPED
            ),
            Decision::Low {
                announce: false,
                stop: false
            },
            "a manual stop is the operator's decision, stopping it again changes nothing"
        );
    }

    #[test]
    fn an_operator_who_re_enables_a_still_low_channel_gets_it_stopped_again_under_stop() {
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                10,
                true,
                true,
                false,
                ENABLED
            ),
            Decision::Low {
                announce: false,
                stop: true
            }
        );
    }

    #[test]
    fn only_a_channel_this_job_stopped_is_resumed() {
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                500,
                true,
                true,
                false,
                STOPPED
            ),
            Decision::Recovered { resume: true }
        );
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                500,
                true,
                false,
                false,
                STOPPED
            ),
            Decision::Recovered { resume: false },
            "stopped by an operator while flagged: the flag goes, the status stays"
        );
        assert_eq!(
            decide(
                &rule(100),
                LowBalanceAction::Stop,
                500,
                true,
                true,
                false,
                ENABLED
            ),
            Decision::Recovered { resume: false },
            "already enabled again by an operator"
        );
    }
}
