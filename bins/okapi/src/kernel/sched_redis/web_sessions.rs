//! 门户 / 管理后台的 web 会话（cookie sid → 用户，7 天滑动）。
use super::SchedulerRedis;
use fred::interfaces::{HashesInterface, KeysInterface, LuaInterface, SetsInterface};

const WEB_SESSION_TTL_SECS: i64 = 7 * 24 * 3600;

/// 门户会话列表行（不含 cookie 原文以外的敏感字段；sid 本身是能力令牌，仅回给属主）。
#[derive(Debug, Clone)]
pub struct WebSessionRow {
    pub sid: String,
    pub ip: Option<String>,
    pub ua: Option<String>,
    /// 建立时刻（unix 秒，展示用）。
    pub created_at: i64,
    /// 建立时刻（unix 毫秒，排序用）：同秒内多次登录要分得出先后，会话上限才能踢对人。
    pub created_ms: i64,
}

impl SchedulerRedis {
    /// web 会话 id：`<user_id>.<随机串>`。三把会话键都挂 `{user_id}` hash-tag（Cluster 下同槽，
    /// 登记 / 校验 / 吊销的 Lua 才成立），校验时只有 cookie 里的 sid，属主得能从 sid 里读出来。
    #[must_use]
    pub fn web_session_sid(user_id: i64, random: &str) -> String {
        format!("{user_id}.{random}")
    }

    /// sid 的属主（前缀）；不是 `<数字>.<非空>` 形状的一律当无效会话。
    fn web_session_owner(sid: &str) -> Option<i64> {
        let (uid, rest) = sid.split_once('.')?;
        (!rest.is_empty()).then_some(())?;
        uid.parse().ok().filter(|uid: &i64| *uid > 0)
    }

    /// (会话键, 用户索引键, 元数据键)，同一个 `{user_id}` tag。
    #[must_use]
    pub fn web_session_keys(sid: &str) -> Option<(String, String, String)> {
        let uid = Self::web_session_owner(sid)?;
        Some((
            format!("sess:{{{uid}}}:web:{sid}"),
            format!("sess:{{{uid}}}:idx"),
            format!("sess:{{{uid}}}:meta:{sid}"),
        ))
    }

    /// web 会话（/auth/* 自助面专用，§6.4）：7d 滑动过期；同步写入用户索引与展示元数据。
    /// `sid` 必须由 [`Self::web_session_sid`] 为同一用户生成。
    pub async fn web_session_set(
        &self,
        sid: &str,
        user_id: i64,
        ip: Option<&str>,
        ua: Option<&str>,
    ) {
        let Some((web, idx, meta)) =
            Self::web_session_keys(sid).filter(|_| Self::web_session_owner(sid) == Some(user_id))
        else {
            tracing::error!(
                user_id,
                "web session id does not belong to the user; not stored"
            );
            return;
        };
        // Index first, valid session last. Lua is atomic but does not roll back
        // command errors: this ordering also handles ACL rejection safely.
        let now = chrono::Utc::now();
        let script = r"
          local idx_type=redis.call('TYPE',KEYS[2]).ok
          local meta_type=redis.call('TYPE',KEYS[3]).ok
          if idx_type~='none' and idx_type~='set' then return redis.error_reply('invalid session index') end
          if meta_type~='none' and meta_type~='hash' then return redis.error_reply('invalid session metadata') end
          redis.call('SADD',KEYS[2],ARGV[1])
          redis.call('EXPIRE',KEYS[2],ARGV[3])
          redis.call('HSET',KEYS[3],'ip',ARGV[4],'ua',ARGV[5],'created_at',ARGV[6],'created_ms',ARGV[7])
          redis.call('EXPIRE',KEYS[3],ARGV[3])
          redis.call('SET',KEYS[1],ARGV[2],'EX',ARGV[3])
          return 1
        ";
        let result: Result<i64, _> = self
            .client
            .eval(
                script,
                vec![web, idx, meta],
                vec![
                    sid.to_owned(),
                    user_id.to_string(),
                    WEB_SESSION_TTL_SECS.to_string(),
                    ip.unwrap_or("").to_owned(),
                    ua.unwrap_or("").to_owned(),
                    now.timestamp().to_string(),
                    now.timestamp_millis().to_string(),
                ],
            )
            .await;
        if let Err(error) = result {
            tracing::warn!(%error, "web session creation failed");
        }
    }

    pub async fn web_session_get(&self, sid: &str) -> Option<i64> {
        self.web_session_check(sid).await.ok().flatten()
    }

    /// 同 [`Self::web_session_get`]，但把「会话不存在 / 已失效」（`Ok(None)`）和
    /// 「Redis 不可达」（`Err`）分开：数据面鉴权据此区分 401 与 503。
    pub async fn web_session_check(&self, sid: &str) -> Result<Option<i64>, fred::error::Error> {
        let Some((web, idx, meta)) = Self::web_session_keys(sid) else {
            return Ok(None);
        };
        let Some(uid) = Self::web_session_owner(sid) else {
            return Ok(None);
        };
        // Verify mapping + membership and slide all three TTLs atomically with
        // revocation, so a replaced or revoked session cannot be authorized.
        let script = r"
          if redis.call('GET',KEYS[1]) ~= ARGV[2] then return 0 end
          if redis.call('SISMEMBER',KEYS[2],ARGV[1]) ~= 1 then return 0 end
          redis.call('EXPIRE',KEYS[1],ARGV[3])
          redis.call('EXPIRE',KEYS[2],ARGV[3])
          redis.call('EXPIRE',KEYS[3],ARGV[3])
          return 1
        ";
        let valid: i64 = self
            .client
            .eval(
                script,
                vec![web, idx, meta],
                vec![
                    sid.to_owned(),
                    uid.to_string(),
                    WEB_SESSION_TTL_SECS.to_string(),
                ],
            )
            .await?;
        Ok((valid == 1).then_some(uid))
    }

