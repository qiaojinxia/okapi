//! Conversation archives: ClickHouse first, PostgreSQL for the rows ClickHouse could not take.
//!
//! - Archives written to ClickHouse list back from ClickHouse with the time filter on. The queries select
//!   `toString(created_at)` / `toString(request_id)`; ClickHouse resolves SELECT aliases inside WHERE, so aliases
//!   named like the columns turned the filters into String comparisons and every query failed.
//! - A failing ClickHouse query is an error, not an empty page. Falling back to PostgreSQL (empty while ClickHouse
//!   takes the writes) is what hid that bug: the admin list said "no data" and the detail was not found.
//! - Rows that landed in PostgreSQL during a ClickHouse write failure are listed together with the ClickHouse ones.
//! - Only an unreachable ClickHouse sends the whole read to PostgreSQL.
use chrono::{Duration, Utc};
use okapi_store::ChClient;
use okapi_store::conversation_archive::{self, ArchiveFilter, ArchiveRecord};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

fn record(request_id: Uuid, minutes_ago: i64, text: &str) -> ArchiveRecord {
    ArchiveRecord {
        request_id,
        user_id: 7,
        api_key_id: 8,
        channel_id: 3,
        model: "archive-test".into(),
        requested_model: "archive-test".into(),
        endpoint: "/v1/messages".into(),
        is_stream: true,
        status_code: 200,
        error_code: String::new(),
        request_body: format!("{{\"messages\":[{{\"role\":\"user\",\"content\":\"{text}\"}}]}}"),
        response_body: String::new(),
        response_text: format!("reply to {text}"),
        reasoning_text: String::new(),
        tool_calls: String::new(),
        truncated: false,
        created_at: Utc::now() - Duration::minutes(minutes_ago),
        storage: String::new(),
    }
}

fn page(limit: u32, offset: u32) -> ArchiveFilter {
    ArchiveFilter {
        limit,
        offset,
        ..ArchiveFilter::default()
    }
}

/// Runs `check` against a throwaway ClickHouse database and drops it afterwards, even when `check` panics.
async fn with_clickhouse<F, Fut>(check: F)
where
    F: FnOnce(ChClient) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let database = format!("okapi_archive_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    let outcome = tokio::spawn(check(ch.clone())).await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn clickhouse_archives_list_back_with_the_hours_filter(pg: PgPool) {
    with_clickhouse(|ch| async move {
        let (newer, older, stale) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for (id, minutes, text) in [
            (older, 30, "older"),
            (newer, 5, "newer"),
            (stale, 60 * 30, "stale"),
        ] {
            let stored = conversation_archive::insert(&pg, Some(&ch), &record(id, minutes, text))
                .await
                .unwrap();
            assert_eq!(stored, "clickhouse");
        }

        let filter = ArchiveFilter {
            hours: Some(24),
            ..page(50, 0)
        };
        let listed = conversation_archive::list(&pg, Some(&ch), &filter)
            .await
            .unwrap();
        assert_eq!(listed.backend, "clickhouse");
        let ids: Vec<Uuid> = listed.rows.iter().map(|r| r.request_id).collect();
        assert_eq!(
            ids,
            vec![newer, older],
            "inside 24 h, newest first; the 30 h old one is filtered out"
        );
        assert!(listed.rows.iter().all(|r| r.storage == "clickhouse"));
        let age = Utc::now() - listed.rows[0].created_at;
        assert!(
            age > Duration::minutes(4) && age < Duration::minutes(6),
            "created_at parsed back: {age}"
        );

        let all = conversation_archive::list(&pg, Some(&ch), &page(50, 0))
            .await
            .unwrap();
        assert_eq!(all.rows.len(), 3);

        let searched = conversation_archive::list(
            &pg,
            Some(&ch),
            &ArchiveFilter {
                hours: Some(24),
                search: Some("older".into()),
                ..page(50, 0)
            },
        )
        .await
        .unwrap();
        assert_eq!(
            searched
                .rows
                .iter()
                .map(|r| r.request_id)
                .collect::<Vec<_>>(),
            vec![older]
        );

        let by_id = conversation_archive::list(
            &pg,
            Some(&ch),
            &ArchiveFilter {
                hours: Some(24),
                request_id: Some(older),
                ..page(50, 0)
            },
        )
        .await
        .unwrap();
        assert_eq!(
            by_id.rows.iter().map(|r| r.request_id).collect::<Vec<_>>(),
            vec![older]
        );

        let one = conversation_archive::get_by_request_id(&pg, Some(&ch), newer, Some(7))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(one.response_text, "reply to newer");
        assert_eq!(one.storage, "clickhouse");

        let stats = conversation_archive::stats(&pg, Some(&ch)).await.unwrap();
        assert_eq!(stats.backend, "clickhouse");
        assert_eq!((stats.ch_count, stats.pg_count), (Some(3), 0));
    })
    .await;
}

