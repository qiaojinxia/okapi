//! Claude Code 外形的出站连接：ClientHello 与真机抓包逐项一致、经各类出口代理真实往返、
//! 连接失败按「是不是代理这一跳」分类、总时限覆盖到响应体。
use super::*;
use crate::error::UpstreamError;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_rustls::{TlsAcceptor, rustls};

const CA: &[u8] = include_bytes!("../tests/fixtures/ws-test-ca.der");
const CERT: &[u8] = include_bytes!("../tests/fixtures/ws-localhost.der");
const KEY: &[u8] = include_bytes!("../tests/fixtures/ws-localhost-key.der");
const WAIT: Duration = Duration::from_secs(5);

/// 2.1.293 第一方抓包里 `/v1/messages` 那条连接的 ClientHello（JA4 t13d1713h1_5b57614c22b0_6a3d802a7139）。
const REAL_CIPHERS: [u16; 17] = [
    0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9, 0xcca8, 0xc009, 0xc013, 0xc00a,
    0xc014, 0x009c, 0x009d, 0x002f, 0x0035,
];
const REAL_EXTENSIONS: [u16; 13] = [
    0x0000, 0x0017, 0xff01, 0x000a, 0x000b, 0x0023, 0x0010, 0x0005, 0x000d, 0x0012, 0x0033, 0x002d,
    0x002b,
];
const REAL_GROUPS: [u16; 4] = [0x11ec, 0x001d, 0x0017, 0x0018];
const REAL_SIGALGS: [u16; 9] = [
    0x0403, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501, 0x0806, 0x0601, 0x0201,
];

/// 同一抓包里账号接口（`/api/claude_code/settings`、`/v1/oauth/token`）那条连接的 ClientHello
/// （JA4 t13d181000_5d04281c6031_78e6aca7449b，axios 发的）。
const ACCOUNT_CIPHERS: [u16; 18] = [
    0x1301, 0x1302, 0x1303, 0xc02f, 0xc02b, 0xc030, 0xc02c, 0xc027, 0xcca9, 0xcca8, 0xc009, 0xc013,
    0xc00a, 0xc014, 0x009c, 0x009d, 0x002f, 0x0035,
];
const ACCOUNT_EXTENSIONS: [u16; 10] = [
    0x0000, 0x0017, 0xff01, 0x000a, 0x000b, 0x0023, 0x000d, 0x0033, 0x002d, 0x002b,
];

struct Hello {
    ciphers: Vec<u16>,
    extensions: Vec<u16>,
    groups: Vec<u16>,
    sigalgs: Vec<u16>,
    alpn: Vec<String>,
    sni: String,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

fn u16_list(bytes: &[u8]) -> Vec<u16> {
    let len = usize::from(u16_at(bytes, 0));
    (0..len).step_by(2).map(|i| u16_at(bytes, 2 + i)).collect()
}

fn parse_hello(record: &[u8]) -> Hello {
    let mut p = 4 + 2 + 32;
    p += 1 + usize::from(record[p]);
    let cipher_len = usize::from(u16_at(record, p));
    let ciphers = (0..cipher_len)
        .step_by(2)
        .map(|i| u16_at(record, p + 2 + i))
        .collect();
    p += 2 + cipher_len;
    p += 1 + usize::from(record[p]);
    let end = p + 2 + usize::from(u16_at(record, p));
    p += 2;
    let mut hello = Hello {
        ciphers,
        extensions: vec![],
        groups: vec![],
        sigalgs: vec![],
        alpn: vec![],
        sni: String::new(),
    };
    while p < end {
        let (kind, len) = (u16_at(record, p), usize::from(u16_at(record, p + 2)));
        let body = &record[p + 4..p + 4 + len];
        p += 4 + len;
        hello.extensions.push(kind);
        match kind {
            0x0000 => hello.sni = String::from_utf8_lossy(&body[5..]).into_owned(),
            0x000a => hello.groups = u16_list(body),
            0x000d => hello.sigalgs = u16_list(body),
            0x0010 => {
                let mut q = 2;
                while q < body.len() {
                    let n = usize::from(body[q]);
                    hello
                        .alpn
                        .push(String::from_utf8_lossy(&body[q + 1..=q + n]).into_owned());
                    q += 1 + n;
                }
            }
            _ => {}
        }
    }
    hello
}

/// 应答 CONNECT 后不建 TLS，只截下 ClientHello。
async fn sniff_proxy() -> (SocketAddr, tokio::sync::oneshot::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        read_head(&mut tcp).await;
        tcp.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        let mut header = [0u8; 5];
        tcp.read_exact(&mut header).await.unwrap();
        let mut record = vec![0u8; usize::from(u16_at(&header, 3))];
        tcp.read_exact(&mut record).await.unwrap();
        let _ = send.send(record);
    });
    (addr, receive)
}

