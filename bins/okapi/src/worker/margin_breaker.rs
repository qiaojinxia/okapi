//! 负毛利自动熔断评估（IMPLEMENTATION §11.34）：worker 每轮读 `settings.margin_breaker`，
//! 按（分组, 渠道）聚合 CH `mv_analysis_hour` 窗口内**成本已知**的行，亏钱的对写进
//! Redis `mb:blocks`（契约见 `crate::margin`）。到期条目剪掉；管理员 `lifted` 的对跳过。

use crate::margin::{self, BlockEntry, BlockState, BreakerConfig};
use okapi_store::ChClient;
use sqlx::PgPool;
use std::collections::HashMap;

/// 一轮评估的结果。
#[derive(Debug, Default)]
pub struct Report {
    /// 本轮新熔断的对（通知只发这些；续期不重复吵）。
    pub tripped: Vec<Tripped>,
    /// 仍在熔断中（含本轮新增）。
    pub blocked_total: usize,
    /// 到期剪除的字段数。
    pub pruned: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Tripped {
    pub group_code: String,
    pub channel_id: i64,
    pub requests: i64,
    pub amount_micro: i64,
    pub cost_micro: i64,
    pub margin_bp: i64,
}

/// 读配置（PG settings；缺省关）。
pub async fn load_config(pg: &PgPool) -> anyhow::Result<BreakerConfig> {
    let value = sqlx::query_scalar!(r#"SELECT value FROM settings WHERE key = 'margin_breaker'"#)
        .fetch_optional(pg)
        .await?;
    Ok(BreakerConfig::from_setting(value.as_ref()))
}

/// 窗口内按（分组, 渠道）聚合的成本已知样本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairSample {
    pub group_code: String,
    pub channel_id: i64,
    pub requests: i64,
    pub amount_micro: i64,
    pub cost_micro: i64,
}

/// 只拼夹过的整数进 SQL；group_code / channel_id 是聚合键的输出不是输入。
pub async fn sample_pairs(ch: &ChClient, window_hours: i64) -> anyhow::Result<Vec<PairSample>> {
    let hours = window_hours.clamp(1, 168);
    let sql = format!(
        "SELECT group_code, channel_id, \
                countIfMerge(cost_known) AS known, \
                sumMerge(known_amount) AS amount, \
                sumMerge(known_cost) AS cost \
         FROM mv_analysis_hour \
         WHERE hour >= toStartOfHour(now() - INTERVAL {hours} HOUR) AND channel_id > 0 \
         GROUP BY group_code, channel_id \
         HAVING known > 0"
    );
    let rows = ch.query_json_each_row(&sql).await?;
    Ok(rows
        .iter()
        .map(|r| PairSample {
            group_code: r["group_code"].as_str().unwrap_or("").to_owned(),
            channel_id: ch_i64(r, "channel_id"),
            requests: ch_i64(r, "known"),
            amount_micro: ch_i64(r, "amount"),
            cost_micro: ch_i64(r, "cost"),
        })
        .collect())
}

/// CH JSONEachRow 里 64 位整数以字符串下发（防 JS 精度丢失）。
fn ch_i64(row: &serde_json::Value, key: &str) -> i64 {
    match &row[key] {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

/// 一轮评估。`ch = None`（未配 ClickHouse）时只做剪除，不评估。
pub async fn evaluate(
    pg: &PgPool,
    ch: Option<&ChClient>,
    redis: &fred::clients::Client,
    now: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Report> {
    let cfg = load_config(pg).await?;
    let now_s = now.timestamp();
    if !cfg.enabled {
        // 关掉功能 = 立刻放行一切，包括残留的 lifted 记录
        margin::clear_blocks(redis).await?;
        return Ok(Report::default());
    }
    let mut existing = margin::load_blocks(redis)
        .await
        .ok_or_else(|| anyhow::anyhow!("redis hgetall mb:blocks failed"))?;

    // 到期剪除（blocked 到期 = 放行一轮重新采样；lifted 到期 = 恢复评估）
    let expired: Vec<String> = existing
        .iter()
        .filter(|(_, e)| e.until <= now_s)
        .map(|(k, _)| k.clone())
        .collect();
    margin::remove_blocks(redis, &expired).await?;
    for k in &expired {
        existing.remove(k);
    }

    let mut report = Report {
        pruned: expired.len(),
        ..Report::default()
    };
    let Some(ch) = ch else {
        report.blocked_total = count_blocked(&existing, now_s);
        return Ok(report);
    };

    for s in sample_pairs(ch, cfg.window_hours).await? {
        if s.group_code.is_empty() || !cfg.trips(s.requests, s.amount_micro, s.cost_micro) {
            continue;
        }
        let field = margin::field(&s.group_code, s.channel_id);
        let bp = margin::margin_bp(s.amount_micro, s.cost_micro);
        let entry = match existing.get(&field) {
            // 管理员解除期内不碰
            Some(e) if e.state == BlockState::Lifted => continue,
            // 仍在熔断：续期 + 刷新样本，不算新增
            Some(e) => BlockEntry {
                since: e.since,
                until: now_s + cfg.cooldown_secs,
                requests: s.requests,
                amount_micro: s.amount_micro,
                cost_micro: s.cost_micro,
                margin_bp: bp,
                state: BlockState::Blocked,
            },
            None => {
                report.tripped.push(Tripped {
                    group_code: s.group_code.clone(),
                    channel_id: s.channel_id,
                    requests: s.requests,
                    amount_micro: s.amount_micro,
                    cost_micro: s.cost_micro,
                    margin_bp: bp,
                });
                BlockEntry {
                    state: BlockState::Blocked,
                    since: now_s,
                    until: now_s + cfg.cooldown_secs,
                    requests: s.requests,
                    amount_micro: s.amount_micro,
                    cost_micro: s.cost_micro,
                    margin_bp: bp,
                }
            }
        };
        margin::set_block(redis, &field, &entry).await?;
        existing.insert(field, entry);
    }
    report.blocked_total = count_blocked(&existing, now_s);
    Ok(report)
}

fn count_blocked(entries: &HashMap<String, BlockEntry>, now_s: i64) -> usize {
    entries.values().filter(|e| e.blocks_at(now_s)).count()
}
