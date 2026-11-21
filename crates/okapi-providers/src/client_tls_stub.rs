//! `mimic-tls` feature 关闭时的占位实现：跳过 `boring` / `tokio-boring`（C++/CMake）编译，
//! 让非 TLS 开发与编译检查更快；命中 Claude Code 定制 TLS 传输时显式返回构建期错误。

use crate::error::UpstreamError;
use std::future::Future;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    Messages,
    Account,
}

pub(crate) struct ClaudeCodeTls;

impl ClaudeCodeTls {
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) fn new(_shape: Shape) -> Result<Self, UpstreamError> {
        Ok(Self)
    }

    pub(crate) fn send(
        &self,
        request: reqwest::Request,
        proxy_url: Option<&str>,
    ) -> impl Future<Output = Result<reqwest::Response, UpstreamError>> {
        self.send_for(None, request, proxy_url)
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn send_for(
        &self,
        _account: Option<&str>,
        _request: reqwest::Request,
        _proxy_url: Option<&str>,
    ) -> impl Future<Output = Result<reqwest::Response, UpstreamError>> {
        std::future::ready(Err(UpstreamError::Build(
            "mimic-tls feature is disabled in this build".to_owned(),
        )))
    }
}
