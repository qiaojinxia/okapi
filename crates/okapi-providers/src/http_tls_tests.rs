//! Real TLS negotiation: HTTP upgrades must not inherit the HTTP/2 ALPN pool.
use super::*;
use crate::{
    ChatEvent,
    responses_ws::{ResponsesSocket, SocketTimeouts},
};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, rustls, server::TlsStream};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request, Response},
    },
};

const CA: &[u8] = include_bytes!("../tests/fixtures/ws-test-ca.der");
const CERT: &[u8] = include_bytes!("../tests/fixtures/ws-localhost.der");
const KEY: &[u8] = include_bytes!("../tests/fixtures/ws-localhost-key.der");
const WAIT: Duration = Duration::from_secs(5);
type TlsPeer = oneshot::Receiver<std::io::Result<TlsStream<TcpStream>>>;

async fn tls_server() -> (SocketAddr, TlsPeer) {
    // The full workspace enables both ring and aws-lc-rs. Pick a local provider
    // rather than mutating process-wide defaults or depending on test order.
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
    // Prefer h2, just like a normal HTTPS server; this catches request-version-only fixes.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (send, receive) = oneshot::channel();
    tokio::spawn(async move {
        let (tcp, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
        let tls = timeout(WAIT, acceptor.accept(tcp)).await.unwrap();
        let _ = send.send(tls);
    });
    (addr, receive)
}

fn trusted_client(proxy: Option<&str>, policy: ClientPolicy) -> reqwest::Client {
    client_builder(proxy, policy)
        .unwrap()
        .tls_certs_only([reqwest::Certificate::from_der(CA).unwrap()])
        .build()
        .unwrap()
}

async fn exchange(pool: HttpPool, outbound: Outbound, addr: SocketAddr, peer: TlsPeer) {
    let connecting = tokio::spawn(async move {
        ResponsesSocket::connect(
            &pool,
            &format!("wss://localhost:{}/v1/responses", addr.port()),
            &[("authorization", "Bearer tls-test-only")],
            &outbound,
            SocketTimeouts::default(),
        )
        .await
        .unwrap()
    });
    let tls = timeout(WAIT, peer).await.unwrap().unwrap().unwrap();
    assert_eq!(
        tls.get_ref().1.alpn_protocol(),
        Some(b"http/1.1".as_slice())
    );
    // Tungstenite fixes the callback error type to ErrorResponse; this fixture cannot box it.
    #[allow(clippy::result_large_err)]
    let check_headers = |req: &Request, res: Response| {
        assert_eq!(req.version(), reqwest::Version::HTTP_11);
        assert_eq!(req.uri().path(), "/v1/responses");
        assert_eq!(req.headers()["authorization"], "Bearer tls-test-only");
        Ok(res)
    };
    let mut socket = timeout(WAIT, accept_hdr_async(tls, check_headers))
        .await
        .unwrap()
        .unwrap();
    let client = timeout(WAIT, connecting).await.unwrap().unwrap();
    let body = r#"{"type":"response.create","model":"fixture","input":"tls"}"#;
    let handle = client
        .create(Bytes::from_static(body.as_bytes()))
        .await
        .unwrap();
    let message = timeout(WAIT, socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(message.into_text().unwrap().as_str(), body);
    socket.send(Message::Text(r#"{"type":"response.completed","response":{"id":"resp_tls","usage":{"input_tokens":10,"output_tokens":7}}}"#.into())).await.unwrap();
    let events: Vec<_> = timeout(WAIT, handle.events.collect()).await.unwrap();
    assert!(events.iter().all(Result::is_ok));
    assert!(events.iter().any(|event| matches!(event, Ok(ChatEvent::Data { usage: Some(usage), .. }) if usage.prompt_tokens == 10 && usage.completion_tokens == 7)));
    assert!(matches!(events.last(), Some(Ok(ChatEvent::Done))));
    client.close();
}

#[tokio::test]
async fn wss_uses_http1_alpn_and_transports_responses_frames() {
    let mut pool = HttpPool::new().unwrap();
    Arc::get_mut(&mut pool.clients).unwrap().websocket_default =
        trusted_client(None, ClientPolicy::WebSocket);
    let (addr, peer) = tls_server().await;
    exchange(pool, Outbound::default(), addr, peer).await;
}

#[tokio::test]
async fn normal_and_probe_clients_still_negotiate_http2_despite_http1_request_version() {
    for policy in [ClientPolicy::Forward, ClientPolicy::Probe] {
        let client = trusted_client(None, policy);
        let (addr, peer) = tls_server().await;
        let request = tokio::spawn(async move {
            client
                .get(format!("https://localhost:{}/", addr.port()))
                .version(reqwest::Version::HTTP_11)
                .send()
                .await
        });
        let tls = timeout(WAIT, peer).await.unwrap().unwrap().unwrap();
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
        drop(tls);
        // TLS was successful; no HTTP/2 response is served by this ALPN control fixture.
        assert!(timeout(WAIT, request).await.unwrap().unwrap().is_err());
    }
}

async fn connect_proxy(addr: SocketAddr) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://fixture:password@{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(async move {
        let (mut socket, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 8192);
            bytes.push(timeout(WAIT, socket.read_u8()).await.unwrap().unwrap());
        }
        let headers = String::from_utf8(bytes).unwrap().to_ascii_lowercase();
        assert!(headers.starts_with(&format!("connect localhost:{} http/1.1\r\n", addr.port())));
        assert!(headers.contains("proxy-authorization: basic zml4dhvyztpwyxnzd29yza==\r\n"));
        assert!(!headers.contains("bearer"));
        let mut upstream = TcpStream::connect(addr).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
    });
    (proxy_url, proxy)
}