    pub async fn web_session_del(&self, sid: &str) {
        let Some((web, idx, meta)) = Self::web_session_keys(sid) else {
            return;
        };
        let script = r"
          redis.call('SREM',KEYS[2],ARGV[1])
          return redis.call('DEL',KEYS[1],KEYS[3])
        ";
        let _: Result<i64, _> = self
            .client
            .eval(script, vec![web, idx, meta], vec![sid.to_owned()])
            .await;
    }

    /// 会话属主。`Err` = Redis 读失败：调用方不能把它当成「会话已失效」。
    async fn web_session_get_raw(&self, sid: &str) -> Result<Option<i64>, fred::error::Error> {
        let Some((web, _, _)) = Self::web_session_keys(sid) else {
            return Ok(None);
        };
        let value: Option<String> = self.client.get(web).await?;
        Ok(value.and_then(|value| value.parse().ok()))
    }

    /// 该用户仍有效的 web 会话（过期成员顺手从索引摘掉）。
    pub async fn web_session_list(&self, user_id: i64) -> Vec<WebSessionRow> {
        let idx = format!("sess:{{{user_id}}}:idx");
        let members: Vec<String> = self.client.smembers(&idx).await.unwrap_or_default();
        let mut out = Vec::with_capacity(members.len());
        for sid in members {
            let meta_key = Self::web_session_keys(&sid).map(|(_, _, meta)| meta);
            let owner = match self.web_session_get_raw(&sid).await {
                Ok(owner) => owner,
                // 读失败不等于会话已死：把它从索引里摘掉，`web_session_check` 的成员资格检查
                // 就再也过不了，还活着的会话等于被永久登出。这次先不列它
                Err(error) => {
                    tracing::warn!(%error, user_id, "读取 web 会话失败，本次不列出也不清理");
                    continue;
                }
            };
            match (owner, meta_key) {
                (Some(uid), Some(meta_key)) if uid == user_id => {
                    let meta: std::collections::HashMap<String, String> =
                        self.client.hgetall(meta_key).await.unwrap_or_default();
                    let created_at = meta
                        .get("created_at")
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0);
                    out.push(WebSessionRow {
                        sid,
                        ip: meta.get("ip").filter(|s| !s.is_empty()).cloned(),
                        ua: meta.get("ua").filter(|s| !s.is_empty()).cloned(),
                        created_at,
                        // 旧会话没有毫秒字段：按秒补齐（同秒并列时靠 keep 钉住新会话即可）
                        created_ms: meta
                            .get("created_ms")
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or(created_at.saturating_mul(1000)),
                    });
                }
                _ => {
                    let _: Result<i64, _> = self.client.srem(&idx, sid.as_str()).await;
                }
            }
        }
        out.sort_by_key(|row| std::cmp::Reverse(row.created_ms));
        out
    }

    /// 会话数上限裁剪（§11.37）：按建立时刻（毫秒）降序保留前 `limit` 条，其余删掉；
    /// `keep` 是刚建立的那条，钉在首位永不被踢（毫秒也可能并列，不靠排序保证）。
    /// 返回被踢的 sid 数；`limit <= 0` = 不限。
    pub async fn web_session_trim(&self, user_id: i64, limit: i64, keep: &str) -> usize {
        let Ok(limit) = usize::try_from(limit) else {
            return 0;
        };
        if limit == 0 {
            return 0;
        }
        let mut rows = self.web_session_list(user_id).await;
        if let Some(pos) = rows.iter().position(|r| r.sid == keep) {
            let current = rows.remove(pos);
            rows.insert(0, current);
        }
        let evicted: Vec<WebSessionRow> = rows.drain(limit.min(rows.len())..).collect();
        for row in &evicted {
            self.web_session_del(&row.sid).await;
        }
        evicted.len()
    }

    /// 吊销一条：必须属于该用户。
    pub async fn web_session_revoke(&self, user_id: i64, sid: &str) -> bool {
        match self.web_session_get_raw(sid).await {
            Ok(Some(uid)) if uid == user_id => {
                self.web_session_del(sid).await;
                true
            }
            Ok(_) => false,
            Err(error) => {
                tracing::warn!(%error, user_id, "读取 web 会话失败，无法吊销");
                false
            }
        }
    }

    /// 清空该用户全部 web 会话（密码重置 / 封禁 / 删除）。脚本里拼出的会话键与 KEYS[1]
    /// 同一个 `{user_id}` tag，Cluster 下仍在同槽。
    pub async fn web_session_revoke_user(&self, user_id: i64) {
        let script = r"
          local members=redis.call('SMEMBERS',KEYS[1])
          for _,sid in ipairs(members) do
            redis.call('DEL',ARGV[1]..'web:'..sid,ARGV[1]..'meta:'..sid)
          end
          redis.call('DEL',KEYS[1])
          return #members
        ";
        let result: Result<i64, _> = self
            .client
            .eval(
                script,
                vec![format!("sess:{{{user_id}}}:idx")],
                vec![format!("sess:{{{user_id}}}:")],
            )
            .await;
        if let Err(error) = result {
            tracing::error!(%error,user_id,"session revocation unavailable");
        }
    }
}
