//! 渠道凭证落库信封（AES-256-GCM，主密钥 `OKAPI_MASTER_KEY`，32 字节 hex）。
//!
//! 落库布局：`MAGIC(4B) || nonce(12B) || ciphertext`。
//!
//! **前缀而非"试解密再回退"**：上游 key 就是一串可打印 ASCII，与密文在字节层面
//! 无从区分，试解密失败时分不清"这是明文"还是"主密钥配错了"——前者该放行，
//! 后者必须炸。有了 MAGIC，两种情形在读取时是两条确定的分支。
//!
//! 升级不需要停机：无前缀的行按历史明文读，写入时才封。存量明文会一直是明文，
//! 直到被凭证轮换重写——想一次性收口用 `okapi seal-credentials`。

use crate::StoreError;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};

/// 信封版本前缀（okapi credential v1）。换算法/换密钥派生方式时递增。
const MAGIC: &[u8; 4] = b"okc1";
const NONCE_LEN: usize = 12;

/// 主密钥 hex → cipher。TOTP 信封（`identity`）共用同一把主密钥与同一套校验。
pub(crate) fn master_cipher(master_key_hex: &str) -> Result<Aes256Gcm, StoreError> {
    let bytes = hex::decode(master_key_hex.trim())
        .map_err(|_| StoreError::InvalidData("master_key_not_hex"))?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StoreError::InvalidData("master_key_must_be_32_bytes"))?;
    Ok(Aes256Gcm::new(&Key::<Aes256Gcm>::from(key)))
}

/// 该字节串是否为本模块封装的密文。
#[must_use]
pub fn is_sealed(stored: &[u8]) -> bool {
    stored.len() > MAGIC.len() + NONCE_LEN && stored.starts_with(MAGIC)
}

/// 封装一条凭证。
pub fn seal(master_key_hex: &str, plaintext: &str) -> Result<Vec<u8>, StoreError> {
    let cipher = master_cipher(master_key_hex)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| StoreError::InvalidData("credential_seal_failed"))?;
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend(ct);
    Ok(out)
}

/// 落库形态：配了主密钥就封，没配就退回明文。
///
/// 没配主密钥仍放行（而不是拒绝建渠道）是刻意的：加密是本次补上的能力，
/// 不该让既有部署一升级就建不了渠道。缺密钥的告警在 `warn_if_unprotected`。
pub fn seal_or_plain(master_key_hex: Option<&str>, plaintext: &str) -> Result<Vec<u8>, StoreError> {
    match master_key_hex {
        Some(key) => seal(key, plaintext),
        None => Ok(plaintext.as_bytes().to_vec()),
    }
}

/// 读出一条凭证：有 MAGIC 走解密（缺主密钥即错），无 MAGIC 按历史明文读。
pub fn open(master_key_hex: Option<&str>, stored: &[u8]) -> Result<String, StoreError> {
    if !is_sealed(stored) {
        return String::from_utf8(stored.to_vec())
            .map_err(|_| StoreError::InvalidData("channel_key credential not utf-8"));
    }
    let key = master_key_hex.ok_or(StoreError::InvalidData(
        "credential_sealed_but_no_master_key",
    ))?;
    let cipher = master_cipher(key)?;
    let body = &stored[MAGIC.len()..];
    let (nonce, ct) = body.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| StoreError::InvalidData("credential_ciphertext_too_short"))?;
    let plain = cipher
        .decrypt(&Nonce::from(nonce), ct)
        .map_err(|_| StoreError::InvalidData("credential_open_failed"))?;
    String::from_utf8(plain)
        .map_err(|_| StoreError::InvalidData("channel_key credential not utf-8"))
}

/// 未配主密钥时提示一次：凭证会明文落库。
pub fn warn_if_unprotected(master_key_hex: Option<&str>) {
    if master_key_hex.is_none() {
        tracing::warn!("未配置 OKAPI_MASTER_KEY，渠道凭证将明文落库");
    }
}

/// `seal_existing` 的统计。
#[derive(Debug, Default)]
pub struct SealStats {
    /// 本次新封的明文行。
    pub sealed: u64,
    /// 已是密文、跳过的行。
    pub already_sealed: u64,
    /// 非 UTF-8、封不了的行（channel_key id）。
    pub unreadable: Vec<i64>,
}

