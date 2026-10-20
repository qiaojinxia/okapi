//! Channel concurrency is a renewable member lease, never a shared counter.
//!
//! 出口代理（IMPLEMENTATION §11.41）的并发上限用同一种租约（`conc:px:{proxy}`）：一次准入同时占
//! key 与代理两份，任一份满了就当「渠道忙」放弃并退回已占的那份；任一份续租失败即整体失效。
use super::SchedulerRedis;
use fred::interfaces::{LuaInterface, SortedSetsInterface};
use okapi_providers::{UpstreamError, response_lifetime::ResponseGuard};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use uuid::Uuid;

const LEASE_MS: u64 = 90_000;
const ACQUIRE: &str = r"
 local t=redis.call('TIME'); local now=tonumber(t[1])*1000+math.floor(tonumber(t[2])/1000)
 redis.call('ZREMRANGEBYSCORE',KEYS[1],'-inf',now)
 if redis.call('ZCARD',KEYS[1])>=tonumber(ARGV[1]) then return 0 end
 redis.call('ZADD',KEYS[1],now+tonumber(ARGV[3]),ARGV[2])
 redis.call('PEXPIRE',KEYS[1],tonumber(ARGV[3])*2)
 return 1
";
const RENEW: &str = r"
 local score=redis.call('ZSCORE',KEYS[1],ARGV[1])
 local t=redis.call('TIME'); local now=tonumber(t[1])*1000+math.floor(tonumber(t[2])/1000)
 if not score or tonumber(score)<=now then return 0 end
 redis.call('ZADD',KEYS[1],now+tonumber(ARGV[2]),ARGV[1])
 redis.call('PEXPIRE',KEYS[1],tonumber(ARGV[2])*2)
 return 1
";

/// key 的并发租约集合：成员是一次在途尝试，分数是租约到期的毫秒时刻。
pub(super) fn key_leases(key: i64) -> String {
    format!("conc:ck:{{{key}}}:v2")
}

