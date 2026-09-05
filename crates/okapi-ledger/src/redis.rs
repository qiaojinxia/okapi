//! Redis 热账本：四个 Lua 契约的强类型封装。
//!
//! M1 用 EVAL 全量下发脚本（正确优先）；EVALSHA/Script 缓存在 M2 引入。
//! 限速为固定分钟窗计数（GCRA 滑窗 M2），键形状见 docs/database.md §2.1。

use crate::error::LedgerError;
use chrono::{DateTime, Utc};
use fred::clients::Client;
use fred::interfaces::{HashesInterface, LuaInterface};
use fred::types::Value;
use okapi_domain::Money;
use uuid::Uuid;

const RESERVE_LUA: &str = include_str!("lua/reserve.lua");
const COMMIT_LUA: &str = include_str!("lua/commit.lua");
const REFUND_LUA: &str = include_str!("lua/refund.lua");
const REPAIR_LUA: &str = include_str!("lua/repair.lua");

/// 预扣悬置时限：超时未结算的预扣由对账任务懒清理（M2 reconciler）。
const RESERVATION_TTL_MS: i64 = 600_000;

/// key 级限额（<=0 表示不限）。
#[derive(Debug, Clone, Copy, Default)]
pub struct LimitCaps {
    pub rpm: i64,
    pub tpm: i64,
    pub rpd: i64,
    pub concurrency: i64,
}

/// 预扣请求参数（具名字段防相邻 i64 错位）。
#[derive(Debug, Clone, Copy)]
pub struct ReserveRequest {
    pub user_id: i64,
    pub api_key_id: i64,
    pub request_id: Uuid,
    pub est: Money,
    pub caps: LimitCaps,
    pub est_tokens: u64,
}

#[derive(Debug, Clone)]
pub enum ReserveOutcome {
    Reserved { balance_after: Money },
    Insufficient { balance: Money },
    RateLimited { which: String },
}

#[derive(Debug, Clone)]
pub enum CommitOutcome {
    /// refund_delta = 预扣 − 实际（正=退，负=补扣）。
    Committed {
        refund_delta: Money,
        balance_after: Money,
    },
    /// 预扣不存在（重复结算/对账竞争）：调用方不得直接改余额。
    NoReservation,
}

/// Redis 余额账本客户端。
/// `repair` 的结果：修复前后的 avail 与被保留的在途合计。
#[derive(Debug, Clone, Copy)]
pub struct RepairOutcome {
    pub before: Money,
    pub after: Money,
    pub inflight: Money,
}

#[derive(Clone)]
pub struct BalanceLedger {
    client: Client,
}

impl BalanceLedger {
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    fn bal_key(user_id: i64) -> String {
        format!("bal:{{{user_id}}}")
    }

    /// key 级并发槽（限额配置在 api_keys 行 → 键按 key 维度，docs/database.md §2.1）。
    fn conc_key(user_id: i64, api_key_id: i64) -> String {
        format!("conc:{{{user_id}}}:k:{api_key_id}")
    }

    /// 预扣 + 限速/并发准入（单 Lua 原子）。限额四件套均为 key 级
    /// （同一用户多把 key 各自独立计数，互不挤兑）。
    pub async fn reserve(
        &self,
        req: ReserveRequest,
        now: DateTime<Utc>,
    ) -> Result<ReserveOutcome, LedgerError> {
        let ReserveRequest {
            user_id,
            api_key_id,
            request_id,
            est,
            caps,
            est_tokens,
        } = req;
        let minute_bucket = now.timestamp().div_euclid(60);
        let day_bucket = now.format("%Y%m%d").to_string();
        let deadline_ms = now.timestamp_millis().saturating_add(RESERVATION_TTL_MS);

        let keys = vec![
            Self::bal_key(user_id),
            format!("rl:{{{user_id}}}:k:{api_key_id}:rpm:{minute_bucket}"),
            format!("rl:{{{user_id}}}:k:{api_key_id}:tpm:{minute_bucket}"),
            format!("rl:{{{user_id}}}:k:{api_key_id}:rpd:{day_bucket}"),
            Self::conc_key(user_id, api_key_id),
        ];
        let args = vec![
            request_id.to_string(),
            est.as_micros().to_string(),
            deadline_ms.to_string(),
            caps.rpm.to_string(),
            caps.tpm.to_string(),
            caps.rpd.to_string(),
            caps.concurrency.to_string(),
            est_tokens.to_string(),
            api_key_id.to_string(),
        ];

        let reply: Value = self.client.eval(RESERVE_LUA, keys, args).await?;
        let items = as_array(&reply)?;
        match (first_i64(items), second_str(items)) {
            (Some(1), _) => Ok(ReserveOutcome::Reserved {
                balance_after: money_at(items, 1)?,
            }),
            (Some(0), Some("INSUFFICIENT")) => Ok(ReserveOutcome::Insufficient {
                balance: money_at(items, 2)?,
            }),
            (Some(0), Some("RATE_LIMITED")) => Ok(ReserveOutcome::RateLimited {
                which: str_at(items, 2)?.to_owned(),
            }),
            _ => Err(LedgerError::UnexpectedReply("reserve")),
        }
    }