async fn read_head(tcp: &mut (impl AsyncRead + Unpin)) -> String {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(tcp.read_u8().await.unwrap());
    }
    String::from_utf8(head).unwrap()
}

async fn captured_hello(shape: Shape, url: &str) -> Hello {
    let (proxy, hello) = sniff_proxy().await;
    let tls = ClaudeCodeTls::new(shape).unwrap();
    let request = reqwest::Client::new().post(url).build().unwrap();
    let proxy_url = format!("http://{proxy}");
    let sending = tokio::spawn(async move { tls.send(request, Some(&proxy_url)).await });
    let hello = parse_hello(&timeout(WAIT, hello).await.unwrap().unwrap());
    sending.abort();
    hello
}

#[tokio::test]
async fn client_hello_matches_the_captured_claude_code_cli() {
    let hello = captured_hello(Shape::Messages, "https://api.anthropic.com/v1/messages").await;
    assert_eq!(hello.ciphers, REAL_CIPHERS);
    assert_eq!(
        hello.extensions, REAL_EXTENSIONS,
        "顺序也要一致（JA3 按顺序算）"
    );
    assert_eq!(hello.groups, REAL_GROUPS);
    assert_eq!(hello.sigalgs, REAL_SIGALGS);
    assert_eq!(hello.alpn, ["http/1.1"], "真机不声明 h2");
    assert_eq!(hello.sni, "api.anthropic.com");
}

#[tokio::test]
async fn account_client_hello_matches_the_captured_axios_connection() {
    let hello = captured_hello(Shape::Account, "https://platform.claude.com/v1/oauth/token").await;
    assert_eq!(hello.ciphers, ACCOUNT_CIPHERS);
    assert_eq!(hello.extensions, ACCOUNT_EXTENSIONS, "无 ALPN / OCSP / SCT");
    assert_eq!(hello.groups, REAL_GROUPS);
    assert_eq!(hello.sigalgs, REAL_SIGALGS);
    assert!(hello.alpn.is_empty());
    assert_eq!(hello.sni, "platform.claude.com");
}

/// 明文源站：同一连接上逐个收请求，把收到的原始请求头当响应体回过去。
async fn head_echo() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        loop {
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                match tcp.read_u8().await {
                    Ok(byte) => head.push(byte),
                    Err(_) => return,
                }
            }
            let head = String::from_utf8(head).unwrap();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .map_or(0, |v| v.parse::<usize>().unwrap());
            let mut body = vec![0u8; length];
            tcp.read_exact(&mut body).await.unwrap();
            let reply = format!("{head}{}", String::from_utf8(body).unwrap());
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{reply}",
                reply.len()
            );
            tcp.write_all(response.as_bytes()).await.unwrap();
        }
    });
    addr
}