/// 把存量明文凭证一次性封起来（`okapi seal-credentials`）。
///
/// 幂等：已带 MAGIC 的行跳过，重复跑不会二次封装。逐行独立更新——一行坏数据
/// 不该挡住其余行收口，坏行 id 汇总返回给运维。
pub async fn seal_existing(
    pool: &sqlx::PgPool,
    master_key_hex: &str,
) -> Result<SealStats, StoreError> {
    let rows = sqlx::query!(r#"SELECT id, credential_ciphertext FROM channel_keys ORDER BY id"#)
        .fetch_all(pool)
        .await?;
    let mut stats = SealStats::default();
    for row in rows {
        if is_sealed(&row.credential_ciphertext) {
            stats.already_sealed += 1;
            continue;
        }
        let Ok(plain) = std::str::from_utf8(&row.credential_ciphertext) else {
            stats.unreadable.push(row.id);
            continue;
        };
        let sealed = seal(master_key_hex, plain)?;
        sqlx::query!(
            r#"UPDATE channel_keys SET credential_ciphertext = $2, updated_at = now() WHERE id = $1"#,
            row.id,
            sealed
        )
        .execute(pool)
        .await?;
        stats.sealed += 1;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        hex::encode([7u8; 32])
    }

    #[test]
    fn seal_open_roundtrip() {
        let sealed = seal(&key(), "sk-upstream-secret").unwrap();
        assert!(is_sealed(&sealed));
        assert_eq!(open(Some(&key()), &sealed).unwrap(), "sk-upstream-secret");
    }

    #[test]
    fn sealed_bytes_do_not_leak_plaintext() {
        let sealed = seal(&key(), "sk-upstream-secret").unwrap();
        assert!(
            !sealed
                .windows(6)
                .any(|w| w == b"sk-upstream-secret"[..6].as_ref()),
            "密文里不该出现明文片段"
        );
    }

    #[test]
    fn legacy_plaintext_row_still_reads() {
        // 升级前落库的明文行：无主密钥、有主密钥两种情形都必须能读出来
        let legacy = b"sk-legacy-plaintext".to_vec();
        assert_eq!(open(None, &legacy).unwrap(), "sk-legacy-plaintext");
        assert_eq!(open(Some(&key()), &legacy).unwrap(), "sk-legacy-plaintext");
    }

    #[test]
    fn sealed_row_without_master_key_is_an_error_not_garbage() {
        // 丢了主密钥要炸得明确，绝不能把密文当明文发给上游
        let sealed = seal(&key(), "sk-upstream-secret").unwrap();
        assert!(open(None, &sealed).is_err());
    }

    #[test]
    fn wrong_master_key_fails_closed() {
        let sealed = seal(&key(), "sk-upstream-secret").unwrap();
        assert!(open(Some(&hex::encode([9u8; 32])), &sealed).is_err());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let mut sealed = seal(&key(), "sk-upstream-secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(
            open(Some(&key()), &sealed).is_err(),
            "GCM 认证标签应拒绝篡改"
        );
    }

    #[test]
    fn seal_or_plain_follows_master_key_presence() {
        assert!(is_sealed(&seal_or_plain(Some(&key()), "sk-x").unwrap()));
        assert_eq!(seal_or_plain(None, "sk-x").unwrap(), b"sk-x".to_vec());
    }

    #[test]
    fn nonce_is_fresh_per_seal() {
        let a = seal(&key(), "sk-x").unwrap();
        let b = seal(&key(), "sk-x").unwrap();
        assert_ne!(a, b, "同明文两次封装必须不同（nonce 复用会毁掉 GCM）");
    }

    #[test]
    fn rejects_bad_master_key() {
        assert!(seal("not-hex", "sk-x").is_err());
        assert!(
            seal(&hex::encode([1u8; 16]), "sk-x").is_err(),
            "长度须 32 字节"
        );
    }

    #[test]
    fn short_prefixed_value_is_treated_as_plaintext() {
        // 恰好以 okc1 开头但长度不足信封的历史明文，不该被当密文
        assert_eq!(open(Some(&key()), b"okc1").unwrap(), "okc1");
    }
}
