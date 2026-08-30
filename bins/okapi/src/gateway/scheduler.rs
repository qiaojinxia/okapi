//! 调度：priority 降序分层 + 层内（成本修正后）加权随机排列，返回 failover 尝试序。
//! 成本感知权重语义见 IMPLEMENTATION §3.8：有效权重 = weight × 1000 / cost_milli。

use okapi_store::ChannelCandidate;
use rand::RngExt;

/// 层内抽样权重（整数运算，非计费路径）。
fn effective_weight(c: &ChannelCandidate) -> i64 {
    (i64::from(c.weight.max(1)) * 1000 / c.cost_milli.max(1)).max(1)
}

pub fn order_candidates(candidates: Vec<ChannelCandidate>) -> Vec<ChannelCandidate> {
    let mut rng = rand::rng();
    let mut groups: Vec<(i32, Vec<ChannelCandidate>)> = Vec::new();
    for cand in candidates {
        match groups.iter_mut().find(|(p, _)| *p == cand.priority) {
            Some((_, bucket)) => bucket.push(cand),
            None => groups.push((cand.priority, vec![cand])),
        }
    }
    groups.sort_by_key(|(priority, _)| std::cmp::Reverse(*priority));

    let mut ordered = Vec::new();
    for (_, mut bucket) in groups {
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
}