#[tokio::test]
async fn header_names_go_out_spelled_and_ordered_like_the_cli() {
    let origin = head_echo().await;
    let tls = ClaudeCodeTls::new(Shape::Messages).unwrap();
    // 第二个请求复用同一连接：头部之后的请求体（里面也有冒号和换行）原样放过，下一个头照样改写
    for body in ["{\"a\":\"x-app: y\\r\\n\"}", "second:\r\n\r\nbody"] {
        // 故意打乱插入顺序；`x-extra` 是渠道额外头之类真机没有的
        let request = reqwest::Client::new()
            .post(format!("http://{origin}/v1/messages?beta=true"))
            .header("x-app", "cli")
            .header("x-extra", "1")
            .header("anthropic-version", "2023-06-01")
            .header("x-stainless-os", "MacOS")
            .header("content-type", "application/json")
            .header("x-claude-code-session-id", "s")
            .header("accept", "application/json")
            .body(body)
            .build()
            .unwrap();
        let response = timeout(WAIT, tls.send(request, None))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            response.text().await.unwrap(),
            format!(
                "POST /v1/messages?beta=true HTTP/1.1\r\nAccept: application/json\r\n\
                 Content-Type: application/json\r\nX-Claude-Code-Session-Id: s\r\n\
                 X-Stainless-OS: MacOS\r\nanthropic-version: 2023-06-01\r\nx-app: cli\r\n\
                 x-extra: 1\r\nConnection: keep-alive\r\nHost: {origin}\r\n\
                 Accept-Encoding: gzip, deflate, br, zstd\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        );
    }
}

async fn compress(encoding: &str, data: &[u8]) -> Vec<u8> {
    use async_compression::tokio::write::{BrotliEncoder, GzipEncoder, ZlibEncoder, ZstdEncoder};
    let mut out = Vec::new();
    match encoding {
        "gzip" => {
            let mut e = GzipEncoder::new(&mut out);
            e.write_all(data).await.unwrap();
            e.shutdown().await.unwrap();
        }
        "deflate" => {
            let mut e = ZlibEncoder::new(&mut out);
            e.write_all(data).await.unwrap();
            e.shutdown().await.unwrap();
        }
        "br" => {
            let mut e = BrotliEncoder::new(&mut out);
            e.write_all(data).await.unwrap();
            e.shutdown().await.unwrap();
        }
        _ => {
            let mut e = ZstdEncoder::new(&mut out);
            e.write_all(data).await.unwrap();
            e.shutdown().await.unwrap();
        }
    }
    out
}

/// 明文源站：回一次固定的头与体，之后不关连接。
async fn reply_once(head: String, body: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        read_head(&mut tcp).await;
        tcp.write_all(head.as_bytes()).await.unwrap();
        tcp.write_all(&body).await.unwrap();
        // 连接留着：体没发完时客户端看到的是停住，而不是断开
        tokio::time::sleep(Duration::from_mins(1)).await;
    });
    addr
}

#[tokio::test]
async fn compressed_responses_are_decoded_for_every_declared_encoding() {
    let text = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".repeat(50);
    for shape in [Shape::Messages, Shape::Account] {
        for encoding in ["gzip", "deflate", "br", "zstd"] {
            let body = compress(encoding, text.as_bytes()).await;
            let origin = reply_once(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-encoding: {encoding}\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                ),
                body,
            )
            .await;
            let tls = ClaudeCodeTls::new(shape).unwrap();
            let request = reqwest::Client::new()
                .get(format!("http://{origin}/"))
                .build()
                .unwrap();
            let response = timeout(WAIT, tls.send(request, None))
                .await
                .unwrap()
                .unwrap();
            assert!(response.headers().get("content-encoding").is_none());
            assert!(response.headers().get("content-length").is_none());
            assert_eq!(response.text().await.unwrap(), text, "{shape:?} {encoding}");
        }
    }
}

