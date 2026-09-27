use super::{AppError, BTreeMap, StatusCode, Value};
use hmac::{Hmac, KeyInit as _, Mac};
use serde::Deserialize;
use sha2::Sha256;

pub(super) fn gateway_error() -> AppError {
    AppError::new(StatusCode::BAD_GATEWAY, "payment_gateway_error")
}

pub(super) fn checkout_response(value: &Value) -> Result<(&str, &str), AppError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_identity(id, 128) && id.starts_with("cs_"))
        .ok_or_else(gateway_error)?;
    let url = value
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(gateway_error)?;
    let parsed = reqwest::Url::parse(url).map_err(|_| gateway_error())?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(gateway_error());
    }
    Ok((id, url))
}

fn valid_identity(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.chars().any(char::is_whitespace)
        && !value.chars().any(char::is_control)
}

pub(super) fn required<'a>(
    params: &'a BTreeMap<String, String>,
    name: &str,
    max: usize,
) -> Result<&'a str, AppError> {
    params
        .get(name)
        .map(String::as_str)
        .filter(|v| valid_identity(v, max))
        .ok_or_else(|| AppError::bad_request().with_param(name))
}

pub(super) fn epay_params(query: &str) -> Result<BTreeMap<String, String>, AppError> {
    if query.len() > 16_384 || query.split('&').count() > 64 {
        return Err(AppError::bad_request().with_param("query"));
    }
    // URL's form decoder is intentionally forgiving. Reject broken escapes
    // first rather than signing a repaired representation of the input.
    let bytes = query.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !bytes
                .get(i + 1..i + 3)
                .is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(AppError::bad_request().with_param("query"));
        }
    }
    let mut url =
        reqwest::Url::parse("https://callback.invalid").map_err(|_| AppError::internal())?;
    url.set_query(Some(query));
    let mut params = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if key.is_empty()
            || key.contains('\u{fffd}')
            || value.contains('\u{fffd}')
            || params
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(AppError::bad_request().with_param("query"));
        }
    }
    Ok(params)
}

pub(super) fn epay_signature(params: &BTreeMap<String, String>, key: &str) -> Result<(), AppError> {
    if params.get("sign_type").is_some_and(|v| v != "MD5") {
        return Err(AppError::bad_request().with_param("sign_type"));
    }
    let sign = required(params, "sign", 32)?;
    let mut given = [0_u8; 16];
    hex::decode_to_slice(sign, &mut given)
        .map_err(|_| AppError::bad_request().with_param("sign"))?;
    let mut expected = [0_u8; 16];
    hex::decode_to_slice(super::epay_sign(params, key), &mut expected)
        .map_err(|_| AppError::internal())?;
    // Reuse the crypto library's constant-time digest comparison.
    if md5::digest::CtOutput::<md5::Md5>::new(expected.into())
        != md5::digest::CtOutput::<md5::Md5>::new(given.into())
    {
        return Err(AppError::bad_request().with_param("sign"));
    }
    Ok(())
}

pub(super) fn money_minor(value: &str) -> Result<i64, AppError> {
    let invalid = || AppError::bad_request().with_param("money");
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let major: i64 = whole.parse().map_err(|_| invalid())?;
    let minor = match fraction.len() {
        0 => 0,
        1 => i64::from(fraction.as_bytes()[0] - b'0') * 10,
        _ => fraction.parse::<i64>().map_err(|_| invalid())?,
    };
    major
        .checked_mul(100)
        .and_then(|v| v.checked_add(minor))
        .filter(|v| *v > 0)
        .ok_or_else(invalid)
}

pub(super) fn stripe_signature(header: &str, body: &[u8], secret: &str) -> Result<(), AppError> {
    let invalid = || AppError::bad_request().with_param("stripe_signature");
    if header.len() > 8192 {
        return Err(invalid());
    }
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',').map(str::trim) {
        if let Some(value) = part.strip_prefix("t=") {
            if timestamp.replace(value).is_some() {
                return Err(invalid());
            }
        } else if let Some(value) = part.strip_prefix("v1=") {
            signatures.push(value);
        }
    }
    let timestamp = timestamp.ok_or_else(invalid)?;
    let ts: i64 = timestamp.parse().map_err(|_| invalid())?;
    if chrono::Utc::now().timestamp().abs_diff(ts) > 300 {
        return Err(invalid());
    }
    let mut mac =
        <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).map_err(|_| AppError::internal())?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    for signature in signatures {
        let mut tag = [0_u8; 32];
        if hex::decode_to_slice(signature, &mut tag).is_ok()
            && mac.clone().verify_slice(&tag).is_ok()
        {
            return Ok(());
        }
    }
    Err(invalid())
}

#[derive(Deserialize)]
pub(super) struct CheckoutSession {
    pub id: String,
    object: String,
    mode: String,
    status: String,
    payment_status: String,
    pub amount_total: i64,
    pub currency: String,
    pub metadata: OrderMetadata,
}
#[derive(Deserialize)]
pub(super) struct OrderMetadata {
    pub order_no: String,
}

pub(super) fn paid_session(body: &[u8]) -> Result<Option<CheckoutSession>, AppError> {
    let event: Value = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    if !matches!(
        event.get("type").and_then(Value::as_str),
        Some("checkout.session.completed" | "checkout.session.async_payment_succeeded")
    ) {
        return Ok(None);
    }
    let session: CheckoutSession =
        serde_json::from_value(event.pointer("/data/object").cloned().unwrap_or_default())
            .map_err(|_| AppError::bad_request().with_param("session"))?;
    if session.object != "checkout.session"
        || session.mode != "payment"
        || session.status != "complete"
        || !valid_identity(&session.id, 128)
        || !valid_identity(&session.metadata.order_no, 64)
        || session.amount_total <= 0
        || session.currency != "usd"
    {
        return Err(AppError::bad_request().with_param("session"));
    }
    match session.payment_status.as_str() {
        "paid" => Ok(Some(session)),
        "unpaid" | "no_payment_required" => Ok(None),
        _ => Err(AppError::bad_request().with_param("payment_status")),
    }
}
