//! API key 渠道上的 `anthropic-beta`：客户端带来的 beta 转给上游（原先整行丢掉，依赖 beta 的功能在 key 渠道上悄悄失效或 400），
//! 与渠道自己配的合并成一行；形状不对的值不转；上游 400「Unexpected value(s) …」点名拒收时去掉点名的重发一次并按 key 记住。
//! 本地 TCP 服务按脚本依次应答，记下每个请求头。
use bytes::Bytes;
use okapi_providers::profiles::RequestContext;
use okapi_providers::{AnthropicUpstream, Outbound, UpstreamError, anthropic::MessagesResponse};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MESSAGE: &str = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
const BODY: &str =
    r#"{"model":"claude-sonnet-5-5","max_tokens":8,"messages":[{"role":"user","content":"hi"}]}"#;

fn refusal(beta: &str) -> String {
    format!(
        r#"{{"type":"error","error":{{"type":"invalid_request_error","message":"Unexpected value(s) `{beta}` for the `anthropic-beta` header. Please consult our documentation at platform.claude.com/docs or try again without the header."}},"request_id":"req_test"}}"#
    )
}

/// 依次应答 `script` 里的 (状态码, 体)，返回基地址与每个请求的头部文本。
async fn serve(script: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
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

/// 下游客户端带着 `client` 这些 beta 来，渠道自己配的是 `configured`（None = 没配）。
fn outbound(client: Option<&str>, configured: Option<&str>) -> Outbound {
    let mut outbound = Outbound::default();
    if let Some(value) = configured {
        outbound
            .extra_headers
            .push(("Anthropic-Beta".into(), value.into()));
    }
    if let Some(value) = client {
        outbound.context = RequestContext {
            client_headers: vec![("anthropic-beta".into(), value.into())],
            ..RequestContext::default()
        };
    }
    outbound
}

/// 请求头里所有 `anthropic-beta` 行的值（应该最多一行）。
fn betas_of(head: &str) -> Vec<String> {
    head.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("anthropic-beta")
                .then(|| value.trim().to_owned())
        })
        .collect()
}

async fn send_message(
    upstream: &AnthropicUpstream,
    base: &str,
    key: &str,
    outbound: &Outbound,
) -> Result<MessagesResponse, UpstreamError> {
    upstream
        .messages(base, key, Bytes::from(BODY), false, outbound)
        .await
}

