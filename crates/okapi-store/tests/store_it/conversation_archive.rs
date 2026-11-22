//! Conversation archives written to ClickHouse must be listed back from ClickHouse with the time filter on.
//!
//! The queries select `toString(created_at)` / `toString(request_id)`; ClickHouse resolves SELECT aliases inside WHERE,
//! so aliases named like the columns turned `created_at >= now64(3) - INTERVAL N HOUR` and `request_id = toUUID(…)` into
//! String comparisons. The query failed, the store fell back to PostgreSQL (empty while ClickHouse takes the writes):
//! the admin list said "no data" and the detail of a row was not found.
use chrono::{Duration, Utc};
use okapi_store::ChClient;
use okapi_store::conversation_archive::{self, ArchiveFilter, ArchiveRecord};
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

#[tokio::test]
async fn clickhouse_archives_list_back_with_the_hours_filter() {
    let database = format!("okapi_archive_{}", Uuid::new_v4().simple());
    let ch = ChClient::new(&std::env::var("OKAPI_CLICKHOUSE_URL").unwrap(), &database).unwrap();
    ch.ensure_schema().await.unwrap();
    // Nothing listens here: if the ClickHouse path fails, the PostgreSQL fallback errors instead of quietly coming back empty.
    let pg = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy("postgres://okapi:okapi@127.0.0.1:9/none")
        .unwrap();

    let (ch_task, pg_task) = (ch.clone(), pg.clone());
    let check = async move {
        let (ch, pg) = (ch_task, pg_task);
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
            limit: 50,
            ..ArchiveFilter::default()
        };
        let rows = conversation_archive::list(&pg, Some(&ch), &filter)
            .await
            .unwrap();
        let ids: Vec<Uuid> = rows.iter().map(|r| r.request_id).collect();
        assert_eq!(
            ids,
            vec![newer, older],
            "inside 24 h, newest first; the 30 h old one is filtered out"
        );
        assert!(rows.iter().all(|r| r.storage == "clickhouse"));
        let age = Utc::now() - rows[0].created_at;
        assert!(
            age > Duration::minutes(4) && age < Duration::minutes(6),
            "created_at parsed back: {age}"
        );

        let all = conversation_archive::list(
            &pg,
            Some(&ch),
            &ArchiveFilter {
                limit: 50,
                ..ArchiveFilter::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(all.len(), 3);

        let searched = conversation_archive::list(
            &pg,
            Some(&ch),
            &ArchiveFilter {
                hours: Some(24),
                search: Some("older".into()),
                limit: 50,
                ..ArchiveFilter::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            searched.iter().map(|r| r.request_id).collect::<Vec<_>>(),
            vec![older]
        );

        let by_id = conversation_archive::list(
            &pg,
            Some(&ch),
            &ArchiveFilter {
                hours: Some(24),
                request_id: Some(older),
                limit: 50,
                ..ArchiveFilter::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            by_id.iter().map(|r| r.request_id).collect::<Vec<_>>(),
            vec![older]
        );

        let one = conversation_archive::get_by_request_id(&pg, Some(&ch), newer, Some(7))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(one.response_text, "reply to newer");
        assert_eq!(one.storage, "clickhouse");
    };
    let outcome = tokio::spawn(check).await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
}
