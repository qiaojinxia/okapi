//! Shared browser isolation for console and gateway HTTP responses.
use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};

pub(crate) async fn apply(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (name, value) in [
        ("x-frame-options", "DENY"),
        // 只放行同源脚本：管理台的 key 存在 localStorage，注入的脚本拿得到它。入口页的主题
        // 初始化外置成了 /theme-init.js，SPA 不加载任何第三方脚本。
        (
            "content-security-policy",
            "frame-ancestors 'none'; script-src 'self'; object-src 'none'; base-uri 'self'",
        ),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "same-origin"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}
