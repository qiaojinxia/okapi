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
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        let database_url = std::env::var("DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("DATABASE_URL 未设置（见 .env.example）"))?;
        let redis_url = std::env::var("OKAPI_REDIS_URL")
            .map_err(|_| anyhow::anyhow!("OKAPI_REDIS_URL 未设置（见 .env.example）"))?;
        let bind: SocketAddr = std::env::var("OKAPI_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
            .parse()?;
        let single_user_mode =
            std::env::var("OKAPI_SINGLE_USER_MODE").is_ok_and(|v| v == "true" || v == "1");
        let node = std::env::var("OKAPI_NODE").unwrap_or_else(|_| "okapi-1".to_owned());
        let clickhouse_url = std::env::var("OKAPI_CLICKHOUSE_URL").ok();
        let nats_url = std::env::var("OKAPI_NATS_URL").ok();
        let master_key = std::env::var("OKAPI_MASTER_KEY").ok();
        let console_bind: std::net::SocketAddr = std::env::var("OKAPI_CONSOLE_BIND")
            .unwrap_or_else(|_| "127.0.0.1:8081".to_owned())
            .parse()?;
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
