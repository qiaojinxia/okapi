//! 渠道账号测试（控制台「测试输出」）：把一次真实数据面请求钉在指定渠道 / 账号上。
//!
//! - 单机 / 进程内形态：钉住走 task-local，只有已校验渠道写权限的控制台中继能进入作用域；
//! - 独立 `console` → `gateway` HTTP 桥接形态：经短效（30s）HMAC-SHA256 签名内部头
//!   `x-okapi-internal-channel-pin` 透传，网关中间件先剥离外部同名头再验签并进入同一 task-local 作用域。

use crate::state::AppState;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::future::Future;

/// 独立 `console` → `gateway` 内部桥接用的签名渠道钉住头（网关入口一律先剥离再验签）。
pub const INTERNAL_PIN_HEADER: &str = "x-okapi-internal-channel-pin";
const PIN_TTL_SECS: i64 = 30;
const PIN_MAX_FUTURE_SKEW_SECS: i64 = 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPin {
    pub channel_id: i64,
    /// None = 渠道内任一可用账号（按调度顺序）。
    pub key_id: Option<i64>,
    /// 渠道所在的池：不受管理员令牌的池链限制，测哪条就查哪条。
    pub pools: Vec<String>,
}

impl ChannelPin {
    #[must_use]
    pub fn matches(&self, cand: &okapi_store::ChannelCandidate) -> bool {
        cand.channel_id == self.channel_id
            && self.key_id.is_none_or(|key| key == cand.channel_key_id)
    }
}

#[derive(Serialize, Deserialize)]
struct SignedPinPayload {
    pin: ChannelPin,
    exp: i64,
}

tokio::task_local! { static PIN: ChannelPin; }

/// 当前请求钉住的渠道；数据面正常请求恒为 None。
#[must_use]
pub fn current() -> Option<ChannelPin> {
    PIN.try_with(Clone::clone).ok()
}

pub async fn scope<F: Future>(pin: ChannelPin, future: F) -> F::Output {
    PIN.scope(pin, future).await
}

/// 内部桥接签名密钥：优先使用集群共用的 `OKAPI_MASTER_KEY`，缺省回退 `OKAPI_EDGE_KEY`。
#[must_use]
pub fn signing_secret(state: &AppState) -> Option<String> {
    if let Some(key) = state.master_key.as_deref().filter(|s| !s.trim().is_empty()) {
        return Some(key.to_owned());
    }
    std::env::var("OKAPI_EDGE_KEY")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// 将 [`ChannelPin`] 编码为带 30s 有效期的 HMAC-SHA256 签名头值。
#[must_use]
pub fn encode_signed_header(pin: &ChannelPin, secret: &str) -> String {
    encode_signed_header_at(pin, secret, chrono::Utc::now().timestamp())
}

fn encode_signed_header_at(pin: &ChannelPin, secret: &str, now_unix: i64) -> String {
    let payload = SignedPinPayload {
        pin: pin.clone(),
        exp: now_unix.saturating_add(PIN_TTL_SECS),
    };
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&json);
    let mac = hmac_sha256(secret.as_bytes(), encoded.as_bytes());
    format!("{encoded}.{}", hex::encode(mac))
}

/// 校验并解码签名后的 [`ChannelPin`] 头；篡改、签名错误或过期一律返回 `None`。
#[must_use]
pub fn decode_signed_header(raw: &str, secret: &str) -> Option<ChannelPin> {
    decode_signed_header_at(raw, secret, chrono::Utc::now().timestamp())
}

fn decode_signed_header_at(raw: &str, secret: &str, now_unix: i64) -> Option<ChannelPin> {
    let (encoded, sig_hex) = raw.trim().split_once('.')?;
    let supplied = hex::decode(sig_hex).ok()?;
    if supplied.len() != 32 {
        return None;
    }
    let expected = hmac_sha256(secret.as_bytes(), encoded.as_bytes());
    if !constant_time_eq(&supplied, &expected) {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()?;
    let payload: SignedPinPayload = serde_json::from_slice(&bytes).ok()?;
    if payload.exp < now_unix || payload.exp > now_unix.saturating_add(PIN_MAX_FUTURE_SKEW_SECS) {
        return None;
    }
    Some(payload.pin)
}

/// 网关中间件：先剥离请求中的内部钉住头，仅当签名合法且未过期时进入 [`scope`]。
pub async fn accept_internal_pin(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let header_value = req
        .headers_mut()
        .remove(INTERNAL_PIN_HEADER)
        .and_then(|v| v.to_str().ok().map(str::to_owned));
    let verified = header_value.as_deref().and_then(|raw| {
        let secret = signing_secret(&state)?;
        decode_signed_header(raw, &secret)
    });
    match verified {
        Some(pin) => scope(pin, next.run(req)).await,
        None => next.run(req).await,
    }
}

/// RFC 2104 HMAC-SHA256（复用已有 `sha2`，不引入额外 crate）。
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = Sha256::digest(key);
        k_block[..32].copy_from_slice(&hashed);
    } else {
        k_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k_block[i];
        opad[i] ^= k_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_channel_pin_round_trips_and_rejects_tampering_or_expiry() {
        let pin = ChannelPin {
            channel_id: 42,
            key_id: Some(7),
            pools: vec!["default".into(), "vip".into()],
        };
        let secret = "0123456789abcdef0123456789abcdef";
        let token = encode_signed_header_at(&pin, secret, 1_700_000_000);
        assert_eq!(
            decode_signed_header_at(&token, secret, 1_700_000_010),
            Some(pin.clone())
        );
        // Wrong secret, tampered token, expired timestamp, or future skew are rejected.
        assert!(decode_signed_header_at(&token, "wrong-secret", 1_700_000_010).is_none());
        let tampered = format!("x{token}");
        assert!(decode_signed_header_at(&tampered, secret, 1_700_000_010).is_none());
        assert!(decode_signed_header_at(&token, secret, 1_700_000_031).is_none());
        assert!(decode_signed_header_at(&token, secret, 1_699_999_900).is_none());
    }
}
