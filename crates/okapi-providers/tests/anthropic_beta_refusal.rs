//! 订阅路径上游对 `anthropic-beta` 的拒收：400「Unexpected value(s) `x` for the `anthropic-beta` header」时，
//! 去掉点名的 beta 重发一次并记住（真机的 `refusedBetaHeaders` 闩锁），不把一个我们自己带多的 beta 变成客户端的 502。
//! 本地 TCP 服务按脚本依次应答，记下每个请求头。
use bytes::Bytes;
use okapi_providers::{
    HttpPool, Outbound, UpstreamError, anthropic::MessagesResponse, oauth::anthropic_max,
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const REFUSAL: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Unexpected value(s) `tool-provenance-2026-10-26` for the `anthropic-beta` header. Please consult our documentation at platform.claude.com/docs or try again without the header."},"request_id":"req_test"}"#;
const MESSAGE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
const BODY: &str =
    r#"{"model":"claude-sonnet-5-5","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#;

/// 依次应答 `script` 里的 (状态码, 体)，返回基地址与每个请求的头部文本。
async fn serve(script: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = listener.local_addr().unwrap().to_string();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    tokio::spawn(async move {
        for (status, body) in script {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            while !raw.ends_with(b"\r\n\r\n") {
                raw.push(tcp.read_u8().await.unwrap());
            }
            let head = String::from_utf8(raw).unwrap();
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            let mut request_body = vec![0u8; length];
            tcp.read_exact(&mut request_body).await.unwrap();
            seen.lock().unwrap().push(head);
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            tcp.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{origin}/v1"), heads)
}

fn outbound(beta: &str) -> Outbound {
    Outbound {
        extra_headers: vec![("anthropic-beta".into(), beta.into())],
        ..Default::default()
    }
}

fn beta_of(head: &str) -> String {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("anthropic-beta")
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn count_tokens_resends_without_the_refused_beta_and_remembers_it_per_account() {
    let (base, heads) = serve(vec![
        (400, REFUSAL),
        (200, r#"{"input_tokens":7}"#),
        (200, r#"{"input_tokens":8}"#),
        (200, r#"{"input_tokens":9}"#),
    ])
    .await;
    let pool = HttpPool::new().unwrap();
    let client_betas = "claude-code-20250219,tool-provenance-2026-10-26";

    let first = anthropic_max::count_tokens(
        &pool,
        &base,
        "tok",
        Bytes::from(BODY),
        &outbound(client_betas),
        Some("acct-latch-a"),
    )
    .await
    .unwrap();
    assert_eq!(
        &first[..],
        br#"{"input_tokens":7}"#,
        "重发成功，客户端看不到那次 400"
    );
    {
        let heads = heads.lock().unwrap();
        assert_eq!(heads.len(), 2);
        assert!(
            beta_of(&heads[0]).contains("tool-provenance-2026-10-26"),
            "第一次照常带着"
        );
        let resent = beta_of(&heads[1]);
        assert!(!resent.contains("tool-provenance-2026-10-26"), "{resent}");
        assert!(
            resent.contains("claude-code-20250219") && resent.contains("oauth-2025-04-20"),
            "必备的 beta 一个不少：{resent}"
        );
    }

    // 同一账号之后的请求一开始就不带，只发一次
    let second = anthropic_max::count_tokens(
        &pool,
        &base,
        "tok",
        Bytes::from(BODY),
        &outbound(client_betas),
        Some("acct-latch-a"),
    )
    .await
    .unwrap();
    assert_eq!(&second[..], br#"{"input_tokens":8}"#);
    {
        let heads = heads.lock().unwrap();
        assert_eq!(heads.len(), 3, "没有多余的重发");
        assert!(!beta_of(&heads[2]).contains("tool-provenance-2026-10-26"));
    }

    // 记录是按账号的：另一个账号照旧带着它
    anthropic_max::count_tokens(
        &pool,
        &base,
        "tok",
        Bytes::from(BODY),
        &outbound(client_betas),
        Some("acct-latch-b"),
    )
    .await
    .unwrap();
    let heads = heads.lock().unwrap();
    assert_eq!(heads.len(), 4);
    assert!(beta_of(&heads[3]).contains("tool-provenance-2026-10-26"));
}

#[tokio::test]
async fn messages_resend_once_and_other_400s_are_returned_untouched() {
    let (base, heads) = serve(vec![
        (400, REFUSAL),
        (200, MESSAGE),
        (400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too large"}}"#),
        (400, REFUSAL),
        (400, REFUSAL),
    ])
    .await;
    let pool = HttpPool::new().unwrap();
    let betas = "claude-code-20250219,tool-provenance-2026-10-26";

    let sent = anthropic_max::messages(
        &pool,
        &base,
        "tok",
        Bytes::from(BODY),
        false,
        &outbound(betas),
        Some("acct-messages"),
    )
    .await
    .unwrap();
    assert!(matches!(sent, MessagesResponse::Json { status: 200, .. }));
    assert_eq!(heads.lock().unwrap().len(), 2);

    // 不是点名 beta 的 400：不重发，原样交回上层
    let other = anthropic_max::messages(
        &pool,
        &base,
        "tok",
        Bytes::from(BODY),
        false,
        &outbound(betas),
        Some("acct-messages"),
    )
    .await;
    assert!(
        matches!(other, Err(UpstreamError::Status { status: 400, .. })),
        "a 400 that does not name a beta goes back to the caller as it is"
    );
    assert_eq!(heads.lock().unwrap().len(), 3, "没有多余的重发");

    // 点名的 beta 这次请求里本来就没有（已经拉黑过 / 客户端没带）：也不重发
    let still = anthropic_max::messages(
        &pool,
        &base,
        "tok",
        Bytes::from(BODY),
        false,
        &outbound(betas),
        Some("acct-messages"),
    )
    .await;
    assert!(matches!(
        still,
        Err(UpstreamError::Status { status: 400, .. })
    ));
    assert_eq!(heads.lock().unwrap().len(), 4);
    assert!(!beta_of(&heads.lock().unwrap()[3]).contains("tool-provenance-2026-10-26"));
}