    /// 结算（多退少补）。
    pub async fn commit(
        &self,
        user_id: i64,
        api_key_id: i64,
        request_id: Uuid,
        actual: Money,
    ) -> Result<CommitOutcome, LedgerError> {
        let keys = vec![Self::bal_key(user_id), Self::conc_key(user_id, api_key_id)];
        let args = vec![request_id.to_string(), actual.as_micros().to_string()];
        let reply: Value = self.client.eval(COMMIT_LUA, keys, args).await?;
        let items = as_array(&reply)?;
        match (first_i64(items), second_str(items)) {
            (Some(1), _) => Ok(CommitOutcome::Committed {
                refund_delta: money_at(items, 1)?,
                balance_after: money_at(items, 2)?,
            }),
            (Some(0), Some("NO_RESERVATION")) => Ok(CommitOutcome::NoReservation),
            _ => Err(LedgerError::UnexpectedReply("commit")),
        }
    }

    /// 全额释放（不计费路径）；幂等。返回释放金额与余额。
    pub async fn refund(
        &self,
        user_id: i64,
        api_key_id: i64,
        request_id: Uuid,
    ) -> Result<(Money, Money), LedgerError> {
        let keys = vec![Self::bal_key(user_id), Self::conc_key(user_id, api_key_id)];
        let args = vec![request_id.to_string()];
        let reply: Value = self.client.eval(REFUND_LUA, keys, args).await?;
        let items = as_array(&reply)?;
        if first_i64(items) == Some(1) {
            Ok((money_at(items, 1)?, money_at(items, 2)?))
        } else {
            Err(LedgerError::UnexpectedReply("refund"))
        }
    }

    /// 入账（充值/调整/种子）：热账本侧；PG 事件由调用方另记。
    pub async fn credit(&self, user_id: i64, amount: Money) -> Result<Money, LedgerError> {
        let after: i64 = self
            .client
            .hincrby(Self::bal_key(user_id), "avail", amount.as_micros())
            .await?;
        Ok(Money::from_micros(after))
    }

    /// 余额有效期到期清零：原子取出全部可用余额并返回（在途预扣不动，
    /// 各自按结算/退款路径终结）。avail ≤ 0 时不动返回 0。
    pub async fn drain(&self, user_id: i64) -> Result<Money, LedgerError> {
        const LUA: &str = r"
            local v = tonumber(redis.call('HGET', KEYS[1], 'avail') or '0')
            if v <= 0 then return 0 end
            redis.call('HSET', KEYS[1], 'avail', 0)
            return v
        ";
        let drained: i64 = self
            .client
            .eval(LUA, vec![Self::bal_key(user_id)], Vec::<String>::new())
            .await?;
        Ok(Money::from_micros(drained))
    }