#[tokio::test]
async fn a_clients_betas_reach_an_api_key_channel_on_messages_and_count_tokens() {
    let (base, heads) = serve(vec![
        (200, MESSAGE.to_owned()),
        (200, r#"{"input_tokens":7}"#.to_owned()),
        (200, MESSAGE.to_owned()),
    ])
    .await;
    let upstream = AnthropicUpstream::new().unwrap();
    let client = Some("prompt-caching-2024-07-31, interleaved-thinking-2025-05-14");

    send_message(&upstream, &base, "key-forward", &outbound(client, None))
        .await
        .unwrap();
    upstream
        .count_tokens(
            &base,
            "key-forward",
            Bytes::from(BODY),
            &outbound(client, None),
        )
        .await
        .unwrap();
    send_message(&upstream, &base, "key-forward", &outbound(None, None))
        .await
        .unwrap();

    let heads = heads.lock().unwrap();
    for head in &heads[..2] {
        assert_eq!(
            betas_of(head),
            ["prompt-caching-2024-07-31,interleaved-thinking-2025-05-14"],
            "{head}"
        );
    }
    assert!(
        betas_of(&heads[2]).is_empty(),
        "a client that sent no beta gets none sent for it"
    );
}

#[tokio::test]
async fn a_configured_beta_and_the_clients_become_one_header_line() {
    let (base, heads) = serve(vec![(200, MESSAGE.to_owned())]).await;
    let upstream = AnthropicUpstream::new().unwrap();
    send_message(
        &upstream,
        &base,
        "key-merge",
        &outbound(
            Some("prompt-caching-2024-07-31,files-api-2025-04-14"),
            Some("files-api-2025-04-14"),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        betas_of(&heads.lock().unwrap()[0]),
        ["files-api-2025-04-14,prompt-caching-2024-07-31"],
        "the channel's own value first, the client's after it, no duplicates and a single line"
    );
}

#[tokio::test]
async fn client_values_with_the_wrong_shape_are_not_forwarded() {
    let (base, heads) = serve(vec![(200, MESSAGE.to_owned())]).await;
    let upstream = AnthropicUpstream::new().unwrap();
    let too_long = "x".repeat(65);
    let client =
        format!("ok-beta-2025-01-01, Bad-Case, has space, semi;colon, {too_long}, -leading, ok2");
    send_message(
        &upstream,
        &base,
        "key-shape",
        &outbound(Some(&client), None),
    )
    .await
    .unwrap();
    assert_eq!(
        betas_of(&heads.lock().unwrap()[0]),
        ["ok-beta-2025-01-01,ok2"]
    );
}

#[tokio::test]
async fn at_most_thirty_two_betas_are_forwarded() {
    let (base, heads) = serve(vec![(200, MESSAGE.to_owned())]).await;
    let upstream = AnthropicUpstream::new().unwrap();
    let client = (0..40)
        .map(|n| format!("beta-{n}"))
        .collect::<Vec<_>>()
        .join(",");
    send_message(&upstream, &base, "key-cap", &outbound(Some(&client), None))
        .await
        .unwrap();
    let sent = betas_of(&heads.lock().unwrap()[0]).remove(0);
    assert_eq!(sent.split(',').count(), 32, "{sent}");
}

#[tokio::test]
async fn a_refused_beta_is_dropped_once_remembered_per_key_and_other_keys_still_send_it() {
    let (base, heads) = serve(vec![
        (400, refusal("tool-provenance-2026-10-26")),
        (200, MESSAGE.to_owned()),
        (200, MESSAGE.to_owned()),
        (200, MESSAGE.to_owned()),
    ])
    .await;
    let upstream = AnthropicUpstream::new().unwrap();
    let client = Some("prompt-caching-2024-07-31,tool-provenance-2026-10-26");

    let first = send_message(&upstream, &base, "key-heal-a", &outbound(client, None)).await;
    assert!(
        matches!(first, Ok(MessagesResponse::Json { status: 200, .. })),
        "the caller never sees the refused first attempt"
    );
    {
        let heads = heads.lock().unwrap();
        assert_eq!(heads.len(), 2);
        assert_eq!(
            betas_of(&heads[0]),
            ["prompt-caching-2024-07-31,tool-provenance-2026-10-26"]
        );
        assert_eq!(betas_of(&heads[1]), ["prompt-caching-2024-07-31"]);
    }

    // 同一把 key 之后一开始就不带，只发一次
    send_message(&upstream, &base, "key-heal-a", &outbound(client, None))
        .await
        .unwrap();
    assert_eq!(heads.lock().unwrap().len(), 3);
    assert_eq!(
        betas_of(&heads.lock().unwrap()[2]),
        ["prompt-caching-2024-07-31"]
    );

    // 记录是按 key 的：另一把 key 照旧带着它
    send_message(&upstream, &base, "key-heal-b", &outbound(client, None))
        .await
        .unwrap();
    assert_eq!(
        betas_of(&heads.lock().unwrap()[3]),
        ["prompt-caching-2024-07-31,tool-provenance-2026-10-26"]
    );
}

#[tokio::test]
async fn count_tokens_heals_the_same_way() {
    let (base, heads) = serve(vec![
        (400, refusal("context-1m-2025-08-07")),
        (200, r#"{"input_tokens":7}"#.to_owned()),
    ])
    .await;
    let upstream = AnthropicUpstream::new().unwrap();
    let counted = upstream
        .count_tokens(
            &base,
            "key-heal-count",
            Bytes::from(BODY),
            &outbound(Some("context-1m-2025-08-07"), None),
        )
        .await
        .unwrap();
    assert_eq!(&counted[..], br#"{"input_tokens":7}"#);
    let heads = heads.lock().unwrap();
    assert_eq!(heads.len(), 2);
    assert_eq!(betas_of(&heads[0]), ["context-1m-2025-08-07"]);
    assert!(betas_of(&heads[1]).is_empty(), "nothing left to send");
}

#[tokio::test]
async fn a_beta_the_operator_configured_is_never_dropped_and_other_400s_pass_through() {
    let (base, heads) = serve(vec![
        (400, refusal("files-api-2025-04-14")),
        (
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too large"}}"#
                .to_owned(),
        ),
    ])
    .await;
    let upstream = AnthropicUpstream::new().unwrap();
    let configured = outbound(
        Some("prompt-caching-2024-07-31"),
        Some("files-api-2025-04-14"),
    );

    let refused = send_message(&upstream, &base, "key-configured", &configured).await;
    assert!(
        matches!(refused, Err(UpstreamError::Status { status: 400, .. })),
        "the operator asked for that beta: the refusal goes back to the caller"
    );
    assert_eq!(heads.lock().unwrap().len(), 1, "no resend");

    let other = send_message(
        &upstream,
        &base,
        "key-configured-2",
        &outbound(Some("a-beta"), None),
    )
    .await;
    assert!(matches!(
        other,
        Err(UpstreamError::Status { status: 400, .. })
    ));
    assert_eq!(
        heads.lock().unwrap().len(),
        2,
        "a 400 that names no beta is not retried"
    );
}

/// 客户端画像（模拟 Claude Code）接管时，beta 由画像自己管：客户端那行 beta 不会再另发一行，画像的也不被改动。
#[tokio::test]
async fn a_client_profile_keeps_its_own_beta_header_and_the_clients_is_not_added_on_top() {
    let (base, heads) = serve(vec![(200, MESSAGE.to_owned())]).await;
    let upstream = AnthropicUpstream::new().unwrap();
    let outbound = Outbound {
        context: RequestContext {
            extensions: json!({"client_profile": {"name": "claude-code", "mode": "mimic"}}),
            identity_seed: Some("beta-profile".into()),
            client_headers: vec![(
                "anthropic-beta".into(),
                "custom-client-beta-2026-01-01".into(),
            )],
            ..RequestContext::default()
        },
        ..Outbound::default()
    };
    send_message(&upstream, &base, "key-profile", &outbound)
        .await
        .unwrap();
    let sent = betas_of(&heads.lock().unwrap()[0]);
    assert_eq!(sent.len(), 1, "one header line only: {sent:?}");
    assert!(
        !sent[0].contains("custom-client-beta-2026-01-01"),
        "the profile's own set is sent as it is: {sent:?}"
    );
}
