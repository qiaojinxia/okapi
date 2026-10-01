//! Optional fixture-only SQL recorder. Headers/credentials are never recorded.
use axum::body::to_bytes;
use axum::extract::Request;
use axum::response::IntoResponse;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub async fn configured(
    url: Option<&str>,
    selected_database: Option<&str>,
) -> Option<okapi_store::ChClient> {
    let directory = std::env::var("OKAPI_TEST_STATS_SQL_CAPTURE").ok()?;
    let database = selected_database.map_or_else(
        || std::env::var("OKAPI_TEST_CH_DATABASE").unwrap_or_else(|_| "okapi".to_owned()),
        str::to_owned,
    );
    let directory = if selected_database.is_some() {
        format!("{directory}/{database}")
    } else {
        directory
    };
    Some(client(url?, &database, directory).await)
}

async fn client(url: &str, database: &str, directory: String) -> okapi_store::ChClient {
    let directory = PathBuf::from(directory);
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let mut origin = reqwest::Url::parse(url).unwrap();
    origin.set_username("").unwrap();
    origin.set_password(None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let sequence = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().fallback(move |request: Request| {
        let http = http.clone();
        let origin = origin.clone();
        let directory = directory.clone();
        let sequence = sequence.clone();
        async move {
            let (parts, body) = request.into_parts();
            let body = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
            let url = origin.join(&parts.uri.to_string()).unwrap();
            let response = http
                .post(url)
                .headers(parts.headers)
                .body(body.clone())
                .send()
                .await
                .unwrap();
            let status = response.status();
            let reply = response.bytes().await.unwrap();
            let record = serde_json::json!({
                "uri":parts.uri.to_string(),
                "sql":String::from_utf8_lossy(&body),
                "status":status.as_u16(),
                "response":String::from_utf8_lossy(&reply),
            });
            let number = sequence.fetch_add(1, Ordering::Relaxed);
            tokio::fs::write(
                directory.join(format!("{number:04}.json")),
                record.to_string(),
            )
            .await
            .unwrap();
            (status, reply).into_response()
        }
    });
    // The recorder has exactly the fixture process lifetime.
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut proxy = reqwest::Url::parse(url).unwrap();
    proxy.set_host(Some("127.0.0.1")).unwrap();
    proxy.set_port(Some(address.port())).unwrap();
    okapi_store::ChClient::new(proxy.as_str(), database).unwrap()
}
