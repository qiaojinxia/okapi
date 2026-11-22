//! 渠道池调度策略仓储：读写 `channel_pools.policy` / `policy_version`，以及池图无环检查。

use crate::error::StoreError;
use okapi_domain::pool_policy::{EffectivePoolPolicy, PoolPolicy, RequestFeatures};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

/// 渠道池生效配置快照（进入 `RoutingCaches` 的 5s TTL 缓存，独立于全站 `auth_flush`）。
#[derive(Debug, Clone)]
pub struct StoredPoolPolicy {
    pub pool_code: String,
    pub routing_strategy: String,
    pub fallback_pool_code: Option<String>,
    pub policy: PoolPolicy,
    pub policy_version: i64,
}

impl StoredPoolPolicy {
    /// 找不到池行时的安全回退（与既有 `default` / `priority_weighted` 一致）。
    #[must_use]
    pub fn fallback_default(pool_code: &str) -> Self {
        Self {
            pool_code: pool_code.to_owned(),
            routing_strategy: "priority_weighted".to_owned(),
            fallback_pool_code: None,
            policy: PoolPolicy::default(),
            policy_version: 0,
        }
    }

    #[must_use]
    pub fn resolve(&self, features: &RequestFeatures<'_>) -> EffectivePoolPolicy {
        self.policy.resolve(
            &self.pool_code,
            &self.routing_strategy,
            self.fallback_pool_code.as_deref(),
            features,
        )
    }
}

/// 池拓扑图节点（用于写入期存在性校验、无环检查与删除占用检查）。
#[derive(Debug, Clone)]
pub struct PoolGraphNode {
    pub pool_code: String,
    pub fallback_pool_code: Option<String>,
    pub policy: PoolPolicy,
}

impl PoolGraphNode {
    /// 该池通过单跳 `fallback_pool_code`、`policy.fallback_pools` 与规则 `override_pools`
    /// 引用的全部外部池列表。
    #[must_use]
    pub fn referenced_pools(&self) -> Vec<String> {
        let mut refs: Vec<String> = self
            .policy
            .referenced_pools()
            .into_iter()
            .map(str::to_owned)
            .collect();
        if let Some(fb) = self
            .fallback_pool_code
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && !refs.iter().any(|r| r == fb)
        {
            refs.push(fb.to_owned());
        }
        refs
    }

    /// 降级边（不含自引用）：`policy.fallback_pools` 非空时优先用它，否则退回 `fallback_pool_code`。
    fn fallback_edges(&self) -> Vec<String> {
        if !self.policy.fallback_pools.is_empty() {
            self.policy
                .fallback_pools
                .iter()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        } else if let Some(fb) = self
            .fallback_pool_code
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            vec![fb.to_owned()]
        } else {
            Vec::new()
        }
    }
}

fn strip_retired_policy_fields(raw: &mut serde_json::Value) {
    let Some(obj) = raw.as_object_mut() else {
        return;
    };
    obj.remove("compaction");
    if let Some(rules) = obj.get_mut("rules").and_then(serde_json::Value::as_array_mut) {
        for rule in rules {
            if let Some(apply) = rule
                .get_mut("apply")
                .and_then(serde_json::Value::as_object_mut)
            {
                apply.remove("compaction");
            }
        }
    }
}

fn parse_policy(pool_code: &str, mut raw: serde_json::Value) -> PoolPolicy {
    strip_retired_policy_fields(&mut raw);
    serde_json::from_value(raw).unwrap_or_else(|error| {
        tracing::warn!(
            pool_code,
            %error,
            "channel_pools.policy failed to deserialize; falling back to default policy"
        );
        PoolPolicy::default()
    })
}

/// 读取单个池的策略快照；不存在回 `Ok(None)`。
pub async fn load_pool_policy(
    pool: &PgPool,
    pool_code: &str,
) -> Result<Option<StoredPoolPolicy>, StoreError> {
    let row: Option<(String, String, Option<String>, serde_json::Value, i64)> = sqlx::query_as(
        r"SELECT pool_code, routing_strategy, fallback_pool_code, policy, policy_version
          FROM channel_pools
          WHERE pool_code = $1",
    )
    .bind(pool_code)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(code, routing_strategy, fallback_pool_code, raw_policy, policy_version)| {
            let policy = parse_policy(&code, raw_policy);
            StoredPoolPolicy {
                pool_code: code,
                routing_strategy,
                fallback_pool_code,
                policy,
                policy_version,
            }
        },
    ))
}

/// 读取全站渠道池拓扑（用于写入校验与删除引用检查；池数量通常个位到几十个）。
pub async fn list_pool_graph(pool: &PgPool) -> Result<Vec<PoolGraphNode>, StoreError> {
    let rows: Vec<(String, Option<String>, serde_json::Value)> = sqlx::query_as(
        r"SELECT pool_code, fallback_pool_code, policy
          FROM channel_pools
          ORDER BY pool_code",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(pool_code, fallback_pool_code, raw)| {
            let policy = parse_policy(&pool_code, raw);
            PoolGraphNode {
                pool_code,
                fallback_pool_code,
                policy,
            }
        })
        .collect())
}

