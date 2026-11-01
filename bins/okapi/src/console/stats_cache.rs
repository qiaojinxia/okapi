//! Opt-in dashboard query cache. Guards run before this helper; errors aren't cached.
//! 缓存本体挂在 `AppState` 上，类型与构造在 `gateway::state`（网关构造状态时不引用 console）。
#[cfg(test)]
use crate::gateway::state::build_query_cache as build;
use crate::gateway::state::{AppState, QueryCache};
use crate::kernel::error::AppError;
use axum::http::HeaderMap;
use serde_json::Value;
use std::{future::Future, sync::Arc};

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
            retry_after_secs: err.retry_after_secs,
        })
}

/// 规划成本高的大 SQL（完整分析源、测量覆盖）：ClickHouse 24.8 光分析规划就要单核 ~2s，
/// 与读多少行无关。一次刷新并发 6 条时，4 核机器上每条都拖过 15s 的执行上限被掐断。
/// 进 ClickHouse 前在这里排队，排队不计入它的执行时限，几条按序各自 ~2s 跑完。
const HEAVY_SQL_BYTES: usize = 16 * 1024;
static HEAVY_QUERIES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

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
        let _permit = if sql.len() >= HEAVY_SQL_BYTES {
            HEAVY_QUERIES.acquire().await.ok()
        } else {
            None
        };
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
