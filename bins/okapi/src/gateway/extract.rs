//! 全二进制共用的 `Query<T>` 提取器。
//!
//! 为什么不用 `axum::extract::Query`：它解析失败时回 `text/plain` 400，正文是
//! `Failed to deserialize query string: ...` 这样的英文句子——违反"后端错误只回 error_code"
//! 的 i18n 定案（00-project 红线 4）。本模块的 `Query` 语义完全相同，只是拒绝时走 `AppError`，
//! 与其他 400 一样是 `{"error":{"code":"bad_request","param":"query"}}`。
//! 具体哪个字段解析失败只进 debug 日志，不进响应体。
//!
//! 放在 gateway 层是因为 gateway（`/v1/realtime?model=`）与 console 都要用，而 console
//! 已依赖 gateway 的 `AppError` / `AppState`；反向放 console 会让 gateway 依赖 console。

use super::error::AppError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

/// `axum::extract::Query` 的替身：`Query(q): Query<T>` 用法不变，拒绝回 `AppError`。
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::try_from_uri(&parts.uri)
            .map(|axum::extract::Query(inner)| Self(inner))
            .map_err(|err| {
                tracing::debug!(error = %err, path = %parts.uri.path(), "bad query string");
                AppError::bad_request().with_param("query")
            })
    }
}