    /// 按账本权威值重建热余额（对账修复）。
    ///
    /// 为什么需要：Redis 是**唯一**热账本，而 `reserve.lua` 对不存在的键按余额 0 处理、
    /// 不回源 PG。实例没开持久化重启一次、故障切到空副本、maxmemory 把 `bal:{}` 淘汰掉、
    /// 或者谁手滑 FLUSHDB——余额就集体归零，全站付费请求静默拒服务
    /// （429 insufficient_quota），且**不会自愈**：对账任务此前只报不修，
    /// 全仓也没有第二个入口能把余额写回去。
    ///
    /// 权威源是 `billing_events` 求和，重建幂等：同一个 target 重跑结果相同。
    /// 在途预扣保持不动，详见 `lua/repair.lua`。
    pub async fn repair(
        &self,
        user_id: i64,
        target: Money,
    ) -> Result<RepairOutcome, LedgerError> {
        let reply: Value = self
            .client
            .eval(
                REPAIR_LUA,
                vec![Self::bal_key(user_id)],
                vec![target.as_micros().to_string()],
            )
            .await?;
        let items = as_array(&reply)?;
        let parse = |i: usize| -> Result<Money, LedgerError> {
            str_at(items, i)?
                .parse::<i64>()
                .map(Money::from_micros)
                .map_err(|_| LedgerError::UnexpectedReply("repair"))
        };
        Ok(RepairOutcome {
            before: parse(0)?,
            after: parse(1)?,
            inflight: parse(2)?,
        })
    }

    /// 读当前热余额（诊断/测试）。
    pub async fn balance(&self, user_id: i64) -> Result<Money, LedgerError> {
        let raw: Option<String> = self.client.hget(Self::bal_key(user_id), "avail").await?;
        let micros = raw
            .map(|s| s.parse::<i64>())
            .transpose()
            .map_err(|_| LedgerError::UnexpectedReply("balance"))?
            .unwrap_or(0);
        Ok(Money::from_micros(micros))
    }

    /// 列出用户全部在途预扣（对账/悬置清理输入）。
    pub async fn list_reservations(&self, user_id: i64) -> Result<Vec<Reservation>, LedgerError> {
        let map: std::collections::HashMap<String, String> =
            self.client.hgetall(Self::bal_key(user_id)).await?;
        let mut out = Vec::new();
        for (field, value) in map {
            let Some(id_str) = field.strip_prefix("r:") else {
                continue;
            };
            let Ok(request_id) = Uuid::parse_str(id_str) else {
                continue;
            };
            // 字段格式 "<amount>|<deadline_ms>|<api_key_id>"（旧格式缺 kid 时取 0）
            let mut parts = value.split('|');
            let amount = parts
                .next()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or(LedgerError::UnexpectedReply("reservation_amount"))?;
            let deadline_ms = parts
                .next()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let api_key_id = parts
                .next()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            out.push(Reservation {
                request_id,
                amount: Money::from_micros(amount),
                deadline_ms,
                api_key_id,
            });
        }
        Ok(out)
    }
}

/// 一笔在途预扣。
#[derive(Debug, Clone, Copy)]
pub struct Reservation {
    pub request_id: Uuid,
    pub amount: Money,
    pub deadline_ms: i64,
    pub api_key_id: i64,
}

// ---- Lua 回复解析（Lua 数字回 RESP integer，tostring 回 bulk string）----

fn as_array(value: &Value) -> Result<&[Value], LedgerError> {
    match value {
        Value::Array(items) => Ok(items),
        _ => Err(LedgerError::UnexpectedReply("not_array")),
    }
}

fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(i) => Some(*i),
        Value::String(s) => s.parse::<i64>().ok(),
        Value::Bytes(b) => std::str::from_utf8(b).ok()?.parse::<i64>().ok(),
        _ => None,
    }
}

fn value_str(value: &Value) -> Option<&str> {
    match value {
        Value::String(s) => Some(s),
        Value::Bytes(b) => std::str::from_utf8(b).ok(),
        _ => None,
    }
}

fn first_i64(items: &[Value]) -> Option<i64> {
    items.first().and_then(value_i64)
}

fn second_str(items: &[Value]) -> Option<&str> {
    items.get(1).and_then(value_str)
}

fn str_at(items: &[Value], idx: usize) -> Result<&str, LedgerError> {
    items
        .get(idx)
        .and_then(value_str)
        .ok_or(LedgerError::UnexpectedReply("missing_str"))
}

fn money_at(items: &[Value], idx: usize) -> Result<Money, LedgerError> {
    items
        .get(idx)
        .and_then(value_i64)
        .map(Money::from_micros)
        .ok_or(LedgerError::UnexpectedReply("missing_int"))
}
