//! 调度：priority 降序分层 + 层内排列，返回 failover 尝试序。
//!
//! 层内怎么排由池的 routing_strategy 决定（docs/database.md §3.7）：
//! - `priority_weighted`（默认）：成本修正后加权随机。有效权重 = weight × 1000 / cost_milli。
//! - `least_latency`：按时延 EWMA 升序，无样本者按本层中位数插队。
//!
//! priority 分层在两种策略下都严格生效——层是运维显式表达的"先用谁"，
//! 不该被时延或权重推翻。分层键是 (pool_rank, priority)：主池的全部层排完才进
//! 降级池（IMPLEMENTATION §11.14）——降级池里优先级再高的渠道，也是"主池打不通
//! 才轮到"的备胎。

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

/// 分层键：池序升序（主池先）、层内 priority 降序。
type LayerKey = (i32, std::cmp::Reverse<i32>);

fn layer_key(c: &ChannelCandidate) -> LayerKey {
    (c.pool_rank, std::cmp::Reverse(c.priority))
}

/// 按 (pool_rank 升序, priority 降序) 分层。
fn layer(candidates: Vec<ChannelCandidate>) -> Vec<(LayerKey, Vec<ChannelCandidate>)> {
    let mut groups: Vec<(LayerKey, Vec<ChannelCandidate>)> = Vec::new();
    for cand in candidates {
        let key = layer_key(&cand);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, bucket)) => bucket.push(cand),
            None => groups.push((key, vec![cand])),
        }
    }
    groups.sort_by_key(|(key, _)| *key);
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
            data_retention: None,
            priority,
            weight,
            trust_upstream_usage: false,
            same_key_retries: 1,
            first_output_timeout_secs: 30,
            max_concurrency: None,
            rpm_limit: None,
            daily_spend_cap_micro: None,
            model_mapping: serde_json::json!({}),
            thinking_to_content: false,
            bill_by_response_model: false,
            strip_request_fields: Vec::new(),
            inject_request_fields: serde_json::Map::new(),
            responses_native: true,
            api_version: None,
            aws_region: None,
            oauth_token_url: None,
            proxy_url: None,
            extra_headers: Vec::new(),
            capabilities: serde_json::json!({}),
            cost_milli,
            pool_rank: 0,
        }
    }

    fn cand_in_pool(key: i64, pool_rank: i32, priority: i32) -> ChannelCandidate {
        let mut c = cand(priority, 1, 1000);
        c.channel_key_id = key;
        c.pool_rank = pool_rank;
        c
    }

    /// 降级池整体排在主池之后：备池里 priority 100 的渠道也要等主池 priority 0 的层用完。
    #[test]
    fn fallback_pool_layers_after_primary() {
        let ordered = order_candidates(vec![
            cand_in_pool(1, 1, 100),
            cand_in_pool(2, 0, 0),
            cand_in_pool(3, 0, 10),
        ]);
        assert_eq!(
            ordered.iter().map(|c| c.channel_key_id).collect::<Vec<_>>(),
            vec![3, 2, 1],
            "主池按 priority 降序排完，降级池最后"
        );
        let mut lat = HashMap::new();
        lat.insert(1, 1_u32);
        lat.insert(2, 900);
        let by_lat =
            order_candidates_by_latency(vec![cand_in_pool(1, 1, 0), cand_in_pool(2, 0, 0)], &lat);
        assert_eq!(
            by_lat[0].channel_key_id, 2,
            "least_latency 也不能把降级池提到主池之前"
        );
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
        // 判据要钉死的是"**中位数**"，不是"某个来自样本的值"。
        // 旧版只喂两个样本 {50,500} 并断言相对位置，把 fallback 换成**最小值**
        // 照样绿（并列时输入序决定胜负，恰好还是原来的相对位置）——变异测试实测活了下来。
        // 取最小和取 0 一样坏：无样本的新渠道会排到本层最快渠道的位置，
        // 抢在已验证的好渠道前面拿流量。
        //
        // 并列时的胜负由输入序定，而"杀掉取最小"与"杀掉取最大"需要相反的输入序，
        // 一次排序做不到，故分两个子 case。样本 {10,100,1000}，中位数 = 100。
        let mut lat = HashMap::new();
        lat.insert(10, 10_u32);
        lat.insert(100, 100);
        lat.insert(1000, 1000);
        let others = || {
            vec![
                cand_with_key(10, 0),
                cand_with_key(100, 0),
                cand_with_key(1000, 0),
            ]
        };
        let pos_in = |ordered: &[ChannelCandidate], k: i64| {
            ordered.iter().position(|c| c.channel_key_id == k).unwrap()
        };

        // A：无样本者排在输入最前。取最小 → 它会并列到最快那个之前，被下面第一条断言杀掉。
        let mut a = vec![cand_with_key(7, 0)];
        a.extend(others());
        let a = order_candidates_by_latency(a, &lat);
        assert!(
            pos_in(&a, 10) < pos_in(&a, 7),
            "无样本 key 不得排到本层最快 key 之前（取最小或取 0 都会这样）"
        );

        // B：无样本者排在输入最后。取最大 → 它会并列到最慢那个之后，被下面第二条断言杀掉。
        let mut b = others();
        b.push(cand_with_key(7, 0));
        let b = order_candidates_by_latency(b, &lat);
        assert!(
            pos_in(&b, 7) < pos_in(&b, 1000),
            "无样本 key 不得排到本层最慢 key 之后（取最大会这样，等于永远排不上）"
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
