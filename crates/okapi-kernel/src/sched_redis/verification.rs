//! 一次性凭据：OAuth state、邮箱验证码、找回密码 token、待确认的 TOTP 绑定。
use super::SchedulerRedis;
use fred::interfaces::KeysInterface;
use fred::types::{Expiration, SetOptions};

impl SchedulerRedis {
    /// OAuth state 一次性键（CSRF 防线）：写入带 TTL。
    pub async fn oauth_state_set(&self, token: &str, ttl_secs: i64) {
        let result: Result<(), _> = self
            .client
            .set(
                format!("oauth:state:{token}"),
                "1",
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, "oauth_state_set 失败");
        }
    }

    /// 校验并销毁（DEL 返回 1 = 有效且唯一一次）。
    pub async fn oauth_state_take(&self, token: &str) -> bool {
        let deleted: Result<i64, _> = self.client.del(format!("oauth:state:{token}")).await;
        deleted.is_ok_and(|n| n == 1)
    }

    /// 渠道 OAuth 登录流程的 PKCE 状态（`oauth:cred:<state>`，§11.38）。返回 false = Redis 故障。
    pub async fn oauth_cred_state_set(&self, state: &str, payload: &str, ttl_secs: i64) -> bool {
        let result: Result<(), _> = self
            .client
            .set(
                format!("oauth:cred:{state}"),
                payload,
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        result.is_ok()
    }

    /// 取出并销毁（GETDEL）；不存在 / 已用 / 过期 = None。
    pub async fn oauth_cred_state_take(&self, state: &str) -> Option<String> {
        self.client
            .getdel::<Option<String>, _>(format!("oauth:cred:{state}"))
            .await
            .ok()
            .flatten()
    }

    // ---- 邮箱验证码 / 找回密码（IMPLEMENTATION §11.27）----

    /// 同一邮箱重发冷却：NX 抢到 = 允许发送。Redis 故障放行（限流是尽力语义）。
    pub async fn email_code_cooldown_acquire(&self, email: &str, ttl_secs: i64) -> bool {
        let set: Result<Option<String>, _> = self
            .client
            .set(
                format!("verify:email:cd:{email}"),
                "1",
                Some(Expiration::EX(ttl_secs)),
                Some(SetOptions::NX),
                false,
            )
            .await;
        match set {
            Ok(reply) => reply.is_some(),
            Err(_) => true,
        }
    }

    /// 存验证码（覆盖旧码）。返回 false = Redis 故障，调用方应报错而不是发一封对不上的码。
    /// 码与冷却各占 `code:` / `cd:` 前缀：此前码键是 `verify:email:<email>`，邮箱 `cd:victim@…`
    /// 的码键恰好就是 victim 的冷却键，能定向挡住别人取码。
    pub async fn email_code_set(&self, email: &str, code: &str, ttl_secs: i64) -> bool {
        let result: Result<(), _> = self
            .client
            .set(
                format!("verify:email:code:{email}"),
                code,
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        if let Err(err) = &result {
            tracing::warn!(error = %err, "email_code_set 失败");
        }
        result.is_ok()
    }

    /// 校验并销毁：对上即 DEL（一次性）；不对 / 不存在 / Redis 故障 → false。
    pub async fn email_code_take(&self, email: &str, code: &str) -> bool {
        // Every attempt consumes the challenge; GETDEL makes success single-use
        // even when two requests present the right code concurrently.
        let stored: Option<String> = self
            .client
            .getdel(format!("verify:email:code:{email}"))
            .await
            .ok()
            .flatten();
        stored.is_some_and(|stored| {
            stored.len() == code.len()
                && stored
                    .bytes()
                    .zip(code.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
        })
    }

    /// 找回密码 token → user_id（键存 token 的 sha256，明文只出现在邮件里）。
    ///
    /// 每个用户同一时刻只有一个有效 token：`pwreset:user:<uid>` 记着他当前那把的哈希，发新的
    /// 就把上一把作废。否则早先发出、可能已经泄露的链接在 TTL 内一直能重置密码——用户用新链接
    /// 改完密码之后也一样。先落新 token 再换指针：并发的两次申请里，后换指针的那次一定能看到
    /// 并删掉先换的那把，不会漏下一个删不掉的。返回 false = Redis 故障（调用方不要发信）。
    pub async fn pwreset_set(&self, token_hash: &str, user_id: i64, ttl_secs: i64) -> bool {
        let token_key = format!("pwreset:{token_hash}");
        let stored: Result<(), _> = self
            .client
            .set(
                token_key.clone(),
                user_id.to_string(),
                Some(Expiration::EX(ttl_secs)),
                None,
                false,
            )
            .await;
        if let Err(err) = &stored {
            tracing::warn!(error = %err, "pwreset_set 失败");
            return false;
        }
        let swapped: Result<Option<String>, _> = self
            .client
            .set(
                format!("pwreset:user:{user_id}"),
                token_hash,
                Some(Expiration::EX(ttl_secs)),
                None,
                true,
            )
            .await;
        match swapped {
            Ok(Some(previous)) if previous != token_hash => {
                let _: Result<i64, _> = self.client.del(format!("pwreset:{previous}")).await;
            }
            Ok(_) => {}
            Err(err) => {
                // 守不住「只留一把」就不发这把：撤回刚落的 token，让调用方报错
                tracing::warn!(error = %err, "pwreset 指针更新失败");
                let _: Result<i64, _> = self.client.del(token_key).await;
                return false;
            }
        }
        true
    }

    /// 取出并销毁 token（GETDEL），返回 user_id。
    pub async fn pwreset_take(&self, token_hash: &str) -> Option<i64> {
        let value: Option<String> = self
            .client
            .getdel(format!("pwreset:{token_hash}"))
            .await
            .ok()
            .flatten();
        value?.parse().ok()
    }

    pub async fn totp_pending_set(&self, binding: &str, sealed: &str) -> bool {
        let r: Result<(), _> = self
            .client
            .set(
                format!("totp:pending:{binding}"),
                sealed,
                Some(Expiration::EX(300)),
                None,
                false,
            )
            .await;
        r.is_ok()
    }
    pub async fn totp_pending_get(&self, binding: &str) -> Option<String> {
        self.client
            .get(format!("totp:pending:{binding}"))
            .await
            .ok()
            .flatten()
    }
    pub async fn totp_pending_del(&self, binding: &str) {
        let _: Result<i64, _> = self.client.del(format!("totp:pending:{binding}")).await;
    }
}

#[cfg(test)]
mod verification_tests {
    use super::*;
    #[tokio::test]
    async fn email_code_is_consumed_atomically_including_failed_guesses() {
        okapi_store::test_support::assert_isolated();
        let redis = okapi_store::connect_redis(&std::env::var("OKAPI_REDIS_URL").unwrap())
            .await
            .unwrap();
        let sched = SchedulerRedis::new(redis);
        let email = format!("atomic-{}@example.test", uuid::Uuid::new_v4());
        assert!(sched.email_code_set(&email, "123456", 60).await);
        let (a, b) = tokio::join!(
            sched.email_code_take(&email, "123456"),
            sched.email_code_take(&email, "123456")
        );
        assert_ne!(a, b, "one winner per code");
        assert!(sched.email_code_set(&email, "123456", 60).await);
        assert!(!sched.email_code_take(&email, "000000").await);
        assert!(
            !sched.email_code_take(&email, "123456").await,
            "wrong guesses consume the challenge"
        );
    }
}
