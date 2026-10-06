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

pub struct ChannelPermit {
    sched: SchedulerRedis,
    leases: Vec<(String, String)>,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
    expired: Pin<Box<dyn Future<Output = ()> + Send>>,
}
impl ChannelPermit {
    /// key 并发上限 + 该候选出口代理的并发上限（若有）。
    pub async fn acquire(
        sched: &SchedulerRedis,
        candidate: &okapi_store::ChannelCandidate,
    ) -> Result<Option<Self>, UpstreamError> {
        Self::acquire_parts(
            sched,
            candidate.channel_key_id,
            candidate.max_concurrency,
            candidate
                .egress_proxy_id
                .zip(candidate.egress_max_concurrency),
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
        Self::acquire_with_ttl(sched, key, cap, proxy, LEASE_MS).await
    }
    async fn acquire_with_ttl(
        sched: &SchedulerRedis,
        key: i64,
        cap: Option<i32>,
        proxy: Option<(i64, i32)>,
        ttl: u64,
    ) -> Result<Option<Self>, UpstreamError> {
        let mut wanted = Vec::new();
        if let Some(cap) = cap.filter(|cap| *cap > 0) {
            wanted.push((format!("conc:ck:{{{key}}}:v2"), cap));
        }
        if let Some((proxy, cap)) = proxy.filter(|(_, cap)| *cap > 0) {
            wanted.push((format!("conc:px:{{{proxy}}}:v1"), cap));
        }
        // 先拿到的租约挂在 permit 上：后一份失败时随 drop 一并退回
        let mut permit = Self {
            sched: sched.clone(),
            leases: Vec::new(),
            heartbeat: None,
            expired: Box::pin(std::future::pending()),
        };
        if wanted.is_empty() {
            return Ok(Some(permit));
        }
        let id = Uuid::new_v4().to_string();
        for (name, cap) in wanted {
            let admitted = sched
                .client
                .eval::<i64, _, _, _>(
                    ACQUIRE,
                    vec![name.clone()],
                    vec![cap.to_string(), id.clone(), ttl.to_string()],
                )
                .await;
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
            permit.leases.push((name, id.clone()));
        }
        let (lost_tx, lost_rx) = tokio::sync::oneshot::channel();
        let renewal = (sched.clone(), permit.leases.clone());
        let heartbeat = tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_millis((ttl / 3).max(1)));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            timer.tick().await;
            loop {
                timer.tick().await;
                for (name, id) in &renewal.1 {
                    let renewed = tokio::time::timeout(
                        Duration::from_millis((ttl / 3).max(1)),
                        renewal.0.client.eval::<i64, _, _, _>(
                            RENEW,
                            vec![name.clone()],
                            vec![id.clone(), ttl.to_string()],
                        ),
                    )
                    .await;
                    if !matches!(renewed, Ok(Ok(1))) {
                        tracing::warn!(channel_key = key, lease = %name, "channel concurrency lease lost");
                        let _ = lost_tx.send(());
                        return;
                    }
                }
            }
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
        let permit = ChannelPermit::acquire_with_ttl(&sched, key, Some(1), None, 450)
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
        let permit = ChannelPermit::acquire_with_ttl(&sched, key, Some(1), None, 150)
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
}
