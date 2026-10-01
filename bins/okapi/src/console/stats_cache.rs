//! Opt-in dashboard query cache. Guards run before this helper; errors aren't cached.
use crate::gateway::{error::AppError, state::AppState};
use axum::http::HeaderMap;
use moka::future::Cache;
use serde_json::Value;
use std::{future::Future, sync::Arc, time::Duration};

pub(crate) type QueryCache = Cache<String, Arc<Vec<Value>>>;

pub(crate) fn build() -> QueryCache {
    Cache::builder()
        .max_capacity(16 * 1024 * 1024)
        .weigher(|key: &String, rows: &Arc<Vec<Value>>| {
            let bytes = serde_json::to_vec(rows.as_ref()).map_or(usize::MAX, |v| v.len());
            u32::try_from(key.len().saturating_add(bytes)).unwrap_or(u32::MAX)
        })
        .time_to_live(Duration::from_secs(15))
        .build()
}

pub(super) fn allowed(headers: &HeaderMap) -> bool {
    !headers
        .get(axum::http::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|part| {
                matches!(
                    part.trim().to_ascii_lowercase().as_str(),
                    "no-cache" | "no-store" | "max-age=0"
                )
            })
        })
}

fn key(sql: &str, params: &[(&str, &str)]) -> String {
    serde_json::to_string(&(sql, params)).expect("string tuple is JSON serializable")
}

async fn load(
    cache: &QueryCache,
    key: String,
    fetch: impl Future<Output = Result<Vec<Value>, AppError>>,
) -> Result<Vec<Value>, AppError> {
    cache
        .try_get_with(key, async { fetch.await.map(Arc::new) })
        .await
        .map(|rows| (*rows).clone())
        .map_err(|err| AppError {
            status: err.status,
            code: err.code.clone(),
            param: err.param.clone(),
        })
}

pub(super) async fn query(
    state: &AppState,
    sql: &str,
    params: &[(&str, &str)],
    cached: bool,
) -> Result<Vec<Value>, AppError> {
    let ch = state.ch.as_ref().ok_or_else(|| {
        AppError::new(
            axum::http::StatusCode::NOT_IMPLEMENTED,
            okapi_api::codes::STATS_DISABLED,
        )
    })?;
    let fetch = async {
        ch.query_with_params(sql, params)
            .await
            .map_err(AppError::from)
    };
    if cached {
        load(&state.stats_query_cache, key(sql, params), fetch).await
    } else {
        // Explicit refresh must also discard an older dashboard entry so navigating
        // back immediately cannot resurrect data that the user just refreshed.
        state.stats_query_cache.invalidate(&key(sql, params)).await;
        fetch.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn identical_in_flight_queries_are_coalesced() {
        let cache = build();
        let calls = AtomicUsize::new(0);
        let request = || {
            load(&cache, "same".to_owned(), async {
                calls.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
                Ok(vec![json!({"requests": 12})])
            })
        };
        let (a, b, c) = tokio::join!(request(), request(), request());
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(c.unwrap(), vec![json!({"requests": 12})]);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn failures_are_not_retained() {
        let cache = build();
        assert!(
            load(&cache, "failed".to_owned(), async {
                Err(AppError::internal())
            })
            .await
            .is_err()
        );
        assert_eq!(
            load(&cache, "failed".to_owned(), async { Ok(vec![json!(1)]) })
                .await
                .unwrap(),
            vec![json!(1)]
        );
    }

    #[test]
    fn query_identity_includes_scope_parameters_and_refresh_bypasses_cache() {
        assert_ne!(
            key("SELECT {user:String}", &[("user", "1")]),
            key("SELECT {user:String}", &[("user", "2")])
        );
        let mut headers = HeaderMap::new();
        assert!(allowed(&headers));
        headers.insert("cache-control", "private, no-cache".parse().unwrap());
        assert!(!allowed(&headers));
    }
}
