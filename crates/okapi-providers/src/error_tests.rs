//! 经代理的连接失败归因（IMPLEMENTATION §11.41）：哪些错误能确定坏在代理这一跳。
//! 识别靠 hyper-util 的错误文本，这里用真连接造出各种失败，把文本钉住——升级依赖时文本变了会先在这里红。
use super::UpstreamError;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 一个此刻没人监听的地址。
async fn dead_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
}

/// 假代理：每个连接按 `script` 走一遍「读一次 → 写回」（写空 = 直接断开）。
async fn fake_proxy(script: &'static [&'static [u8]]) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(run_script(socket, script));
        }
    });
    addr
}

async fn run_script(mut socket: TcpStream, script: &'static [&'static [u8]]) {
    let mut buf = [0u8; 4096];
    for reply in script {
        if socket.read(&mut buf).await.unwrap_or(0) == 0 || reply.is_empty() {
            return;
        }
        if socket.write_all(reply).await.is_err() {
            return;
        }
    }
}

async fn failure(proxy: &str, target: &str) -> reqwest::Error {
    reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(proxy).unwrap())
        .build()
        .unwrap()
        .get(target)
        .send()
        .await
        .unwrap_err()
}

async fn assert_hop(proxy: &str, target: &str, expected: bool) {
    let err = failure(proxy, target).await;
    assert!(err.is_connect(), "{proxy} → {target}: {err:?}");
    assert_eq!(
        UpstreamError::proxy_hop_failed(&err),
        expected,
        "{proxy} → {target}: {err:?}"
    );
}

#[tokio::test]
async fn proxy_hop_markers() {
    // 连不上代理：隧道（https 目标）、SOCKS、http 目标不建隧道，三种包法都认
    let dead = dead_addr().await;
    assert_hop(&format!("http://{dead}"), "https://example.invalid/", true).await;
    assert_hop(
        &format!("socks5h://{dead}"),
        "https://example.invalid/",
        true,
    )
    .await;
    assert_hop(&format!("http://{dead}"), "http://example.invalid/", true).await;

    // 代理拒绝认证：HTTP 407；SOCKS5 选了用户名密码再回认证失败
    let http_auth = fake_proxy(&[b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"]).await;
    assert_hop(
        &format!("http://{http_auth}"),
        "https://example.invalid/",
        true,
    )
    .await;
    let socks_auth = fake_proxy(&[&[5, 2], &[1, 1]]).await;
    assert_hop(
        &format!("socks5h://u:p@{socks_auth}"),
        "https://example.invalid/",
        true,
    )
    .await;

    // 分不清是代理还是目标：隧道建立中断、CONNECT 回 502、SOCKS 报目标不可达
    let eof = fake_proxy(&[b""]).await;
    assert_hop(&format!("http://{eof}"), "https://example.invalid/", false).await;
    let bad_gateway = fake_proxy(&[b"HTTP/1.1 502 Bad Gateway\r\n\r\n"]).await;
    assert_hop(
        &format!("http://{bad_gateway}"),
        "https://example.invalid/",
        false,
    )
    .await;
    let host_unreachable = fake_proxy(&[&[5, 0], &[5, 4, 0, 1, 0, 0, 0, 0, 0, 0]]).await;
    assert_hop(
        &format!("socks5h://{host_unreachable}"),
        "https://example.invalid/",
        false,
    )
    .await;
}
