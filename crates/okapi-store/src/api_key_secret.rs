//! Recoverable API keys. Unlike legacy channel credentials, never accept plaintext.
//! Bind the envelope to its owner and authentication hash to reject row swaps.

use crate::{StoreError, credential::master_cipher};
use aes_gcm::aead::{Aead, AeadCore, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};

const MAGIC: &[u8; 4] = b"okk1";

pub fn seal(master: &str, user_id: i64, hash: &str, token: &str) -> Result<Vec<u8>, StoreError> {
    let cipher = master_cipher(master)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let aad = format!("okapi:api-key:v1:{user_id}:{hash}");
    let encrypted = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: token.as_bytes(),
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| StoreError::InvalidData("api_key_seal_failed"))?;
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&nonce);
    out.extend(encrypted);
    Ok(out)
}

pub fn open(master: &str, user_id: i64, hash: &str, stored: &[u8]) -> Result<String, StoreError> {
    if !stored.starts_with(MAGIC) || stored.len() < 32 {
        return Err(StoreError::InvalidData("api_key_envelope_invalid"));
    }
    let cipher = master_cipher(master)?;
    let nonce: [u8; 12] = stored[4..16]
        .try_into()
        .map_err(|_| StoreError::InvalidData("api_key_envelope_invalid"))?;
    let aad = format!("okapi:api-key:v1:{user_id}:{hash}");
    let plain = cipher
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &stored[16..],
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| StoreError::InvalidData("api_key_open_failed"))?;
    String::from_utf8(plain).map_err(|_| StoreError::InvalidData("api_key_plaintext_invalid"))
}

/// Metadata only: do not load ciphertext into the list response path.
pub async fn saved_ids(
    pg: &sqlx::PgPool,
    user_id: i64,
    ids: &[i64],
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id = $1 AND id = ANY($2) AND deleted_at IS NULL AND key_ciphertext IS NOT NULL")
        .bind(user_id).bind(ids).fetch_all(pg).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_envelope_is_random_authenticated_and_never_plaintext() {
        let master = hex::encode([7u8; 32]);
        let secret = "sk-fixture-copy-only";
        let sealed = seal(&master, 42, "hash", secret).unwrap();
        assert_eq!(open(&master, 42, "hash", &sealed).unwrap(), secret);
        assert_ne!(sealed, seal(&master, 42, "hash", secret).unwrap());
        assert!(!sealed.windows(secret.len()).any(|w| w == secret.as_bytes()));
        assert!(open(&master, 43, "hash", &sealed).is_err());
        assert!(open(&master, 42, "other-hash", &sealed).is_err());
        assert!(open(&hex::encode([8u8; 32]), 42, "hash", &sealed).is_err());
        assert!(open(&master, 42, "hash", secret.as_bytes()).is_err());
        for len in 0..32 {
            assert!(open(&master, 42, "hash", &sealed[..len]).is_err());
        }
        let mut tampered = sealed;
        tampered[20] ^= 1;
        assert!(open(&master, 42, "hash", &tampered).is_err());
    }
}