/// 压缩的 SSE 边到边解：上游只发出第一个事件时，下游就能读到它。
#[tokio::test]
async fn compressed_event_streams_are_decoded_as_they_arrive() {
    use async_compression::tokio::write::GzipEncoder;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = listener.local_addr().unwrap();
    let (go_on, rest) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        read_head(&mut tcp).await;
        tcp.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
              content-encoding: gzip\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
        let mut encoder = GzipEncoder::new(Vec::new());
        let send = async |encoder: &mut GzipEncoder<Vec<u8>>, tcp: &mut TcpStream| {
            let bytes = std::mem::take(encoder.get_mut());
            tcp.write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                .await
                .unwrap();
            tcp.write_all(&bytes).await.unwrap();
            tcp.write_all(b"\r\n").await.unwrap();
        };
        encoder.write_all(b"data: one\n\n").await.unwrap();
        encoder.flush().await.unwrap();
        send(&mut encoder, &mut tcp).await;
        rest.await.unwrap();
        encoder.write_all(b"data: two\n\n").await.unwrap();
        encoder.shutdown().await.unwrap();
        send(&mut encoder, &mut tcp).await;
        tcp.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let tls = ClaudeCodeTls::new(Shape::Messages).unwrap();
    let request = reqwest::Client::new()
        .get(format!("http://{origin}/"))
        .build()
        .unwrap();
    let mut response = timeout(WAIT, tls.send(request, None))
        .await
        .unwrap()
        .unwrap();
    let first = timeout(WAIT, response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(&first[..], b"data: one\n\n");
    go_on.send(()).unwrap();
    let mut rest = Vec::new();
    while let Some(chunk) = timeout(WAIT, response.chunk()).await.unwrap().unwrap() {
        rest.extend_from_slice(&chunk);
    }
    assert_eq!(rest, b"data: two\n\n");
}

/// 解压路径上总时限照样覆盖响应体，且仍报超时。
#[tokio::test]
async fn the_total_timeout_still_applies_to_compressed_bodies() {
    // 只发出压缩流的前一半，解压器停在等后续数据
    let mut body = compress("gzip", "data: x\n\n".repeat(500).as_bytes()).await;
    let full = body.len();
    body.truncate(full / 2);
    let origin = reply_once(
        format!("HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: {full}\r\n\r\n"),
        body,
    )
    .await;
    let tls = ClaudeCodeTls::new(Shape::Messages).unwrap();
    let mut request = reqwest::Client::new()
        .get(format!("http://{origin}/"))
        .build()
        .unwrap();
    *request.timeout_mut() = Some(Duration::from_millis(300));
    let response = timeout(WAIT, tls.send(request, None))
        .await
        .unwrap()
        .unwrap();
    let error = timeout(WAIT, response.bytes()).await.unwrap().unwrap_err();
    assert!(error.is_timeout(), "{error:?}");
}

#[test]
fn head_case_survives_any_write_split() {
    let wire = b"GET / HTTP/1.1\r\nuser-agent: a\r\ncontent-length: 3\r\n\r\nk: \
                 POST / HTTP/1.1\r\nhost: h\r\n\r\n";
    let expected: &[u8] = b"GET / HTTP/1.1\r\nUser-Agent: a\r\nContent-Length: 3\r\n\r\nk: \
                            POST / HTTP/1.1\r\nHost: h\r\n\r\n";
    for chunk in 1..wire.len() {
        let mut case = HeadCase::default();
        let mut out = Vec::new();
        let mut rest = &wire[..];
        while !rest.is_empty() {
            let take = chunk.min(rest.len());
            let used = match case.passthrough(take) {
                Some(n) => {
                    out.extend_from_slice(&rest[..n]);
                    case.passed(n);
                    n
                }
                None => case.rewrite(&rest[..take], &mut out),
            };
            rest = &rest[used..];
        }
        assert_eq!(out, expected, "chunk {chunk}");
    }
}

/// 本地 HTTPS 源站：回显方法、路径、一个自定义头和请求体。`stall_body` 时发完头就停住。
async fn origin(stall_body: bool) -> SocketAddr {
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(CERT.to_vec())],
        rustls::pki_types::PrivatePkcs8KeyDer::from(KEY.to_vec()).into(),
    )
    .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let mut tls = acceptor.accept(tcp).await.unwrap();
                let alpn = tls.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
                let head = read_head(&mut tls).await;
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                tls.read_exact(&mut body).await.unwrap();
                let first = head.lines().next().unwrap_or_default().to_owned();
                let marker = head
                    .lines()
                    .find_map(|line| line.strip_prefix("x-probe: "))
                    .unwrap_or("-")
                    .to_owned();
                let reply = format!(
                    "{first}|{marker}|{}|{}",
                    String::from_utf8_lossy(&body),
                    String::from_utf8_lossy(&alpn.unwrap_or_default())
                );
                if stall_body {
                    tls.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\npartial")
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_mins(1)).await;
                    return;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{reply}",
                    reply.len()
                );
                tls.write_all(response.as_bytes()).await.unwrap();
            });
        }
    });
    addr
}

/// 透明转发的 HTTP CONNECT 代理；`reject` 时回这个状态码。
async fn connect_proxy(reject: Option<u16>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let head = read_head(&mut client).await;
                if let Some(status) = reject {
                    let _ = client
                        .write_all(
                            format!("HTTP/1.1 {status} No\r\ncontent-length: 0\r\n\r\n").as_bytes(),
                        )
                        .await;
                    return;
                }
                let target = head.split_whitespace().nth(1).unwrap().to_owned();
                let target = target.replace("localhost", "127.0.0.1");
                let mut upstream = TcpStream::connect(target).await.unwrap();
                client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
}