pub struct ChannelPermit {
    sched: SchedulerRedis,
    leases: Vec<(String, String)>,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
    expired: Pin<Box<dyn Future<Output = ()> + Send>>,
}
impl ChannelPermit {
    /// key 并发上限 + 该候选出口代理的并发上限（若有）。`least_concurrency` 池的候选
    /// （[`okapi_store::ChannelCandidate::count_in_flight`]）没设上限也登记一份只计数的租约。
    pub async fn acquire(
        sched: &SchedulerRedis,
        candidate: &okapi_store::ChannelCandidate,
    ) -> Result<Option<Self>, UpstreamError> {
        Self::acquire_with_ttl(
            sched,
            candidate.channel_key_id,
            candidate.max_concurrency,
            candidate
                .egress_proxy_id
                .zip(candidate.egress_max_concurrency),
            candidate.count_in_flight,
            LEASE_MS,
        )
        .await
    }
    pub async fn acquire_key(
        sched: &SchedulerRedis,
        key: i64,
        cap: Option<i32>,
    ) -> Result<Option<Self>, UpstreamError> {
        Self::acquire_parts(sched, key, cap, None).await
    }
    /// 不走候选的入口（Realtime、custom_pass）显式给出出口代理与它的上限。
    pub async fn acquire_parts(
        sched: &SchedulerRedis,
        key: i64,
        cap: Option<i32>,
        proxy: Option<(i64, i32)>,
    ) -> Result<Option<Self>, UpstreamError> {
        Self::acquire_with_ttl(sched, key, cap, proxy, false, LEASE_MS).await
    }
    #[allow(clippy::too_many_lines)] // 准入、回滚与续租共用同一份租约清单，拆开反而难核对
    async fn acquire_with_ttl(
        sched: &SchedulerRedis,
        key: i64,
        cap: Option<i32>,
        proxy: Option<(i64, i32)>,
        count: bool,
        ttl: u64,
    ) -> Result<Option<Self>, UpstreamError> {
        // (租约集合, 上限, 是否拦截)。只计数的租约上限放到最大：登记不上、续不上都不影响请求
        let mut wanted = Vec::new();
        match cap.filter(|cap| *cap > 0) {
            Some(cap) => wanted.push((key_leases(key), cap, true)),
            None if count => wanted.push((key_leases(key), i32::MAX, false)),
            None => {}
        }
        if let Some((proxy, cap)) = proxy.filter(|(_, cap)| *cap > 0) {
            wanted.push((format!("conc:px:{{{proxy}}}:v1"), cap, true));
        }
        // 先拿到的租约挂在 permit 上：后一份失败时随 drop 一并退回
        let mut permit = Self {
            sched: sched.clone(),
            leases: Vec::new(),
            heartbeat: None,
            expired: Box::pin(std::future::pending()),
        };
        let id = Uuid::new_v4().to_string();
        let mut renewal = Vec::new();
        for (name, cap, enforced) in wanted {
            let admitted = sched
                .client
                .eval::<i64, _, _, _>(
                    ACQUIRE,
                    vec![name.clone()],
                    vec![cap.to_string(), id.clone(), ttl.to_string()],
                )
                .await;
            if !enforced {
                // Redis 抖一下只是少计这一次在途，不能因此把请求拦下
                if matches!(admitted, Ok(1)) {
                    permit.leases.push((name.clone(), id.clone()));
                    renewal.push((name, id.clone(), false));
                }
                continue;
            }
            if !matches!(admitted, Ok(1)) {
                // 前面已占的租约等退回完成再返回：紧接着的下一次准入不能还看到它被占着
                permit.release().await;
                return match admitted {
                    Ok(_) => Ok(None),
                    Err(_) => Err(super::super::account_control::blocked(
                        "concurrency_unavailable",
                    )),
                };
            }
            permit.leases.push((name.clone(), id.clone()));
            renewal.push((name, id.clone(), true));
        }
        if renewal.is_empty() {
            return Ok(Some(permit));
        }
        let (lost_tx, lost_rx) = tokio::sync::oneshot::channel();
        let client = sched.client.clone();
        let heartbeat = tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis((ttl / 3).max(1)));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            timer.tick().await;
            while !renewal.is_empty() {
                timer.tick().await;
                let mut kept = Vec::with_capacity(renewal.len());
                for (name, id, enforced) in renewal {
                    let renewed = tokio::time::timeout(
                        Duration::from_millis((ttl / 3).max(1)),
                        client.eval::<i64, _, _, _>(
                            RENEW,
                            vec![name.clone()],
                            vec![id.clone(), ttl.to_string()],
                        ),
                    )
                    .await;
                    if matches!(renewed, Ok(Ok(1))) {
                        kept.push((name, id, enforced));
                    } else if enforced {
                        tracing::warn!(channel_key = key, lease = %name, "channel concurrency lease lost");
                        let _ = lost_tx.send(());
                        return;
                    } else {
                        // 只计数的租约续不上就不再计这次在途，请求照常
                        tracing::debug!(channel_key = key, lease = %name, "in-flight counting lease lapsed");
                    }
                }
                renewal = kept;
            }
            // 没有可续的租约了：守着 lost_tx 等 release 中止，别让 expired 误报租约丢失
            std::future::pending::<()>().await;
        });
        permit.heartbeat = Some(heartbeat);
        permit.expired = Box::pin(async {
            let _ = lost_rx.await;
        });
        Ok(Some(permit))
    }
    pub async fn expired(&mut self) {
        self.expired.as_mut().await;
    }
    fn spawn_release(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        if self.leases.is_empty() {
            return None;
        }
        let leases = std::mem::take(&mut self.leases);
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let sched = self.sched.clone();
        Some(runtime.spawn(async move {
            for (name, id) in leases {
                let _: Result<i64, _> = sched.client.zrem(name, id).await;
            }
        }))
    }
    pub async fn release(mut self) {
        if let Some(task) = self.spawn_release() {
            let _ = task.await;
        }
    }
}
impl Drop for ChannelPermit {
    fn drop(&mut self) {
        self.spawn_release();
    }
}
impl ResponseGuard for ChannelPermit {
    fn poll_expired(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.expired.as_mut().poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fred::interfaces::KeysInterface;
    async fn scheduler() -> SchedulerRedis {
        okapi_store::test_support::assert_isolated();
        SchedulerRedis::new(
            okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
                .await
                .unwrap(),
        )
    }
    fn key() -> i64 {
        (Uuid::new_v4().as_u128() & 0x7fff_ffff) as i64 + 100_000
    }
    #[tokio::test]
    async fn active_leases_renew_past_the_initial_expiry() {
        let sched = scheduler().await;
        let key = key();
        let permit = ChannelPermit::acquire_with_ttl(&sched, key, Some(1), None, false, 450)
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            ChannelPermit::acquire_key(&sched, key, Some(1))
                .await
                .unwrap()
                .is_none()
        );
        permit.release().await;
        let next = ChannelPermit::acquire_key(&sched, key, Some(1))
            .await
            .unwrap()
            .unwrap();
        next.release().await;
    }
    /// 出口代理的并发上限跨 key 共享：两把 key 走同一个代理（上限 1）时第二把进不来，
    /// 而且它已占到的 key 租约当场退回，不会把自己的 key 也卡住。
    #[tokio::test]
    async fn proxy_cap_is_shared_across_keys_and_rolls_back_the_key_lease() {
        let sched = scheduler().await;
        let (first_key, second_key, proxy) = (key(), key(), key());
        let first = ChannelPermit::acquire_parts(&sched, first_key, Some(1), Some((proxy, 1)))
            .await
            .unwrap()
            .unwrap();
        assert!(
            ChannelPermit::acquire_parts(&sched, second_key, Some(1), Some((proxy, 1)))
                .await
                .unwrap()
                .is_none(),
            "同一个代理已满"
        );
        // 第二把 key 的 key 租约已退回：不经代理时它自己的并发位是空的
        let alone = ChannelPermit::acquire_key(&sched, second_key, Some(1))
            .await
            .unwrap()
            .unwrap();
        alone.release().await;
        first.release().await;
        let second = ChannelPermit::acquire_parts(&sched, second_key, None, Some((proxy, 1)))
            .await
            .unwrap()
            .unwrap();
        second.release().await;
    }
    #[tokio::test]
    async fn stale_release_never_removes_a_new_permit() {
        let sched = scheduler().await;
        let key = key();
        let first = ChannelPermit::acquire_key(&sched, key, Some(1))
            .await
            .unwrap()
            .unwrap();
        let _: i64 = sched
            .client
            .del(format!("conc:ck:{{{key}}}:v2"))
            .await
            .unwrap();
        let second = ChannelPermit::acquire_key(&sched, key, Some(1))
            .await
            .unwrap()
            .unwrap();
        first.release().await;
        assert!(
            ChannelPermit::acquire_key(&sched, key, Some(1))
                .await
                .unwrap()
                .is_none()
        );
        second.release().await;
    }
    #[tokio::test]
    async fn stream_ownership_releases_on_eof_and_on_drop() {
        use futures::StreamExt;
        use okapi_providers::response_lifetime::ResponseLifetime;
        let sched = scheduler().await;
        let key = key();
        for eof in [true, false] {
            let permit = ChannelPermit::acquire_key(&sched, key, Some(1))
                .await
                .unwrap()
                .unwrap();
            let response = okapi_providers::ChatResponse::Stream(okapi_providers::StreamHandle {
                upstream_request_id: None,
                events: Box::pin(futures::stream::empty()),
            })
            .with_guard(permit);
            assert!(
                ChannelPermit::acquire_key(&sched, key, Some(1))
                    .await
                    .unwrap()
                    .is_none()
            );
            let okapi_providers::ChatResponse::Stream(mut stream) = response else {
                unreachable!()
            };
            if eof {
                assert!(stream.events.next().await.is_none());
            }
            drop(stream);
            let permit = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(permit) = ChannelPermit::acquire_key(&sched, key, Some(1))
                        .await
                        .unwrap()
                    {
                        break permit;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            permit.release().await;
        }
    }
    #[tokio::test]
    async fn losing_a_lease_stops_the_stream_and_cannot_release_a_new_member() {
        use futures::StreamExt;
        use okapi_providers::response_lifetime::ResponseLifetime;
        let sched = scheduler().await;
        let key = key();
        let permit = ChannelPermit::acquire_with_ttl(&sched, key, Some(1), None, false, 150)
            .await
            .unwrap()
            .unwrap();
        let response = okapi_providers::ChatResponse::Stream(okapi_providers::StreamHandle {
            upstream_request_id: None,
            events: Box::pin(futures::stream::pending()),
        })
        .with_guard(permit);
        let okapi_providers::ChatResponse::Stream(mut stream) = response else {
            unreachable!()
        };
        let _: i64 = sched
            .client
            .del(format!("conc:ck:{{{key}}}:v2"))
            .await
            .unwrap();
        let next = ChannelPermit::acquire_key(&sched, key, Some(1))
            .await
            .unwrap()
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), stream.events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.error_code(), "no_available_channel");
        assert!(stream.events.next().await.is_none());
        assert!(
            ChannelPermit::acquire_key(&sched, key, Some(1))
                .await
                .unwrap()
                .is_none()
        );
        next.release().await;
    }
    /// least_concurrency 池：没设上限的 key 也登记只计数的租约——从不拦、读得到在途、
    /// 长请求照常续租、退回后归零；不计数的尝试不出现在读数里。
    #[tokio::test]
    async fn counting_leases_never_block_and_are_counted_until_released() {
        let sched = scheduler().await;
        let key = key();
        let in_flight = || async {
            sched
                .channel_key_in_flight(std::iter::once(key))
                .await
                .get(&key)
                .copied()
                .unwrap_or(0)
        };
        let first = ChannelPermit::acquire_with_ttl(&sched, key, None, None, true, 450)
            .await
            .unwrap()
            .unwrap();
        let second = ChannelPermit::acquire_with_ttl(&sched, key, None, None, true, 450)
            .await
            .unwrap()
            .unwrap();
        let uncounted = ChannelPermit::acquire_key(&sched, key, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(in_flight().await, 2);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(in_flight().await, 2, "续租后仍在途");
        first.release().await;
        assert_eq!(in_flight().await, 1);
        uncounted.release().await;
        second.release().await;
        assert_eq!(in_flight().await, 0);
    }
    /// 只计数的租约丢了（Redis 被清、续租失败）只是少计，不能当成并发位丢失去中断请求。
    #[tokio::test]
    async fn losing_a_counting_lease_never_expires_the_permit() {
        let sched = scheduler().await;
        let key = key();
        let mut permit = ChannelPermit::acquire_with_ttl(&sched, key, None, None, true, 150)
            .await
            .unwrap()
            .unwrap();
        let _: i64 = sched.client.del(key_leases(key)).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(600), permit.expired())
                .await
                .is_err()
        );
        permit.release().await;
    }
}
