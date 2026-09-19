//! 全二进制共用的 `Query<T>` / `Json<T>` 提取器。
//!
//! 为什么不用 `axum::extract::Query`：它解析失败时回 `text/plain` 400，正文是
//! `Failed to deserialize query string: ...` 这样的英文句子——违反"后端错误只回 error_code"
//! 的 i18n 定案（00-project 红线 4）。本模块的 `Query` 语义完全相同，只是拒绝时走 `AppError`，
//! 与其他 400 一样是 `{"error":{"code":"bad_request","param":"query"}}`。
//! 具体哪个字段解析失败只进 debug 日志，不进响应体。
//!
//! `Json<T>` 同理，而且比 `Query` 更要紧：axum 的 `Json` 拒绝时回 422 `text/plain`，
//! 正文是 `Failed to deserialize the JSON body into the target type: missing field \`email\``。
//! 两重问题——
//! 1. 同样违反"后端错误只回 error_code"（前端 `describeError` 拿不到码，只能原样显示英文）；
//! 2. **提取器跑在 handler 体之前，也就跑在 `guard()` 鉴权之前**，于是匿名调用方
//!    POST 一个 `{}` 到任意管理面端点，就能把内部请求结构体的字段名逐个问出来。
//!
//! 全路由机械探测（`tests/route_error_envelope.rs`）一次命中 42 条。
//!
//! 放在 gateway 层是因为 gateway（`/v1/realtime?model=`）与 console 都要用，而 console
//! 已依赖 gateway 的 `AppError` / `AppState`；反向放 console 会让 gateway 依赖 console。

use super::error::AppError;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

/// `axum::extract::Query` 的替身：`Query(q): Query<T>` 用法不变，拒绝回 `AppError`。
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = AppError;

    // 没有真正的 .await：直接给一个就绪的 Future（clippy 1.98 `unused_async_trait_impl`）
    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            axum::extract::Query::<T>::try_from_uri(&parts.uri)
                .map(|axum::extract::Query(inner)| Self(inner))
                .map_err(|err| {
                    tracing::debug!(error = %err, path = %parts.uri.path(), "bad query string");
                    AppError::bad_request().with_param("query")
                }),
        )
    }
}

/// `axum::Json` 的替身（**仅用于提取**；返回体继续用 `axum::Json`）。
///
/// 用法不变：`ExtractJson(req): ExtractJson<CreateChannelReq>`。区别只在拒绝时——不再回
/// `text/plain` 的英文句子，而是与其他 400 同形的 `{"error":{"code":"bad_request","param":"body"}}`。
/// 具体哪个字段解析失败只进 debug 日志：那是内部结构体的形状，不该让未鉴权的
/// 调用方逐个问出来。
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let path = req.uri().path().to_owned();
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(inner)| Self(inner))
            .map_err(|err| {
                tracing::debug!(error = %err, %path, "bad json body");
                AppError::bad_request().with_param("body")
            })
    }
}

/// `axum::extract::Multipart` 的替身（同 [`Json`] 的理由）。
///
/// axum 内置的拒绝同样是 `text/plain`——`Invalid \`boundary\` for multipart/form-data
/// request`。三个媒体端点（audio transcriptions / translations、images edits）经它入参，
/// 客户端传错 content-type 时拿到的是英文句子而不是错误码。
pub struct Multipart(pub axum::extract::Multipart);

impl<S> FromRequest<S> for Multipart
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let path = req.uri().path().to_owned();
        axum::extract::Multipart::from_request(req, state)
            .await
            .map(Self)
            .map_err(|err| {
                tracing::debug!(error = %err, %path, "bad multipart body");
                AppError::bad_request().with_param("multipart")
            })
    }
}
