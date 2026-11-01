use super::{KPI_TTL_SECS, SchedulerRedis};
use fred::{error::Error, interfaces::LuaInterface};
use okapi_store::image_batches::statistics::Delivery;
use sha2::{Digest, Sha256};
use uuid::Uuid;

// Every metric and its receipt share a Redis Cluster slot. Retrying after a lost
// response cannot double-add earlier metrics; INCRBY receives the integer string
// unchanged (Lua doubles cannot represent all valid aggregate micro-USD values).
const ADD: &str = r"
    if redis.call('EXISTS', KEYS[2]) == 1 then return 0 end
    local until_at = tonumber(ARGV[2])
    local now = tonumber(redis.call('TIME')[1])
    if until_at <= now then return 2 end
    redis.call('INCRBY', KEYS[1], ARGV[1])
    local ttl = redis.call('TTL', KEYS[1])
    if ttl < until_at - now then redis.call('EXPIREAT', KEYS[1], until_at) end
    redis.call('SET', KEYS[2], '1', 'EXAT', until_at)
    return 1
";

fn receipt_key(key: &str, id: Uuid) -> String {
    let tag = key
        .split_once('{')
        .and_then(|(_, tail)| tail.split_once('}'))
        .map(|(tag, _)| tag)
        .filter(|tag| !tag.is_empty())
        .unwrap_or(key);
    format!(
        "batch-stats:{{{tag}}}:{}:{}",
        id.simple(),
        hex::encode(Sha256::digest(key.as_bytes()))
    )
}

impl SchedulerRedis {
    async fn batch_counter(
        &self,
        row: &Delivery,
        key: String,
        delta: i64,
        expires: i64,
    ) -> Result<(), Error> {
        if delta == 0 {
            return Ok(());
        }
        let marker = receipt_key(&key, row.batch_id);
        let _: i64 = self
            .client
            .eval(
                ADD,
                vec![key, marker],
                vec![delta.to_string(), expires.to_string()],
            )
            .await?;
        Ok(())
    }

    /// These are settlement-time buckets, never replay-time buckets. Expired
    /// buckets are skipped, not resurrected as fresh activity.
    pub async fn record_batch_statistics(&self, row: &Delivery) -> Result<(), Error> {
        let at = row.recorded_at;
        let month = at.format("%Y%m");
        let monthly_expiry = at.timestamp() + 40 * 24 * 3600;
        if let Some(member) = row.member_user_id {
            self.batch_counter(
                row,
                format!("spend:tm:{}:{member}:{month}", row.user_id),
                row.amount_micro,
                monthly_expiry,
            )
            .await?;
        }
        self.batch_counter(
            row,
            format!("tok:{{{}}}:{month}", row.user_id),
            row.tokens,
            monthly_expiry,
        )
        .await?;
        self.batch_counter(
            row,
            format!("usd:{{{}}}:{month}", row.user_id),
            row.amount_micro,
            monthly_expiry,
        )
        .await?;
        self.batch_counter(
            row,
            format!("spend:ck:{}:{}", row.channel_key_id, at.format("%Y%m%d")),
            row.amount_micro,
            at.timestamp() + 172_800,
        )
        .await?;
        for (key, delta) in Self::kpi_keys(at.timestamp()).into_iter().zip([
            1,
            row.tokens,
            row.amount_micro,
            i64::from(row.is_error),
        ]) {
            self.batch_counter(row, key, delta, at.timestamp() + KPI_TTL_SECS)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_share_cluster_slots_without_colliding_between_metrics() {
        let id = Uuid::new_v4();
        let keys = [
            "spend:tm:1:2:202609",
            "tok:{1}:202609",
            "usd:{1}:202609",
            "spend:ck:2:20260927",
            "kpi:{kpi}:req:1",
            "kpi:{kpi}:amt:1",
        ];
        let mut markers = std::collections::HashSet::new();
        for key in keys {
            let receipt = receipt_key(key, id);
            assert_eq!(
                fred::util::group_by_hash_slot([key, &receipt])
                    .unwrap()
                    .len(),
                1
            );
            assert!(markers.insert(receipt));
        }
    }
}