/// 写入或更新渠道池（带可选 `policy`）。
///
/// - `policy == Some(p)`：写入新策略并将 `policy_version` 递增 1；
/// - `policy == None`：新建时落 `{}`（版本 1），更新已有池时**保留原 `policy` 与版本号**，
///   确保旧管理端表单只改描述/策略列时不会静默清空已配好的策略文档。
///
/// 返回写入后的最新 `policy_version`。
pub async fn upsert_pool_with_policy(
    pool: &PgPool,
    pool_code: &str,
    description: &str,
    routing_strategy: &str,
    fallback_pool_code: Option<&str>,
    policy: Option<&PoolPolicy>,
) -> Result<i64, StoreError> {
    let policy_json = policy
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| StoreError::InvalidData("pool_policy"))?;
    let version: i64 = sqlx::query_scalar(
        r"INSERT INTO channel_pools (pool_code, description, routing_strategy, fallback_pool_code, policy, policy_version)
          VALUES ($1, $2, $3, $4, COALESCE($5, '{}'::jsonb), 1)
          ON CONFLICT (pool_code) DO UPDATE SET
              description        = EXCLUDED.description,
              routing_strategy   = EXCLUDED.routing_strategy,
              fallback_pool_code = EXCLUDED.fallback_pool_code,
              policy             = COALESCE($5, channel_pools.policy),
              policy_version     = CASE
                                       WHEN $5 IS NULL THEN channel_pools.policy_version
                                       ELSE channel_pools.policy_version + 1
                                   END,
              updated_at         = now()
          RETURNING policy_version",
    )
    .bind(pool_code)
    .bind(description)
    .bind(routing_strategy)
    .bind(fallback_pool_code)
    .bind(policy_json)
    .fetch_one(pool)
    .await?;
    Ok(version)
}

/// 检查将 `target_pool` 更新为 `(new_fallback, new_policy)` 后，降级图是否形成环。
/// 同时覆盖单跳 `fallback_pool_code` 与多跳 `policy.fallback_pools`。
#[must_use]
pub fn has_fallback_cycle(
    existing: &[PoolGraphNode],
    target_pool: &str,
    new_fallback: Option<&str>,
    new_policy: &PoolPolicy,
) -> bool {
    let mut adj: HashMap<&str, Vec<String>> = HashMap::with_capacity(existing.len() + 1);
    for node in existing {
        if node.pool_code != target_pool {
            adj.insert(node.pool_code.as_str(), node.fallback_edges());
        }
    }
    let proposed = PoolGraphNode {
        pool_code: target_pool.to_owned(),
        fallback_pool_code: new_fallback.map(str::to_owned),
        policy: new_policy.clone(),
    };
    let start_edges = proposed.fallback_edges();
    adj.insert(target_pool, start_edges);

    // 从 target_pool 出发做可达性 DFS：若能回到 target_pool，则新边引入环。
    let mut visited = HashSet::new();
    let mut stack: Vec<String> = adj.get(target_pool).cloned().unwrap_or_default();
    while let Some(curr) = stack.pop() {
        if curr == target_pool {
            return true;
        }
        if !visited.insert(curr.clone()) {
            continue;
        }
        if let Some(nexts) = adj.get(curr.as_str()) {
            for n in nexts {
                stack.push(n.clone());
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_detection_catches_multi_hop_cycle() {
        let nodes = vec![
            PoolGraphNode {
                pool_code: "a".into(),
                fallback_pool_code: Some("b".into()),
                policy: PoolPolicy::default(),
            },
            PoolGraphNode {
                pool_code: "b".into(),
                fallback_pool_code: None,
                policy: PoolPolicy {
                    fallback_pools: vec!["c".into()],
                    ..Default::default()
                },
            },
            PoolGraphNode {
                pool_code: "c".into(),
                fallback_pool_code: None,
                policy: PoolPolicy::default(),
            },
        ];

        // c -> a 形成 a -> b -> c -> a 三跳环
        let cyc = PoolPolicy {
            fallback_pools: vec!["a".into()],
            ..Default::default()
        };
        assert!(has_fallback_cycle(&nodes, "c", None, &cyc));

        // c -> default 无环
        let ok = PoolPolicy {
            fallback_pools: vec!["default".into()],
            ..Default::default()
        };
        assert!(!has_fallback_cycle(&nodes, "c", None, &ok));
    }

    #[test]
    fn parse_policy_ignores_retired_compaction_field_in_stored_rows() {
        let legacy = serde_json::json!({
            "fallback_pools": ["backup"],
            "quality": { "enabled": true, "min_samples": 12 },
            "compaction": { "mode": "on_channel_switch", "min_prompt_tokens": 16000 },
            "rules": [{
                "name": "r1",
                "when": { "models": ["claude-*"] },
                "apply": { "canary_percent": 10, "compaction": { "mode": "off" } }
            }]
        });
        let parsed = parse_policy("default", legacy);
        assert_eq!(parsed.fallback_pools, vec!["backup"]);
        assert_eq!(parsed.quality.enabled, Some(true));
        assert_eq!(parsed.quality.min_samples, Some(12));
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].apply.canary_percent, Some(10));
    }
}
