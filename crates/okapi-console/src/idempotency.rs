//! 管理写操作的 `Idempotency-Key` 请求头（订阅发放、加款 / 扣款共用）。
//!
//! 键按调用者区分（`<调用者>:<sha256(键)>`）：不同管理员碰巧用了同一个键也互不干扰，库里也不存原文。
use axum::http::{HeaderMap, StatusCode};
use okapi_api::codes;
use okapi_kernel::error::AppError;
use sha2::{Digest, Sha256};

/// 请求带了 `Idempotency-Key` 就返回 `<actor_tag>:<sha256 十六进制>`；没带返回 `None`。
/// 键须是 1–128 个可见 ASCII 字符，否则 400（`param = idempotency_key`）。
///
/// # Errors
/// 键不是合法的可见 ASCII，或为空 / 超长。
pub(crate) fn source(headers: &HeaderMap, actor_tag: &str) -> Result<Option<String>, AppError> {
    let Some(key) = headers.get("Idempotency-Key") else {
        return Ok(None);
    };
    let invalid =
        || AppError::new(StatusCode::BAD_REQUEST, codes::BAD_REQUEST).with_param("idempotency_key");
    let key = key.to_str().map_err(|_| invalid())?;
    if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(invalid());
    }
    Ok(Some(of(actor_tag, key)))
}

/// 调用方自己给的键（MCP 工具参数）按同一规则成形。
pub(crate) fn of(actor_tag: &str, key: &str) -> String {
    format!(
        "{actor_tag}:{}",
        hex::encode(Sha256::digest(key.as_bytes()))
    )
}
