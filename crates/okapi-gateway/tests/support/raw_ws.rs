//! 手写 WebSocket 帧：tungstenite 客户端总会掩码、不会发违规帧，要验证服务端对违规帧的回应，
//! 只能自己写字节。只覆盖这类用例需要的最小子集（小帧、服务端帧不掩码）。
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 带 Bearer 的 HTTP 升级；返回已完成握手（101）的裸连接。不校验 Sec-WebSocket-Accept。
pub async fn upgrade(addr: SocketAddr, path_and_query: &str, token: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET {path_and_query} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Authorization: Bearer {token}\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut byte))
            .await
            .expect("等升级应答超时")
            .unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    stream
}

/// 读一帧（服务端帧不掩码）：(opcode, 负载)。
pub async fn frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let wait = Duration::from_secs(5);
    let mut head = [0u8; 2];
    tokio::time::timeout(wait, stream.read_exact(&mut head))
        .await
        .expect("等帧超时")
        .unwrap();
    let length = match head[1] & 0x7f {
        126 => {
            let mut raw = [0u8; 2];
            stream.read_exact(&mut raw).await.unwrap();
            u64::from(u16::from_be_bytes(raw))
        }
        127 => {
            let mut raw = [0u8; 8];
            stream.read_exact(&mut raw).await.unwrap();
            u64::from_be_bytes(raw)
        }
        short => u64::from(short),
    };
    let mut payload = vec![0u8; usize::try_from(length).unwrap()];
    tokio::time::timeout(wait, stream.read_exact(&mut payload))
        .await
        .expect("等帧负载超时")
        .unwrap();
    (head[0] & 0x0f, payload)
}

/// 读到关闭帧为止（跳过其间的数据帧），返回它带的 (状态码, 原因)；没有状态码是 `None`。
pub async fn close(stream: &mut TcpStream) -> Option<(u16, String)> {
    loop {
        let (opcode, payload) = frame(stream).await;
        if opcode != 0x8 {
            continue;
        }
        return (payload.len() >= 2).then(|| {
            (
                u16::from_be_bytes([payload[0], payload[1]]),
                String::from_utf8_lossy(&payload[2..]).into_owned(),
            )
        });
    }
}

/// 客户端帧必须掩码（RFC 6455 §5.1），这是一个没掩码的文本帧。
pub fn unmasked_text() -> Vec<u8> {
    vec![0x81, 0x02, b'h', b'i']
}

/// 只写帧头：声明一个 `length` 字节的二进制帧（掩码位已置），不发负载。服务端读到头就会按上限拒绝。
pub fn oversized_header(length: u64) -> Vec<u8> {
    let mut bytes = vec![0x82, 0xFF];
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(&[1, 2, 3, 4]);
    bytes
}

/// 掩码正确、但负载不是合法 UTF-8 的文本帧（掩码键全 0，负载原样）。
pub fn invalid_utf8_text() -> Vec<u8> {
    vec![0x81, 0x81, 0, 0, 0, 0, 0xFF]
}