/// 带用户名密码认证（RFC 1929）的 SOCKS5 转发代理，只认 `user` / `pass`。
async fn socks_proxy() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut greeting = [0u8; 2];
                client.read_exact(&mut greeting).await.unwrap();
                let mut methods = vec![0u8; usize::from(greeting[1])];
                client.read_exact(&mut methods).await.unwrap();
                if !methods.contains(&2) {
                    let _ = client.write_all(&[5, 0xff]).await;
                    return;
                }
                client.write_all(&[5, 2]).await.unwrap();
                let mut version = [0u8; 2];
                client.read_exact(&mut version).await.unwrap();
                let mut user = vec![0u8; usize::from(version[1])];
                client.read_exact(&mut user).await.unwrap();
                let mut pass = vec![0u8; usize::from(client.read_u8().await.unwrap())];
                client.read_exact(&mut pass).await.unwrap();
                if (user.as_slice(), pass.as_slice()) != (b"user".as_slice(), b"pass".as_slice()) {
                    let _ = client.write_all(&[1, 1]).await;
                    return;
                }
                client.write_all(&[1, 0]).await.unwrap();
                let mut request = [0u8; 4];
                client.read_exact(&mut request).await.unwrap();
                let host = match request[3] {
                    3 => {
                        let mut name = vec![0u8; usize::from(client.read_u8().await.unwrap())];
                        client.read_exact(&mut name).await.unwrap();
                        String::from_utf8(name)
                            .unwrap()
                            .replace("localhost", "127.0.0.1")
                    }
                    1 => {
                        let mut ip = [0u8; 4];
                        client.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    _ => {
                        let mut ip = [0u8; 16];
                        client.read_exact(&mut ip).await.unwrap();
                        // 本地解析 localhost 常先得到 ::1，测试源站只听 127.0.0.1
                        let ip = std::net::Ipv6Addr::from(ip);
                        if ip.is_loopback() {
                            "127.0.0.1".to_owned()
                        } else {
                            format!("[{ip}]")
                        }
                    }
                };
                let port = client.read_u16().await.unwrap();
                let mut upstream = TcpStream::connect(format!("{host}:{port}")).await.unwrap();
                client
                    .write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
}

fn request(port: u16, body: &str) -> reqwest::Request {
    reqwest::Client::new()
        .post(format!("https://localhost:{port}/v1/messages?beta=true"))
        .header("x-probe", "kept")
        .body(body.to_owned())
        .build()
        .unwrap()
}

#[tokio::test]
async fn requests_round_trip_directly_and_through_every_proxy_kind() {
    let origin = origin(false).await;
    let http_proxy = connect_proxy(None).await;
    let socks = socks_proxy().await;
    let tls = ClaudeCodeTls::with_root(Shape::Messages, CA).unwrap();
    for proxy in [
        None,
        Some(format!("http://{http_proxy}")),
        Some(format!("socks5h://user:pass@{socks}")),
        Some(format!("socks5://user:pass@{socks}")),
    ] {
        for round in 0..2 {
            // 第二轮复用池里的连接
            let body = format!("body-{round}");
            let response = timeout(
                WAIT,
                tls.send(request(origin.port(), &body), proxy.as_deref()),
            )
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{proxy:?}: {e:?}"));
            assert_eq!(response.status(), 200);
            assert_eq!(
                response.text().await.unwrap(),
                format!("POST /v1/messages?beta=true HTTP/1.1|kept|{body}|http/1.1"),
                "{proxy:?}"
            );
        }
    }
}

