use bytes::Bytes;
use std::{collections::HashMap, fmt::Write as _, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};

pub struct Server {
    pub base: String,
    rx: mpsc::UnboundedReceiver<Exchange>,
    task: JoinHandle<()>,
}
pub struct Exchange {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Bytes,
    reply: oneshot::Sender<Reply>,
}
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub chunks: Vec<Bytes>,
    pub chunked: bool,
}
impl Reply {
    pub fn json(value: impl serde::Serialize) -> Self {
        Self {
            status: 200,
            headers: vec![],
            chunks: vec![serde_json::to_vec(&value).unwrap().into()],
            chunked: false,
        }
    }
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            chunks: vec![],
            chunked: false,
        }
    }
}
impl Exchange {
    pub fn respond(self, reply: Reply) {
        assert!(self.reply.send(reply).is_ok());
    }
    pub fn json(self, value: impl serde::Serialize) {
        self.respond(Reply::json(value));
    }
}
impl Server {
    pub async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{let (socket,_)=accepted.unwrap();let tx=tx.clone();connections.spawn(connection(socket,tx));}
                    result=connections.join_next(),if !connections.is_empty()=>{result.unwrap().unwrap();}
                }
            }
        });
        Self { base, rx, task }
    }
    pub async fn next(&mut self) -> Exchange {
        tokio::time::timeout(Duration::from_secs(3), self.rx.recv())
            .await
            .unwrap()
            .unwrap()
    }
    pub async fn quiet(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(80), self.rx.recv())
                .await
                .is_err()
        );
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn connection(mut socket: TcpStream, tx: mpsc::UnboundedSender<Exchange>) {
    let mut buf = Vec::new();
    let split = loop {
        if let Some(n) = buf.windows(4).position(|v| v == b"\r\n\r\n") {
            break n;
        }
        let mut part = [0; 8192];
        let n = socket.read(&mut part).await.unwrap();
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&part[..n]);
        assert!(buf.len() < 65536);
    };
    let head = std::str::from_utf8(&buf[..split]).unwrap();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap().split_whitespace();
    let method = first.next().unwrap().to_owned();
    let path = first.next().unwrap().to_owned();
    let mut headers: HashMap<String, String> = HashMap::new();
    for line in lines {
        let (key, value) = line.split_once(':').unwrap();
        headers
            .entry(key.to_ascii_lowercase())
            .and_modify(|existing| {
                existing.push_str(", ");
                existing.push_str(value.trim());
            })
            .or_insert_with(|| value.trim().to_owned());
    }
    let size = headers
        .get("content-length")
        .map_or(0, |v| v.parse::<usize>().unwrap());
    assert!(size < 2 * 1024 * 1024);
    let mut body = buf[split + 4..].to_vec();
    while body.len() < size {
        let mut part = [0; 8192];
        let n = socket.read(&mut part).await.unwrap();
        assert_ne!(n, 0);
        body.extend_from_slice(&part[..n]);
    }
    let (reply, rx) = oneshot::channel();
    tx.send(Exchange {
        method,
        path,
        headers,
        body: body.into(),
        reply,
    })
    .ok()
    .unwrap();
    let Ok(reply) = rx.await else {
        return;
    };
    let mut head = format!("HTTP/1.1 {} fixture\r\nConnection: close\r\n", reply.status);
    if reply.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n");
    } else if !reply
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        write!(
            head,
            "Content-Length: {}\r\n",
            reply.chunks.iter().map(Bytes::len).sum::<usize>()
        )
        .unwrap();
    }
    for (key, value) in reply.headers {
        write!(head, "{key}: {value}\r\n").unwrap();
    }
    head.push_str("\r\n");
    if socket.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    for chunk in reply.chunks {
        if reply.chunked
            && socket
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await
                .is_err()
        {
            return;
        }
        if socket.write_all(&chunk).await.is_err() {
            return;
        }
        if reply.chunked && socket.write_all(b"\r\n").await.is_err() {
            return;
        }
    }
    if reply.chunked {
        let _ = socket.write_all(b"0\r\n\r\n").await;
    }
}
