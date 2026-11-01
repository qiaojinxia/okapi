use std::net::SocketAddr;

/// 运行配置（M1：环境变量 + .env；配置中心不引入）。
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub redis_url: String,
    pub bind: SocketAddr,
    pub single_user_mode: bool,
    /// 处理节点标识（billing_records.node / CH node 列）。
    pub node: String,
    /// console 控制面监听地址。
    pub console_bind: std::net::SocketAddr,
    /// ClickHouse HTTP 地址；缺省关闭（chsink 停用，统计 fail-closed）。
    pub clickhouse_url: Option<String>,
    /// NATS 地址；缺省 = 单机直连形态（outbox 由 worker 直接消费）。
    pub nats_url: Option<String>,
    /// 信封加密主密钥（32 字节 hex；TOTP 密钥等敏感字段）。缺省 = 2FA 注册不可用。
    pub master_key: Option<String>,
}

impl Config {
    /// `needs_redis = false` 给不碰 Redis 的一次性命令（`seal-credentials`、`migrate --dry-run`）：
    /// 没设 `OKAPI_REDIS_URL` 也能跑，`redis_url` 为空串。
    pub fn from_env(needs_redis: bool) -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        let database_url = std::env::var("DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("DATABASE_URL 未设置（见 .env.example）"))?;
        let redis_url = match std::env::var("OKAPI_REDIS_URL") {
            Ok(url) => url,
            Err(_) if !needs_redis => String::new(),
            Err(_) => anyhow::bail!("OKAPI_REDIS_URL 未设置（见 .env.example）"),
        };
        let bind = socket_addr("OKAPI_BIND", "127.0.0.1:8080")?;
        let single_user_mode =
            okapi_store::env_config::flag("OKAPI_SINGLE_USER_MODE").unwrap_or(false);
        // 实例名。缺省退到容器/主机名而不是固定串——多副本部署里它是**区分实例**的唯一凭据：
        // 记账的 node 列、以及 surge 的集群在途量表（每个实例只写自己那格）都靠它。
        // 都叫 okapi-1 的话，量表会挤在同一格互相覆盖，集群在途数直接算少。
        // compose 的 `deploy.replicas` 没法给每个副本发不同的环境变量，但 Docker/K8s
        // 都会把 HOSTNAME 设成唯一的容器/Pod 名，正好够用。
        let node = std::env::var("OKAPI_NODE")
            .ok()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "okapi-1".to_owned());
        let clickhouse_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
        let nats_url = std::env::var("OKAPI_NATS_URL").ok();
        let master_key = master_key_from_env();
        // 主密钥写错（不是 64 位 hex）时启动照常、到第一次加密才炸，表现成某个写操作莫名 500：
        // 启动时就封一次试试
        if let Some(key) = master_key.as_deref() {
            okapi_store::credential::seal(key, "probe").map_err(|error| {
                anyhow::anyhow!("OKAPI_MASTER_KEY 无效（应为 64 位 hex，即 32 字节）：{error}")
            })?;
        }
        let console_bind = socket_addr("OKAPI_CONSOLE_BIND", "127.0.0.1:8081")?;
        Ok(Self {
            database_url,
            redis_url,
            bind,
            single_user_mode,
            node,
            console_bind,
            clickhouse_url,
            nats_url,
            master_key,
        })
    }
}

/// 监听地址：解析失败时报出是哪个变量、写的是什么，而不是一句不带上下文的 `invalid socket address`。
fn socket_addr(name: &str, default: &str) -> anyhow::Result<SocketAddr> {
    let raw = std::env::var(name).unwrap_or_else(|_| default.to_owned());
    raw.trim().parse().map_err(|error| {
        anyhow::anyhow!("{name}={raw} 不是合法的监听地址（如 0.0.0.0:8080）：{error}")
    })
}

/// `.env.example` 的空值表示未配置，与没有该变量一致。
/// 非空错误值必须保留，让加密层拒绝，不能静默改成明文存储。
pub(crate) fn master_key_from_env() -> Option<String> {
    normalize_master_key(std::env::var("OKAPI_MASTER_KEY").ok())
}

fn normalize_master_key(value: Option<String>) -> Option<String> {
    value
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::normalize_master_key;

    #[test]
    fn blank_master_key_is_unconfigured() {
        for value in [None, Some(String::new()), Some(" \n\t ".to_owned())] {
            assert_eq!(normalize_master_key(value), None);
        }
    }

    #[test]
    fn configured_master_key_keeps_encryption_and_rejects_invalid_values() {
        let key = hex::encode([7_u8; 32]);
        let normalized = normalize_master_key(Some(format!("  {key}\n")));
        assert_eq!(normalized.as_deref(), Some(key.as_str()));
        let sealed =
            okapi_store::credential::seal_or_plain(normalized.as_deref(), "fixture").unwrap();
        assert!(okapi_store::credential::is_sealed(&sealed));
        assert!(okapi_store::credential::open(None, &sealed).is_err());
        for invalid in ["not-hex", "00"] {
            let normalized = normalize_master_key(Some(invalid.to_owned()));
            assert_eq!(normalized.as_deref(), Some(invalid));
            assert!(
                okapi_store::credential::seal_or_plain(normalized.as_deref(), "fixture").is_err()
            );
        }
    }
}
