//! 集群在途量表：各实例上报本进程在途数，surge 规则读合计。
use super::SchedulerRedis;
use fred::interfaces::{HashesInterface, KeysInterface};

/// 在途量表：实例上报间隔上限 1s，故 10s 没动静即认为该实例已不在（不计入合计）。
const INFLIGHT_STALE_MS: i64 = 10_000;
/// 超过这个年龄的格子直接删掉，免得 pod 反复重建把 hash 撑大（名字随 pod 变）。
const INFLIGHT_EVICT_MS: i64 = 300_000;
/// 集群在途量表键。
const INFLIGHT_KEY: &str = "inflight:gauge";

impl SchedulerRedis {
    /// 上报本实例的在途请求数（surge 规则的负载输入）。
    ///
    /// 为什么按实例分格而不是一个全局 INCR/DECR 计数器：pod 崩在请求中间就再也减不回来，
    /// 计数只会单调漂高，surge 会永久卡在加价状态且没人察觉得到。每个实例只写自己那一格
    /// （`node → "<count>|<unix_ms>"`），崩掉的实例最多影响一个陈旧窗口就被读侧排除。
    pub async fn inflight_report(&self, node: &str, count: i64) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let value = format!("{count}|{now_ms}");
        if let Err(err) = self
            .client
            .hset::<(), _, _>(INFLIGHT_KEY, (node.to_owned(), value))
            .await
        {
            tracing::debug!(error = %err, "在途量上报失败");
            return;
        }
        // 整表兜底过期：全站没有网关在跑时不留垃圾键
        let _: Result<bool, _> = self.client.expire(INFLIGHT_KEY, 3600, None).await;
    }

    /// 集群在途请求数合计（陈旧格子不计；顺手清掉早已消失的实例）。
    pub async fn inflight_total(&self) -> i64 {
        let map: std::collections::HashMap<String, String> =
            match self.client.hgetall(INFLIGHT_KEY).await {
                Ok(m) => m,
                Err(err) => {
                    // 读不到就当没有负载：surge 是加价规则，宁可不加也不要凭空加
                    tracing::debug!(error = %err, "在途量读取失败，按零负载处理");
                    return 0;
                }
            };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut total = 0_i64;
        let mut evict: Vec<String> = Vec::new();
        for (node, raw) in map {
            let Some((count, at_ms)) = raw
                .split_once('|')
                .and_then(|(c, t)| Some((c.parse::<i64>().ok()?, t.parse::<i64>().ok()?)))
            else {
                evict.push(node);
                continue;
            };
            let age = now_ms.saturating_sub(at_ms);
            if age > INFLIGHT_EVICT_MS {
                evict.push(node);
            } else if age <= INFLIGHT_STALE_MS {
                total = total.saturating_add(count.max(0));
            }
        }
        if !evict.is_empty() {
            let _: Result<i64, _> = self.client.hdel(INFLIGHT_KEY, evict).await;
        }
        total
    }
}
