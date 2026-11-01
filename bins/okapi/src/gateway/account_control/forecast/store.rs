//! 预判用到的 Redis 状态：烧速计数、响应头观测、θ 状态与样本留存。
//!
//! 预判只是辅助信号：读写失败一律吞掉（留 debug 日志），绝不能让它拖慢或打断推理与结算。
//! 键都是单键操作 / 管道内逐条 GET，Cluster 下不会 CROSSSLOT。

use super::model::{self, Reading, ThetaState};
use crate::gateway::state::AppState;
use fred::interfaces::{KeysInterface, LuaInterface};
use fred::types::{Expiration, SetOptions};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// 预判跟踪的窗口：订阅的 5 小时窗与周窗。
pub(super) const TRACKED_WINDOWS: [i64; 2] = [model::FIVE_HOURS, model::SEVEN_DAYS];
/// 烧速计数的保留期；每次累加顺延，闲置超过它才归零（读侧把「计数回退」当没花钱处理）。
const BURN_TTL_SECS: i64 = 40 * 86_400;
const HEADER_TTL_SECS: i64 = 3_600;
const THETA_TTL_SECS: i64 = 30 * 86_400;
/// 每把 key 最多留这么多条样本（约一周的 5 分钟轮询量），超出从旧的一端丢。
const HISTORY_CAP: i64 = 6_000;
/// 响应头样本的最小间隔：每个响应都写会把留存刷爆，一分钟一条足够做回归。
const HISTORY_GATE_SECS: i64 = 60;

fn burn_key(key: i64) -> String {
    format!("burn:ck:{key}")
}
fn header_key(key: i64) -> String {
    format!("quota:hdr:ck:{key}")
}
fn theta_key(key: i64, window_secs: i64) -> String {
    format!("quota:theta:ck:{key}:{window_secs}")
}
fn history_key(key: i64) -> String {
    format!("quota:hist:ck:{key}")
}

/// 响应头读数的落地形状：用量已折成窗口百分点。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeaderObservation {
    pub at: i64,
    pub burn_micro: i64,
    pub windows: Vec<HeaderWindow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeaderWindow {
    pub window_secs: i64,
    pub used: f64,
    pub resets_at: Option<i64>,
}

/// 一把 key 的预判输入（一次管道读齐）。
#[derive(Clone, Debug, Default)]
pub struct KeyState {
    pub burn_micro: Option<i64>,
    pub header: Option<HeaderObservation>,
    pub theta: BTreeMap<i64, ThetaState>,
    pub plan: Option<String>,
}

/// 结算后累加这把 key 的上游成本（API 等价 micro-USD）。
pub async fn burn_add(state: &AppState, key: i64, micro: i64) {
    if micro <= 0 {
        return;
    }
    if let Err(error) = state
        .sched
        .incr_by_with_ttl(&burn_key(key), micro, BURN_TTL_SECS)
        .await
    {
        tracing::debug!(key, %error, "quota burn counter not updated");
    }
}

/// None = Redis 读失败（基线未知，调用方不得拿 0 顶替）；Some(0) = 这把 key 还没有计数。
pub async fn burn_get(state: &AppState, key: i64) -> Option<i64> {
    match state
        .sched
        .client()
        .get::<Option<i64>, _>(burn_key(key))
        .await
    {
        Ok(count) => Some(count.unwrap_or(0)),
        Err(error) => {
            tracing::debug!(key, %error, "quota burn counter unavailable");
            None
        }
    }
}

