use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn taken(url: &str) -> Sample {
    counter(Some(url)).unwrap().take()
}

// 用局部实例：全局的 `drain` 会取走并行用例的量
#[test]
fn direct_has_no_counter_and_drain_restore_round_trip() {
    let meter = Meter::default();
    assert!(meter.counter(None).is_none());
    assert!(meter.counter(Some("  ")).is_none());
    let url = "http://meter-drain.invalid:1";
    let c = meter.counter(Some(url)).unwrap();
    c.add_up(10);
    c.add_down(20);
    c.add_request();
    // 前后空白是同一个出口
    assert!(Arc::ptr_eq(
        &c,
        &meter.counter(Some(&format!(" {url} "))).unwrap()
    ));
    let drained = meter.drain();
    assert_eq!(
        drained,
        [(
            url.to_owned(),
            Sample {
                up: 10,
                down: 20,
                requests: 1
            }
        )]
    );
    assert!(meter.drain().is_empty(), "取走即清零");
    meter.restore(url, drained[0].1);
    assert_eq!(meter.drain(), drained);
}

#[tokio::test]
async fn metered_io_counts_both_directions() {
    let url = "http://meter-io.invalid:1";
    let (client, mut server) = tokio::io::duplex(64);
    let mut io = MeteredIo::new(client, counter(Some(url)).unwrap());
    io.write_all(b"hello").await.unwrap();
    let mut got = [0u8; 5];
    server.read_exact(&mut got).await.unwrap();
    server.write_all(b"world!!").await.unwrap();
    let mut back = [0u8; 7];
    io.read_exact(&mut back).await.unwrap();
    assert_eq!(
        taken(url),
        Sample {
            up: 5,
            down: 7,
            requests: 0
        }
    );
}

/// 明文目标经 HTTP 代理是转发：代理收到绝对 URI 的请求，直接回应答。
#[tokio::test]
async fn reqwest_send_counts_request_and_response_bodies_through_a_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(tcp.read_u8().await.unwrap());
        }
        let mut body = [0u8; 11];
        tcp.read_exact(&mut body).await.unwrap();
        let reply = "x".repeat(300);
        tcp.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{reply}",
                reply.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .build()
        .unwrap();
    let request = client
        .post("http://upstream.invalid/v1/x")
        .body("hello world");
    let response = send(request, Some(&proxy)).await.unwrap();
    assert_eq!(response.url().as_str(), "http://upstream.invalid/v1/x");
    assert_eq!(response.bytes().await.unwrap().len(), 300);
    assert_eq!(
        taken(&proxy),
        Sample {
            up: 11,
            down: 300,
            requests: 1
        }
    );
}