#[tokio::test]
async fn wss_through_authenticated_connect_proxy_retains_tls_and_http1() {
    let (addr, peer) = tls_server().await;
    let (proxy_url, proxy) = connect_proxy(addr).await;
    let pool = HttpPool::new().unwrap();
    pool.clients.websocket_proxied.write().unwrap().insert(
        proxy_url.clone(),
        trusted_client(Some(&proxy_url), ClientPolicy::WebSocket),
    );
    exchange(
        pool,
        Outbound {
            proxy_url: Some(proxy_url),
            ..Outbound::default()
        },
        addr,
        peer,
    )
    .await;
    timeout(WAIT, proxy).await.unwrap().unwrap();
}

#[tokio::test]
async fn production_wss_pools_offer_only_http1_in_direct_and_proxied_client_hello() {
    for proxied in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (proxy_url, proxy) = if proxied {
            let (url, job) = connect_proxy(addr).await;
            (Some(url), Some(job))
        } else {
            (None, None)
        };
        // Exercise HttpPool::new and its cache miss path without replacing any client.
        let request = tokio::spawn(async move {
            ResponsesSocket::connect(
                &HttpPool::new().unwrap(),
                &format!("wss://localhost:{}/v1/responses", addr.port()),
                &[],
                &Outbound {
                    proxy_url,
                    ..Outbound::default()
                },
                SocketTimeouts::default(),
            )
            .await
        });
        let (tcp, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
        let hello = timeout(
            WAIT,
            tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), tcp),
        )
        .await
        .unwrap()
        .unwrap();
        let offered: Vec<_> = hello
            .client_hello()
            .alpn()
            .unwrap()
            .map(<[u8]>::to_vec)
            .collect();
        assert_eq!(offered, vec![b"http/1.1".to_vec()]);
        drop(hello);
        assert!(timeout(WAIT, request).await.unwrap().unwrap().is_err());
        if let Some(proxy) = proxy {
            timeout(WAIT, proxy).await.unwrap().unwrap();
        }
    }
}

#[tokio::test]
async fn wss_rejects_untrusted_ca_and_wrong_hostname() {
    for trusted in [false, true] {
        let (addr, peer) = tls_server().await;
        let mut pool = HttpPool::new().unwrap();
        let host = if trusted {
            Arc::get_mut(&mut pool.clients).unwrap().websocket_default =
                trusted_client(None, ClientPolicy::WebSocket);
            "127.0.0.1" // Certificate has localhost only; a trusted CA must not bypass hostname checks.
        } else {
            "localhost"
        };
        let result = timeout(
            WAIT,
            ResponsesSocket::connect(
                &pool,
                &format!("wss://{host}:{}/v1/responses", addr.port()),
                &[],
                &Outbound::default(),
                SocketTimeouts::default(),
            ),
        )
        .await
        .unwrap();
        assert!(
            matches!(result, Err(UpstreamError::Connect(reason)) if reason == "responses_ws_handshake")
        );
        // Joining the accept task also verifies the failed handshake closes the TCP peer.
        assert!(timeout(WAIT, peer).await.unwrap().unwrap().is_err());
    }
}