/// A row ClickHouse could not take was written to PostgreSQL. Detail and stats always counted it; the list showed
/// only ClickHouse rows, so it never appeared there.
#[sqlx::test(migrations = "./migrations")]
async fn rows_spilled_to_postgres_are_listed_with_the_clickhouse_ones(pg: PgPool) {
    with_clickhouse(|ch| async move {
        let (first, spilled, last) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for (id, minutes) in [(first, 5), (last, 30)] {
            let stored = conversation_archive::insert(&pg, Some(&ch), &record(id, minutes, "ch"))
                .await
                .unwrap();
            assert_eq!(stored, "clickhouse");
        }
        let stored = conversation_archive::insert(&pg, None, &record(spilled, 10, "pg"))
            .await
            .unwrap();
        assert_eq!(stored, "postgres");

        let listed = conversation_archive::list(&pg, Some(&ch), &page(50, 0))
            .await
            .unwrap();
        assert_eq!(listed.backend, "clickhouse");
        let rows: Vec<(Uuid, &str)> = listed
            .rows
            .iter()
            .map(|r| (r.request_id, r.storage.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (first, "clickhouse"),
                (spilled, "postgres"),
                (last, "clickhouse")
            ]
        );

        // Paging cuts the merged order, not each backend separately.
        let second = conversation_archive::list(&pg, Some(&ch), &page(1, 1))
            .await
            .unwrap();
        assert_eq!(
            second.rows.iter().map(|r| r.request_id).collect::<Vec<_>>(),
            vec![spilled]
        );
        let third = conversation_archive::list(&pg, Some(&ch), &page(2, 2))
            .await
            .unwrap();
        assert_eq!(
            third.rows.iter().map(|r| r.request_id).collect::<Vec<_>>(),
            vec![last]
        );

        let detail = conversation_archive::get_by_request_id(&pg, Some(&ch), spilled, Some(7))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.storage, "postgres");
    })
    .await;
}

/// Nothing listens on the ClickHouse address: the read goes to PostgreSQL and says so.
#[sqlx::test(migrations = "./migrations")]
async fn an_unreachable_clickhouse_falls_back_to_postgres(pg: PgPool) {
    let ch = ChClient::new("http://127.0.0.1:9", "okapi").unwrap();
    let id = Uuid::new_v4();
    conversation_archive::insert(&pg, None, &record(id, 5, "pg"))
        .await
        .unwrap();

    let listed = conversation_archive::list(&pg, Some(&ch), &page(50, 0))
        .await
        .unwrap();
    assert_eq!(listed.backend, "postgres");
    assert_eq!(
        listed.rows.iter().map(|r| r.request_id).collect::<Vec<_>>(),
        vec![id]
    );
    let detail = conversation_archive::get_by_request_id(&pg, Some(&ch), id, None)
        .await
        .unwrap();
    assert!(detail.is_some());
    let stats = conversation_archive::stats(&pg, Some(&ch)).await.unwrap();
    assert_eq!(stats.backend, "postgres");
    assert_eq!((stats.ch_count, stats.pg_count), (None, 1));
}

/// Answers DDL with 200 and every query with the error ClickHouse gave for the shadowing alias.
async fn clickhouse_rejecting_queries() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                // Headers, then the body by Content-Length.
                let (head_end, length) = loop {
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..n]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while request.len() < head_end + length {
                    let n = stream.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..n]);
                }
                let sql = String::from_utf8_lossy(&request[head_end..]).to_string();
                let (status, body) = if sql.trim_start().starts_with("CREATE") {
                    ("200 OK", String::new())
                } else {
                    (
                        "500 Internal Server Error",
                        "Code: 43. DB::Exception: Illegal types of arguments (String, DateTime64(3)) of function \
                         greaterOrEquals. (ILLEGAL_TYPE_OF_ARGUMENT) (version 24.8.14.39 (official build))\n"
                            .to_owned(),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

/// The bug of 2026-11-22: the query is wrong, PostgreSQL has nothing. That is an error to report, not "no data".
#[sqlx::test(migrations = "./migrations")]
async fn a_failing_clickhouse_query_is_an_error_not_an_empty_page(pg: PgPool) {
    let ch = ChClient::new(&clickhouse_rejecting_queries().await, "okapi").unwrap();
    let listed = conversation_archive::list(&pg, Some(&ch), &page(50, 0)).await;
    let error = listed.expect_err("a broken query must not read as an empty page");
    assert!(!error.is_ch_unavailable(), "{error}");
    let detail =
        conversation_archive::get_by_request_id(&pg, Some(&ch), Uuid::new_v4(), None).await;
    assert!(detail.is_err(), "{detail:?}");
}
