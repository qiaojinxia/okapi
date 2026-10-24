//! 刷新得到、却没能落库的新 OAuth 凭据。
//!
//! 上游已经轮换了 refresh token（Codex 等强制单次轮换），这份新凭据是唯一一份：丢了就只能人工重登。
//! 所以存两处：Redis（各副本都看得到——别的副本拿到刷新锁后会先补写它，而不是拿库里已作废的
//! 旧 refresh token 去打上游、撞 invalid_grant 把账号判死），和本进程内存（Redis 同时不可用时兜底）。
//! 两处都只在库里的值仍是当初要比对的旧值时才算数，库里换过值（别处写过、已置失效、key 删了）
//! 即作废；都带 TTL，取不走的也不会一直留着。Redis 里那份按主密钥封装，和库里的凭据同一保护级别
//! （没配主密钥的部署两边都是明文）。
use super::super::state::AppState;
use base64::Engine;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;
use okapi_store::credential::OAuthCredential;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 待补写的凭据留多久。够 PG 从故障里回来，又不至于让过时的 token 长期留在 Redis / 内存里。
const TTL: Duration = Duration::from_hours(24);

fn redis_key(channel_key_id: i64) -> String {
    format!("oauth:pending:{channel_key_id}")
}

fn fingerprint(stored: &[u8]) -> String {
    hex::encode(Sha256::digest(stored))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    /// 落库要比对的旧值（库里密文）的 SHA-256。
    expected: String,
    /// 新凭据明文按主密钥封装后 base64。
    sealed: String,
}

/// 本进程内的兜底：key → (写入时刻, 旧值指纹, 新凭据)。
#[derive(Default)]
pub struct Memory {
    entries: std::sync::Mutex<HashMap<i64, (Instant, String, OAuthCredential)>>,
}

impl Memory {
    fn put(&self, key: i64, expected: String, next: OAuthCredential) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.retain(|_, (at, ..)| at.elapsed() < TTL);
            entries.insert(key, (Instant::now(), expected, next));
        }
    }

    fn get(&self, key: i64, current: &str) -> Option<OAuthCredential> {
        let mut entries = self.entries.lock().ok()?;
        let (at, expected, next) = entries.get(&key)?;
        if at.elapsed() >= TTL || expected != current {
            entries.remove(&key);
            return None;
        }
        Some(next.clone())
    }

    fn clear(&self, key: i64) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(&key);
        }
    }
}

/// 记下没能落库的新凭据。`expected` 是这次落库要比对的旧值（库里的原始字节）。
pub async fn put(state: &AppState, key: i64, expected: &[u8], next: &OAuthCredential) {
    let expected = fingerprint(expected);
    state
        .refresh_gate
        .pending
        .put(key, expected.clone(), next.clone());
    let sealed = match okapi_store::credential::seal_or_plain(
        state.master_key.as_deref(),
        &next.to_plaintext(),
    ) {
        Ok(sealed) => sealed,
        Err(error) => {
            tracing::error!(key, %error, "待补写的 OAuth 凭据封装失败，只留在本进程");
            return;
        }
    };
    let Ok(payload) = serde_json::to_string(&Stored {
        expected,
        sealed: base64::engine::general_purpose::STANDARD.encode(sealed),
    }) else {
        return;
    };
    let result: Result<(), _> = state
        .sched
        .client()
        .set(
            redis_key(key),
            payload,
            Some(Expiration::EX(
                i64::try_from(TTL.as_secs()).unwrap_or(i64::MAX),
            )),
            None,
            false,
        )
        .await;
    if let Err(error) = result {
        tracing::error!(key, %error, "待补写的 OAuth 凭据写不进 Redis，只留在本进程，别的副本看不到");
    }
}

/// 取待补写的新凭据：库里当前值（`current`）还是当初要比对的旧值才返回；否则这份已作废，顺手清掉。
/// 不在这里删：补写成功后由调用方 [`clear`]，再失败就还留着给下一次。
///
/// `Err` = 读不到 / 读出来是坏的，**不能**当成「没有待补写」：那样调用方会拿库里已作废的旧
/// refresh token 去刷，正好撞上这个模块要防的 invalid_grant 判死。
pub async fn take(
    state: &AppState,
    key: i64,
    current: &[u8],
) -> Result<Option<OAuthCredential>, &'static str> {
    let current = fingerprint(current);
    if let Some(next) = state.refresh_gate.pending.get(key, &current) {
        return Ok(Some(next));
    }
    let raw: Option<String> = state
        .sched
        .client()
        .get(redis_key(key))
        .await
        .map_err(|_| "oauth_pending_unavailable")?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let stored: Stored = serde_json::from_str(&raw).map_err(|_| "oauth_pending_unreadable")?;
    if stored.expected != current {
        clear(state, key).await;
        return Ok(None);
    }
    let sealed = base64::engine::general_purpose::STANDARD
        .decode(stored.sealed)
        .map_err(|_| "oauth_pending_unreadable")?;
    let plaintext = okapi_store::credential::open(state.master_key.as_deref(), &sealed)
        .map_err(|_| "oauth_pending_unreadable")?;
    OAuthCredential::parse(&plaintext)
        .map(Some)
        .ok_or("oauth_pending_unreadable")
}

/// 补写成功、或库里的 key 已失效 / 被换掉：两处一起清。
pub async fn clear(state: &AppState, key: i64) {
    state.refresh_gate.pending.clear(key);
    let _: Result<i64, _> = state.sched.client().del(redis_key(key)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(token: &str) -> OAuthCredential {
        OAuthCredential {
            access_token: token.to_owned(),
            refresh_token: "r".to_owned(),
            expires_at: i64::MAX,
            account_id: None,
            account_label: None,
            scope: None,
        }
    }

    #[test]
    fn memory_entries_only_count_against_the_value_they_replace() {
        let memory = Memory::default();
        let old = fingerprint(b"old");
        memory.put(7, old.clone(), credential("new"));
        assert!(memory.get(8, &old).is_none());
        assert_eq!(memory.get(7, &old).unwrap().access_token, "new");
        assert!(memory.get(7, &old).is_some(), "补写成功前一直在");
        assert!(
            memory.get(7, &fingerprint(b"written elsewhere")).is_none(),
            "库里已换过值：这份过时，丢掉"
        );
        assert!(memory.get(7, &old).is_none());
        memory.put(7, old.clone(), credential("new"));
        memory.clear(7);
        assert!(memory.get(7, &old).is_none());
    }
}
