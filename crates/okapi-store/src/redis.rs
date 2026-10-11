use crate::error::StoreError;
use fred::interfaces::ClientLike;
use fred::prelude::*;
use std::time::Duration;

/// 单条命令最长等待：热账本、鉴权缓存、结算 journal 的 Redis 调用都受它约束。
/// 超时按调用方已有的错误分支走（准入 fail-closed、鉴权回源 PG），不再无限挂起。
pub const REDIS_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// 连接上有帧等这么久没有回包，就当连接已失联：强制断开并按重连策略重建。
const REDIS_UNRESPONSIVE_AFTER: Duration = Duration::from_secs(10);

/// 建立 Redis 客户端（fred；单机与 Cluster 同一入口，键设计已带 hash-tag）。
///
/// fred 默认既没有重连策略也没有命令超时：Redis 重启后客户端不再重连、此后每条命令都永远挂起；
/// 网络黑洞（连接还在但不回包）时同样挂起。这里补齐三件事：
/// - 重连：指数退避、不限次数（100ms 起，封顶 5s）；
/// - 命令超时：[`REDIS_COMMAND_TIMEOUT`]；
/// - 失联检测：[`REDIS_UNRESPONSIVE_AFTER`] 内无回包即断开重连。
pub async fn connect_redis(redis_url: &str) -> Result<Client, StoreError> {
    let config = Config::from_url(redis_url)?;
    let client = Builder::from_config(config)
        .with_performance_config(|perf| perf.default_command_timeout = REDIS_COMMAND_TIMEOUT)
        .with_connection_config(|conn| {
            conn.unresponsive.max_timeout = Some(REDIS_UNRESPONSIVE_AFTER);
            conn.unresponsive.interval = Duration::from_secs(2);
        })
        .set_policy(ReconnectPolicy::new_exponential(0, 100, 5_000, 2))
        .build()?;
    client.init().await?;
    Ok(client)
}
