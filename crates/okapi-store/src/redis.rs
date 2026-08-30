use crate::error::StoreError;
use fred::interfaces::ClientLike;
use fred::prelude::*;

/// 建立 Redis 客户端（fred；单机与 Cluster 同一入口，键设计已带 hash-tag）。
pub async fn connect_redis(redis_url: &str) -> Result<Client, StoreError> {
    let config = Config::from_url(redis_url)?;
    let client = Builder::from_config(config).build()?;
    client.init().await?;
    Ok(client)
}