/// 一次管道读齐所有候选 key 的预判输入。读失败的 key 缺席（按没有数据处理）。
pub async fn load(state: &AppState, keys: &[i64]) -> HashMap<i64, KeyState> {
    if keys.is_empty() {
        return HashMap::new();
    }
    let pipe = state.sched.client().pipeline();
    for key in keys {
        let queued = async {
            pipe.get::<(), _>(burn_key(*key)).await?;
            pipe.get::<(), _>(header_key(*key)).await?;
            for window in TRACKED_WINDOWS {
                pipe.get::<(), _>(theta_key(*key, window)).await?;
            }
            pipe.get::<(), _>(format!("plan:ck:{key}")).await
        };
        if queued.await.is_err() {
            return HashMap::new();
        }
    }
    let values: Vec<Option<String>> = match pipe.all().await {
        Ok(values) => values,
        Err(error) => {
            tracing::debug!(%error, "quota forecast state unavailable");
            return HashMap::new();
        }
    };
    let per_key = 3 + TRACKED_WINDOWS.len();
    keys.iter()
        .zip(values.chunks(per_key))
        .map(|(key, chunk)| {
            let json = |index: usize| chunk.get(index).and_then(Option::as_deref);
            let theta = TRACKED_WINDOWS
                .iter()
                .enumerate()
                .filter_map(|(offset, window)| {
                    let state: ThetaState = serde_json::from_str(json(2 + offset)?).ok()?;
                    Some((*window, state))
                })
                .collect();
            let plan = json(2 + TRACKED_WINDOWS.len())
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|value| value.get("plan")?.as_str().map(str::to_owned));
            (
                *key,
                KeyState {
                    burn_micro: json(0).and_then(|text| text.parse().ok()),
                    header: json(1).and_then(|text| serde_json::from_str(text).ok()),
                    theta,
                    plan,
                },
            )
        })
        .collect()
}

/// 把一条读数喂给这把 key 这个窗口的 θ 估计。
pub async fn learn(state: &AppState, key: i64, window_secs: i64, reading: Reading) {
    let redis = state.sched.client();
    let name = theta_key(key, window_secs);
    let mut estimate: ThetaState = redis
        .get::<Option<String>, _>(&name)
        .await
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    estimate.observe(&reading);
    if let Ok(text) = serde_json::to_string(&estimate) {
        let saved: Result<(), _> = redis
            .set(
                name,
                text,
                Some(Expiration::EX(THETA_TTL_SECS)),
                None,
                false,
            )
            .await;
        if let Err(error) = saved {
            tracing::debug!(key, window_secs, %error, "quota theta state not saved");
        }
    }
}

/// 追加一条样本到这把 key 的留存（LPUSH + LTRIM，新的在前）。`gated`：响应头样本一分钟最多一条。
pub async fn history_append(state: &AppState, key: i64, sample: &serde_json::Value, gated: bool) {
    // 追加、截断、续期同一个脚本：中途断开不会留下一个没有过期时间的无限增长的列表
    const LUA: &str = r"
        redis.call('LPUSH', KEYS[1], ARGV[1])
        redis.call('LTRIM', KEYS[1], 0, tonumber(ARGV[2]) - 1)
        redis.call('EXPIRE', KEYS[1], ARGV[3])
        return 1
    ";
    let redis = state.sched.client();
    if gated {
        let acquired: Result<Option<String>, _> = redis
            .set(
                format!("quota:histgate:ck:{key}"),
                "1",
                Some(Expiration::EX(HISTORY_GATE_SECS)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        if !matches!(acquired, Ok(Some(_))) {
            return;
        }
    }
    let pushed: Result<i64, _> = redis
        .eval(
            LUA,
            vec![history_key(key)],
            vec![
                sample.to_string(),
                HISTORY_CAP.to_string(),
                THETA_TTL_SECS.to_string(),
            ],
        )
        .await;
    if let Err(error) = pushed {
        tracing::debug!(key, %error, "quota sample not kept");
    }
}

pub async fn header_save(state: &AppState, key: i64, observation: &HeaderObservation) {
    if let Ok(text) = serde_json::to_string(observation) {
        let saved: Result<(), _> = state
            .sched
            .client()
            .set(
                header_key(key),
                text,
                Some(Expiration::EX(HEADER_TTL_SECS)),
                None,
                false,
            )
            .await;
        if let Err(error) = saved {
            tracing::debug!(key, %error, "quota header observation not saved");
        }
    }
}
