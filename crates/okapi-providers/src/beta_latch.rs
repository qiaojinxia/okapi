//! 上游拒收 `anthropic-beta` 值时的自愈，订阅路径（`oauth::anthropic_max`）与 API key 路径（`anthropic`）共用。
//!
//! 真机碰到 400「Unexpected value(s) `x` for the `anthropic-beta` header」时把点名的 beta 拉黑、之后的请求不再带
//! （二进制里的 `requestLatches.refusedBetaHeaders`）；同一个 beta 是否被接受取决于端点与组织，抓包里看不出来，
//! 所以这里也按真机的办法自愈：去掉点名的 beta 重发一次，并记住，之后一小时这个凭证一开始就不带。

use crate::error::UpstreamError;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 上游拒收的 beta，按（记录归属，beta 名）存一小时。
static REFUSED_BETAS: LazyLock<Mutex<HashMap<(String, String), Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const REFUSED_BETA_TTL: Duration = Duration::from_mins(60);

/// 用凭证的哈希做记录归属（不存凭证本身）：`prefix:` + 16 位十六进制。
pub(crate) fn secret_scope(prefix: &str, secret: &str) -> String {
    let mut hasher = DefaultHasher::new();
    secret.hash(&mut hasher);
    format!("{prefix}:{:016x}", hasher.finish())
}

/// 400 体里被点名的 beta（`Unexpected value(s) `a`, `b` for the `anthropic-beta` header`）。
pub(crate) fn refused_betas(body: &[u8]) -> Vec<String> {
    const TAIL: &str = " for the `anthropic-beta` header";
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let Some(message) = value["error"]["message"].as_str() else {
        return Vec::new();
    };
    let Some((_, rest)) = message.split_once("Unexpected value(s) ") else {
        return Vec::new();
    };
    let Some((list, _)) = rest.split_once(TAIL) else {
        return Vec::new();
    };
    list.split(',')
        .map(|name| name.trim().trim_matches('`').to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

pub(crate) fn beta_parts(beta: &str) -> impl Iterator<Item = &str> {
    beta.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

/// 这个归属当前被拉黑的 beta 先去掉（`required` 里的不动：缺了请求本身就过不了）。
pub(crate) fn drop_refused(scope: &str, beta: &str, required: &[&str]) -> String {
    let refused = REFUSED_BETAS.lock().unwrap_or_else(PoisonError::into_inner);
    beta_parts(beta)
        .filter(|part| {
            required.contains(part)
                || refused
                    .get(&(scope.to_owned(), (*part).to_owned()))
                    .is_none_or(|since| since.elapsed() >= REFUSED_BETA_TTL)
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// 400 点名了本次请求里非必备的 beta：记下并返回去掉它们的 beta 串；点名的都不在请求里（或是必备的）就是别的 400，不重发。
fn resend_without_refused(
    scope: &str,
    beta: &str,
    required: &[&str],
    body: &[u8],
) -> Option<String> {
    let named = refused_betas(body);
    let sent: Vec<&str> = beta_parts(beta).collect();
    let dropped: Vec<&String> = named
        .iter()
        .filter(|name| sent.contains(&name.as_str()) && !required.contains(&name.as_str()))
        .collect();
    if dropped.is_empty() {
        return None;
    }
    {
        let mut refused = REFUSED_BETAS.lock().unwrap_or_else(PoisonError::into_inner);
        refused.retain(|_, since| since.elapsed() < REFUSED_BETA_TTL);
        for name in &dropped {
            refused.insert((scope.to_owned(), (*name).clone()), Instant::now());
        }
    }
    tracing::warn!(
        refused = ?dropped,
        "upstream refused anthropic-beta values; resending without them and leaving them out for an hour"
    );
    Some(
        sent.into_iter()
            .filter(|part| !dropped.iter().any(|name| name.as_str() == *part))
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// 被点名拒收 beta 的 400：返回去掉它们的 beta 串，让调用方重发；只重发一次（还是 400 就是别的问题，交回上层）。
pub(crate) fn refusal_retry<T>(
    scope: &str,
    beta: &str,
    required: &[&str],
    result: &Result<T, UpstreamError>,
    retried: &mut bool,
) -> Option<String> {
    if *retried {
        return None;
    }
    let Err(UpstreamError::Status {
        status: 400, body, ..
    }) = result
    else {
        return None;
    };
    let next = resend_without_refused(scope, beta, required, body)?;
    *retried = true;
    Some(next)
}