fn unreachable(result: Result<reqwest::Response, UpstreamError>) -> (bool, bool) {
    match result {
        Err(UpstreamError::Unreachable {
            proxy_hop,
            timed_out,
            ..
        }) => (proxy_hop, timed_out),
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

#[tokio::test]
async fn connect_failures_say_whether_the_proxy_hop_failed() {
    let tls = ClaudeCodeTls::with_root(Shape::Messages, CA).unwrap();
    let closed = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let origin = origin(false).await;
    let tls = &tls;
    let send =
        |proxy: String| async move { tls.send(request(origin.port(), "x"), Some(&proxy)).await };
    // 连不上代理、代理拒绝认证：确定是代理这一跳
    assert_eq!(
        unreachable(send(format!("http://{closed}")).await),
        (true, false)
    );
    let auth = connect_proxy(Some(407)).await;
    assert_eq!(
        unreachable(send(format!("http://{auth}")).await),
        (true, false)
    );
    let socks = socks_proxy().await;
    assert_eq!(
        unreachable(send(format!("socks5h://user:wrong@{socks}")).await),
        (true, false)
    );
    assert_eq!(
        unreachable(send(format!("socks5h://{socks}")).await),
        (true, false),
        "代理要求认证而渠道没配"
    );
    // 代理报目标不可达、直连目标连不上：分不清或不是代理的问题
    let bad_gateway = connect_proxy(Some(502)).await;
    assert_eq!(
        unreachable(send(format!("http://{bad_gateway}")).await),
        (false, false)
    );
    assert_eq!(
        unreachable(tls.send(request(closed.port(), "x"), None).await),
        (false, false)
    );
}

/// 自建 http 地址与测试 mock 走明文，照常往返。
#[tokio::test]
async fn plain_http_targets_still_work() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        read_head(&mut tcp).await;
        tcp.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
            .await
            .unwrap();
    });
    let tls = ClaudeCodeTls::new(Shape::Messages).unwrap();
    let request = reqwest::Client::new()
        .get(format!("http://{addr}/"))
        .build()
        .unwrap();
    let response = timeout(WAIT, tls.send(request, None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "ok");
}

/// 明文目标经 HTTP 代理与 reqwest 一样走转发：请求行是绝对 URI，代理认证头随请求走。
#[tokio::test]
async fn plain_http_targets_are_forwarded_through_http_proxies() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let head = read_head(&mut tcp).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{head}",
            head.len()
        );
        tcp.write_all(response.as_bytes()).await.unwrap();
    });
    let tls = ClaudeCodeTls::new(Shape::Account).unwrap();
    let request = reqwest::Client::new()
        .get("http://upstream.invalid:8080/token")
        .build()
        .unwrap();
    let response = timeout(
        WAIT,
        tls.send(request, Some(&format!("http://user:pass@{proxy}"))),
    )
    .await
    .unwrap()
    .unwrap();
    let head = response.text().await.unwrap();
    assert!(
        head.starts_with("GET http://upstream.invalid:8080/token HTTP/1.1\r\n"),
        "{head}"
    );
    assert!(
        head.contains("\r\nproxy-authorization: Basic dXNlcjpwYXNz\r\n"),
        "{head}"
    );
}

#[tokio::test]
async fn the_total_timeout_also_covers_the_response_body() {
    let origin = origin(true).await;
    let tls = ClaudeCodeTls::with_root(Shape::Messages, CA).unwrap();
    let mut request = request(origin.port(), "x");
    *request.timeout_mut() = Some(Duration::from_millis(300));
    let response = timeout(WAIT, tls.send(request, None))
        .await
        .unwrap()
        .unwrap();
    let error = timeout(WAIT, response.bytes()).await.unwrap().unwrap_err();
    assert!(error.is_timeout(), "{error:?}");
}

/// keep-alive 源站：每条连接上循环应答，记下一共接了几条连接。
async fn counting_origin() -> (SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let (mut tcp, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                loop {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        let Ok(byte) = tcp.read_u8().await else {
                            return;
                        };
                        head.push(byte);
                    }
                    let length = String::from_utf8_lossy(&head)
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                    let mut body = vec![0u8; length];
                    tcp.read_exact(&mut body).await.unwrap();
                    tcp.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .await
                        .unwrap();
                }
            });
        }
    });
    (addr, accepted)
}

#[tokio::test]
async fn accounts_never_share_a_connection() {
    let (origin, accepted) = counting_origin().await;
    let tls = ClaudeCodeTls::new(Shape::Messages).unwrap();
    for account in ["1", "1", "2", "1", "2"] {
        let request = reqwest::Client::new()
            .post(format!("http://{origin}/v1/messages"))
            .body("{}")
            .build()
            .unwrap();
        let response = timeout(WAIT, tls.send_for(Some(account), request, None))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
    }
    // 同一账号复用自己的连接，两个账号各一条
    assert_eq!(accepted.load(Ordering::SeqCst), 2);
}
