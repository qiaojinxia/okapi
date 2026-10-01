//! A successful HTTP header cannot turn a later query exception into a data row.
use okapi_store::{ChClient, StoreError};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn query(body: String, sql: &str) -> Result<Vec<serde_json::Value>, StoreError> {
    query_response(body, sql, 0, 200).await
}

async fn query_response(
    body: String,
    sql: &str,
    missing_bytes: usize,
    status: u16,
) -> Result<Vec<serde_json::Value>, StoreError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let length = stream.read(&mut buffer).await.unwrap();
            assert!(length > 0);
            request.extend_from_slice(&buffer[..length]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let size = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if request.len() >= end + 4 + size {
                    break;
                }
            }
        }
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len() + missing_bytes
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    let client = ChClient::new(&format!("http://{address}"), "fixture").unwrap();
    let result = client.query_json_each_row(sql).await;
    server.await.unwrap();
    result
}

#[tokio::test]
async fn partial_rows_are_rejected_when_clickhouse_streams_an_exception() {
    for marker in [
        "statistics_request_history_incomplete",
        "statistics_calendar_history_incomplete",
    ] {
        let body = format!(
            "{}\n{}\n",
            json!({"requests":1}),
            json!({"exception":format!("Code: 395. {marker}")})
        );
        let error = query(body, "SELECT 1 AS requests").await.unwrap_err();
        assert!(
            matches!(error,StoreError::InvalidData(code) if code==marker),
            "{error}"
        );
    }
    let error = query(
        format!(
            "{}\n{}\n",
            json!({"requests":1}),
            json!({"exception":"Code: 241. MEMORY_LIMIT_EXCEEDED"})
        ),
        "SELECT 1 AS requests",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, StoreError::ChStatus { status: 200, .. }),
        "{error}"
    );
    let rows = query(
        format!("{}\n", json!({"requests":1})),
        "SELECT 1 AS requests",
    )
    .await
    .unwrap();
    assert_eq!(rows, vec![json!({"requests":1})]);
}

#[tokio::test]
async fn a_failed_coverage_probe_cannot_authorize_a_partial_source() {
    let error = query(
        format!(
            "{}\n{}\n",
            json!({"name":"mv_cube_hour", "missing":"0"}),
            json!({"exception":"Code: 241. MEMORY_LIMIT_EXCEEDED"})
        ),
        "SELECT countMerge(requests) FROM mv_cube_hour",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, StoreError::ChStatus { status: 200, .. }),
        "{error}"
    );
}

#[tokio::test]
async fn a_truncated_response_cannot_become_an_empty_successful_statistic() {
    let error = query_response(
        format!("{}\n", json!({"requests":1})),
        "SELECT 1 AS requests",
        10,
        200,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, StoreError::ChHttp(_)), "{error}");
}

/// SQL echoes contain the guard literals even when an unrelated query failed.
#[tokio::test]
async fn history_labels_require_the_actual_throw_message() {
    for marker in [
        "statistics_request_history_incomplete",
        "statistics_calendar_history_incomplete",
    ] {
        for message in [
            format!(
                "Code: 47. DB::Exception: Unknown identifier. (query: SELECT throwIf(1, '{marker}'))"
            ),
            format!("Code: 241. DB::Exception: Memory limit exceeded. {marker}"),
            format!(
                "Code: 395. DB::Exception: different_guard: while executing SELECT throwIf(1, '{marker}')"
            ),
            format!("Code: 395. DB::Exception: {marker}_unexpected"),
        ] {
            for (status, structured) in [(200, true), (500, false), (500, true)] {
                let body = if structured {
                    format!(
                        "{}\n{}\n",
                        json!({"requests":1}),
                        json!({"exception":message})
                    )
                } else {
                    message.clone()
                };
                let error = query_response(body, "SELECT 1 AS requests", 0, status)
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, StoreError::ChStatus { status:s,.. } if s==status),
                    "{message}: {error}"
                );
            }
        }
        let message = format!(
            "Code: 395. DB::Exception: {marker}: while executing guard. (FUNCTION_THROW_IF_VALUE_IS_NON_ZERO)"
        );
        for (status, structured) in [(200, true), (500, false), (500, true)] {
            let body = if structured {
                json!({"exception":message}).to_string()
            } else {
                message.clone()
            };
            let error = query_response(body, "SELECT 1 AS requests", 0, status)
                .await
                .unwrap_err();
            assert!(
                matches!(error, StoreError::InvalidData(code) if code==marker),
                "{error}"
            );
        }
    }
}
