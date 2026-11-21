use crate::error::StoreError;
use fred::interfaces::{ClientLike, LuaInterface};
use fred::prelude::*;
use fred::types::{FromValue, MultipleKeys, MultipleValues};
use sha1::{Digest, Sha1};
use std::sync::OnceLock;
use std::time::Duration;

/// 预计算 SHA-1 的 Redis Lua 脚本句柄：默认走 `EVALSHA`（只传 40 字节哈希），
/// 遇到 `NOSCRIPT`（冷启动、Redis 重启、主从切换或 `SCRIPT FLUSH`）自动回退一次 `EVAL`
/// （同时在目标分片重新缓存脚本，Cluster 下按 `KEYS[1]` 路由无 CROSSSLOT 问题）。
pub struct CachedScript {
    source: &'static str,
    sha1: OnceLock<String>,
}

impl CachedScript {
    #[must_use]
    pub const fn new(source: &'static str) -> Self {
        Self {
            source,
            sha1: OnceLock::new(),
        }
    }

    #[must_use]
    pub fn sha1(&self) -> &str {
        self.sha1
            .get_or_init(|| hex::encode(Sha1::digest(self.source.as_bytes())))
            .as_str()
    }

    /// 执行脚本：优先 `EVALSHA`，遇 `NOSCRIPT` 回退 `EVAL`。
    pub async fn eval<R, K, A>(
        &self,
        client: &Client,
        keys: K,
        args: A,
    ) -> Result<R, fred::error::Error>
    where
        R: FromValue,
        K: Into<MultipleKeys>,
        A: TryInto<MultipleValues>,
        A::Error: Into<fred::error::Error>,
    {
        let keys = keys.into();
        let args = args.try_into().map_err(Into::into)?;
        let sha1 = self.sha1();
        match client.evalsha(sha1, keys.clone(), args.clone()).await {
            Err(err) if is_noscript(&err) => Box::pin(client.eval(self.source, keys, args)).await,
            other => other,
        }
    }
}

fn is_noscript(err: &fred::error::Error) -> bool {
    err.details().contains("NOSCRIPT")
}

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

#[cfg(test)]
mod tests {
    use super::CachedScript;

    #[test]
    fn cached_script_computes_standard_redis_sha1() {
        static TIME_SCRIPT: CachedScript = CachedScript::new("return redis.call('TIME')");
        // Matches `printf "return redis.call('TIME')" | shasum` / Redis `SCRIPT LOAD`.
        assert_eq!(
            TIME_SCRIPT.sha1(),
            "104ae65debf45156ded1e8e613705534c8e62c17"
        );
        // Repeated reads return the same cached hash.
        assert_eq!(
            TIME_SCRIPT.sha1(),
            "104ae65debf45156ded1e8e613705534c8e62c17"
        );
    }
}
