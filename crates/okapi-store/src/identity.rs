//! 身份体系（IMPLEMENTATION §6.4）：邮箱密码（argon2id）+ TOTP（RFC 6238）。
//! TOTP 密钥 AES-256-GCM 信封加密落库（主密钥 `OKAPI_MASTER_KEY`，32 字节 hex）；
//! web 会话存 Redis（console 层），本模块只管凭证与密钥学。

use crate::StoreError;
use crate::credential::master_cipher;
use aes_gcm::aead::{Aead, AeadCore, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng as HashOsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use hmac::{Hmac, KeyInit as _, Mac};
use sha1::Sha1;
use sqlx::PgPool;

// ---- 密码 ----

/// argon2id 散列（缺省参数：RustCrypto 推荐档）。
pub fn hash_password(password: &str) -> Result<String, StoreError> {
    let salt = SaltString::generate(&mut HashOsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| StoreError::InvalidData("password_hash_failed"))
}

/// 校验密码。除本系统的 argon2id 外，兼容 bcrypt（`$2a$/$2b$/$2y$` 前缀）——
/// 老 ok-api 迁移用户免重置密码；新设/改密仍一律 argon2id。
#[must_use]
pub fn verify_password(password: &str, hash: &str) -> bool {
    if hash.starts_with("$2") {
        return bcrypt::verify(password, hash).unwrap_or(false);
    }
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// 注册（email 唯一冲突返回 None）。
pub async fn register_user(
    pool: &PgPool,
    email: &str,
    username: &str,
    password: &str,
) -> Result<Option<i64>, StoreError> {
    let hash = hash_password(password)?;
    let row = sqlx::query_scalar!(
        r#"
        INSERT INTO users (email, username, password_hash)
        VALUES ($1, $2, $3)
        ON CONFLICT (email) DO NOTHING
        RETURNING id
        "#,
        email,
        username,
        hash
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// 密码登录主体（含 TOTP 状态）。
pub struct LoginUser {
    pub user_id: i64,
    pub role: i16,
    pub totp_enabled: bool,
    pub totp_secret_ciphertext: Option<Vec<u8>>,
}

/// 按 email 取登录主体；密码不匹配/被禁用返回 None。
pub async fn find_login_user(
    pool: &PgPool,
    email: &str,
    password: &str,
) -> Result<Option<LoginUser>, StoreError> {
    let row = sqlx::query!(
        r#"SELECT id, role, password_hash, totp_secret_ciphertext
           FROM users WHERE email = $1 AND status = 1 AND deleted_at IS NULL"#,
        email
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let Some(hash) = row.password_hash else {
        return Ok(None); // OAuth-only 账户无密码
    };
    if !verify_password(password, &hash) {
        return Ok(None);
    }
    Ok(Some(LoginUser {
        user_id: row.id,
        role: row.role,
        totp_enabled: row.totp_secret_ciphertext.is_some(),
        totp_secret_ciphertext: row.totp_secret_ciphertext,
    }))
}

/// OAuth 首登注册或复用绑定（(provider, subject) 唯一）：返回 user_id。
/// username 冲突时追加随机后缀重试一次。
pub async fn link_oauth_user(
    pool: &PgPool,
    provider: &str,
    subject: &str,
    display: &str,
) -> Result<i64, StoreError> {
    if let Some(user_id) = sqlx::query_scalar!(
        r#"SELECT user_id FROM oauth_identities WHERE provider = $1 AND subject = $2"#,
        provider,
        subject
    )
    .fetch_optional(pool)
    .await?
    {
        return Ok(user_id);
    }

    let mut tx = pool.begin().await?;
    let base_name: String = format!("{provider}-{display}").chars().take(56).collect();
    let attempt = sqlx::query_scalar!(
        r#"INSERT INTO users (username) VALUES ($1)
           ON CONFLICT (username) DO NOTHING RETURNING id"#,
        base_name
    )
    .fetch_optional(&mut *tx)
    .await?;
    let user_id = if let Some(id) = attempt {
        id
    } else {
        let salted = format!(
            "{base_name}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        );
        sqlx::query_scalar!(
            r#"INSERT INTO users (username) VALUES ($1) RETURNING id"#,
            salted
        )
        .fetch_one(&mut *tx)
        .await?
    };
    // 并发首登竞争：唯一约束吸收，冲突方读回胜者
    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO oauth_identities (provider, subject, user_id, display)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (provider, subject) DO NOTHING RETURNING user_id"#,
        provider,
        subject,
        user_id,
        display
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    if let Some(id) = inserted {
        return Ok(id);
    }
    let winner = sqlx::query_scalar!(
        r#"SELECT user_id FROM oauth_identities WHERE provider = $1 AND subject = $2"#,
        provider,
        subject
    )
    .fetch_one(pool)
    .await?;
    Ok(winner)
}

// ---- TOTP（RFC 6238，HMAC-SHA1，30s 步长，6 位）----

const TOTP_STEP_SECS: i64 = 30;

/// 生成随机 TOTP 密钥（20 字节）与 otpauth URL（base32 编码）。
#[must_use]
pub fn generate_totp_secret(account: &str) -> (Vec<u8>, String) {
    use aes_gcm::aead::rand_core::RngCore;
    let mut secret = vec![0u8; 20];
    OsRng.fill_bytes(&mut secret);
    let encoded = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, &secret);
    let url = format!("otpauth://totp/Okapi:{account}?secret={encoded}&issuer=Okapi");
    (secret, url)
}

/// 校验 6 位码（±1 窗容忍时钟偏移）。
#[must_use]
pub fn verify_totp(secret: &[u8], code: &str, now_unix: i64) -> bool {
    let Ok(code_num) = code.trim().parse::<u32>() else {
        return false;
    };
    let counter = now_unix / TOTP_STEP_SECS;
    [-1, 0, 1].iter().any(|offset| {
        let c = counter + offset;
        c >= 0 && totp_at(secret, u64::try_from(c).unwrap_or(0)) == code_num
    })
}

fn totp_at(secret: &[u8], counter: u64) -> u32 {
    let mut mac = <Hmac<Sha1>>::new_from_slice(secret)
        .unwrap_or_else(|_| <Hmac<Sha1>>::new_from_slice(&[0u8; 20]).expect("fixed len"));
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[19] & 0x0f);
    let bin = (u32::from(digest[offset]) & 0x7f) << 24
        | u32::from(digest[offset + 1]) << 16
        | u32::from(digest[offset + 2]) << 8
        | u32::from(digest[offset + 3]);
    bin % 1_000_000
}

// ---- TOTP 密钥信封加密 ----

/// AES-256-GCM 加密（nonce 12B 前置拼接存储）。
pub fn seal_totp_secret(master_key_hex: &str, secret: &[u8]) -> Result<Vec<u8>, StoreError> {
    let cipher = master_cipher(master_key_hex)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut out = nonce.to_vec();
    let ct = cipher
        .encrypt(&nonce, secret)
        .map_err(|_| StoreError::InvalidData("totp_seal_failed"))?;
    out.extend(ct);
    Ok(out)
}

pub fn open_totp_secret(master_key_hex: &str, sealed: &[u8]) -> Result<Vec<u8>, StoreError> {
    if sealed.len() < 12 {
        return Err(StoreError::InvalidData("totp_ciphertext_too_short"));
    }
    let cipher = master_cipher(master_key_hex)?;
    let (nonce, ct) = sealed.split_at(12);
    let nonce: [u8; 12] = nonce
        .try_into()
        .map_err(|_| StoreError::InvalidData("totp_ciphertext_too_short"))?;
    cipher
        .decrypt(&Nonce::from(nonce), ct)
        .map_err(|_| StoreError::InvalidData("totp_open_failed"))
}

/// 落库启用 2FA。
pub async fn enable_totp(pool: &PgPool, user_id: i64, sealed: &[u8]) -> Result<(), StoreError> {
    sqlx::query!(
        r#"UPDATE users SET totp_secret_ciphertext = $2, updated_at = now() WHERE id = $1"#,
        user_id,
        sealed
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("hunter2!").unwrap();
        assert!(verify_password("hunter2!", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn password_bcrypt_compat() {
        // 老 ok-api（Go bcrypt.DefaultCost）迁移哈希；低 cost 仅为测试提速
        let hash = bcrypt::hash("legacy-pass", 4).unwrap();
        assert!(hash.starts_with("$2"));
        assert!(verify_password("legacy-pass", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    /// RFC 6238 附录 B 测试向量（SHA1，secret = "12345678901234567890"）。
    #[test]
    fn totp_rfc6238_vectors() {
        let secret = b"12345678901234567890";
        // T=59 → 94287082（8 位向量取后 6 位 287082）
        assert_eq!(totp_at(secret, 59 / 30), 94_287_082 % 1_000_000);
        // T=1111111109 → 07081804
        assert_eq!(totp_at(secret, 1_111_111_109 / 30), 7_081_804 % 1_000_000);
        assert!(verify_totp(secret, "287082", 59));
        assert!(verify_totp(secret, "287082", 59 + 29), "±1 窗容忍");
        assert!(!verify_totp(secret, "000000", 59));
    }

    #[test]
    fn totp_seal_roundtrip() {
        let master = hex::encode([7u8; 32]);
        let (secret, url) = generate_totp_secret("a@b.c");
        assert!(url.starts_with("otpauth://totp/Okapi:"));
        let sealed = seal_totp_secret(&master, &secret).unwrap();
        assert_ne!(sealed, secret);
        let opened = open_totp_secret(&master, &sealed).unwrap();
        assert_eq!(opened, secret);
        assert!(
            open_totp_secret(&hex::encode([8u8; 32]), &sealed).is_err(),
            "错钥必须失败"
        );
    }
}
