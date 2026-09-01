//! 调度：priority 降序分层 + 层内排列，返回 failover 尝试序。
//!
//! 层内怎么排由池的 routing_strategy 决定（docs/database.md §3.7）：
//! - `priority_weighted`（默认）：成本修正后加权随机。有效权重 = weight × 1000 / cost_milli。
//! - `least_latency`：按时延 EWMA 升序，无样本者按本层中位数插队。
//!
//! priority 分层在两种策略下都严格生效——层是运维显式表达的"先用谁"，
//! 不该被时延或权重推翻。

use okapi_store::ChannelCandidate;
use rand::RngExt;
use std::collections::HashMap;

/// 池的选路策略。字符串来自库（CHECK 约束保证取值），未知值按默认处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    PriorityWeighted,
    LeastLatency,
}

impl Strategy {
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("least_latency") => Self::LeastLatency,
            _ => Self::PriorityWeighted,
        }
    }
}

/// 层内抽样权重（整数运算，非计费路径）。
fn effective_weight(c: &ChannelCandidate) -> i64 {
    (i64::from(c.weight.max(1)) * 1000 / c.cost_milli.max(1)).max(1)
}

/// 按 priority 分层（降序）。
fn layer(candidates: Vec<ChannelCandidate>) -> Vec<(i32, Vec<ChannelCandidate>)> {
    let mut groups: Vec<(i32, Vec<ChannelCandidate>)> = Vec::new();
    for cand in candidates {
        match groups.iter_mut().find(|(p, _)| *p == cand.priority) {
            Some((_, bucket)) => bucket.push(cand),
            None => groups.push((cand.priority, vec![cand])),
        }
    }
    groups.sort_by_key(|(priority, _)| std::cmp::Reverse(*priority));
    groups
}

pub fn order_candidates(candidates: Vec<ChannelCandidate>) -> Vec<ChannelCandidate> {
    let mut rng = rand::rng();
    let mut ordered = Vec::new();
    for (_, mut bucket) in layer(candidates) {
        // 层内按权重不放回抽样
        while !bucket.is_empty() {
            let total: i64 = bucket.iter().map(effective_weight).sum();
            let mut pick = rng.random_range(0..total);
            let mut idx = 0;
            for (i, cand) in bucket.iter().enumerate() {
                pick -= effective_weight(cand);
                if pick < 0 {
                    idx = i;
                    break;
                }
            }
            ordered.push(bucket.swap_remove(idx));
        }
    }
    ordered
}

/// 层内按时延 EWMA 升序。`latency` 缺项的 key 用本层中位数参与排序：
/// 给 0 会让新 key 抢下所有流量，给极大值会让它永远排不上——两者都不合理。
pub fn order_candidates_by_latency<S: std::hash::BuildHasher>(
    candidates: Vec<ChannelCandidate>,
    latency: &HashMap<i64, u32, S>,
) -> Vec<ChannelCandidate> {
    let mut ordered = Vec::new();
    for (_, mut bucket) in layer(candidates) {
        let mut samples: Vec<u32> = bucket
            .iter()
            .filter_map(|c| latency.get(&c.channel_key_id).copied())
            .collect();
        samples.sort_unstable();
        let fallback = if samples.is_empty() {
            0
        } else {
            samples[samples.len() / 2]
        };
        bucket.sort_by_key(|c| latency.get(&c.channel_key_id).copied().unwrap_or(fallback));
        ordered.append(&mut bucket);
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(priority: i32, weight: i32, cost_milli: i64) -> ChannelCandidate {
        ChannelCandidate {
            channel_id: 1,
            channel_key_id: 1,
            channel_name: "t".to_owned(),
            provider: "openai".to_owned(),
            api_base: None,
            credential: String::new(),
            priority,
            weight,
            trust_upstream_usage: false,
            max_concurrency: None,
            rpm_limit: None,
            daily_spend_cap_micro: None,
            model_mapping: serde_json::json!({}),
            thinking_to_content: false,
            bill_by_response_model: false,
            strip_request_fields: Vec::new(),
            capabilities: serde_json::json!({}),
            cost_milli,
        }
    }

    /// 成本感知：便宜渠道权重放大、贵渠道缩小；下限 1 防止饿死。
    #[test]
    fn effective_weight_cost_adjustment() {
        assert_eq!(effective_weight(&cand(0, 10, 1000)), 10, "中性成本原样");
        assert_eq!(effective_weight(&cand(0, 10, 500)), 20, "半价 → 双倍权重");
        assert_eq!(effective_weight(&cand(0, 10, 2000)), 5, "双倍成本 → 半权重");
        assert_eq!(effective_weight(&cand(0, 1, 100_000)), 1, "极贵也不为零");
    }

    /// priority 分层严格优先，与成本无关。
    #[test]
    fn priority_layers_ignore_cost() {
        let ordered = order_candidates(vec![cand(0, 100, 1), cand(10, 1, 100_000)]);
        assert_eq!(ordered[0].priority, 10, "高优先级层先行，成本只影响层内");
        assert_eq!(ordered[1].priority, 0);
    }

    fn cand_with_key(key: i64, priority: i32) -> ChannelCandidate {
        let mut c = cand(priority, 1, 1000);
        c.channel_key_id = key;
        c
    }

    /// least_latency：层内按 EWMA 升序，层与层的先后不被时延推翻。
    #[test]
    fn least_latency_orders_within_layer_only() {
        let mut lat = HashMap::new();
        lat.insert(1, 900_u32); // 高优先层里的慢 key
        lat.insert(2, 100); // 高优先层里的快 key
        lat.insert(3, 10); // 低优先层里最快的 key

        let ordered = order_candidates_by_latency(
            vec![
                cand_with_key(1, 10),
                cand_with_key(2, 10),
                cand_with_key(3, 0),
            ],
            &lat,
        );
        assert_eq!(
            ordered.iter().map(|c| c.channel_key_id).collect::<Vec<_>>(),
            vec![2, 1, 3],
            "层内按时延升序；低优先层再快也排在高优先层之后"
        );
    }

    /// 无样本的 key 按本层中位数参与：既不抢占全部流量，也不被永久饿死。
    #[test]
    fn unsampled_key_joins_at_median() {
        let mut lat = HashMap::new();
        lat.insert(1, 50_u32);
        lat.insert(2, 500);
        // key 3 无样本 → 取中位数（50 与 500 排序后取 index 1 = 500）
        let ordered = order_candidates_by_latency(
            vec![
                cand_with_key(1, 0),
                cand_with_key(2, 0),
                cand_with_key(3, 0),
            ],
            &lat,
        );
        let pos = |k: i64| ordered.iter().position(|c| c.channel_key_id == k).unwrap();
        assert!(pos(1) < pos(3), "有样本的快 key 应排在无样本 key 之前");
        assert!(
            pos(3) <= pos(2) + 1,
            "无样本 key 不该被推到队尾之后（它按中位数参与）"
        );
    }

    /// 策略解析：未知值退回默认，避免库里出现新值时热路径 panic。
    #[test]
    fn strategy_parse_falls_back_to_default() {
        assert_eq!(
            Strategy::parse(Some("least_latency")),
            Strategy::LeastLatency
        );
        assert_eq!(
            Strategy::parse(Some("nonsense")),
            Strategy::PriorityWeighted
        );
        assert_eq!(Strategy::parse(None), Strategy::PriorityWeighted);
    }
}
