use super::*;
use axum::extract::{Path, Query};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

type Calls = Arc<Mutex<Vec<(String, String)>>>;

#[derive(Clone)]
struct Source {
    prefix: String,
    calls: Calls,
}

async fn page(
    axum::extract::State(source): axum::extract::State<Source>,
    Path(case): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> axum::response::Response {
    let offset = query
        .get("offset")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    source.calls.lock().unwrap().push((
        case.clone(),
        format!("{}:{offset}", query.get("q").map_or("", String::as_str)),
    ));
    let prefix = &source.prefix;
    if case == "oversize" {
        let chunks = futures::stream::iter(
            (0..40)
                .map(|_| Ok::<_, std::convert::Infallible>(bytes::Bytes::from(vec![b'x'; 65536]))),
        );
        return axum::body::Body::from_stream(chunks).into_response();
    }
    if offset > 0 && case == "late-status" {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response();
    }
    if offset > 0 && case == "slow" {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    if case == "many" || case == "aggregate" {
        return axum::Json(json!({"models":[{"model":format!("{prefix}-{case}-{offset}"),"mode":"ratio","model_ratio":"2"}],"total":100_000,"limit":1,"offset":offset,"has_more":true,"next_offset":offset+1,"padding":if case=="aggregate" {"x".repeat(1_000_000)} else {String::new()}})).into_response();
    }
    let models: Vec<_> = (offset..(offset + 2).min(5))
        .map(|i| json!({"model":format!("{prefix}-{case}-{i}"),"mode":"ratio","model_ratio":"2"}))
        .collect();
    let mut body = json!({"models":models,"total":5,"limit":2,"offset":offset,"has_more":offset+2<5,"next_offset":if offset+2<5 {Some(offset+2)} else {None},"next_url":"http://127.0.0.1:9/never-follow"});
    if offset > 0 {
        match case.as_str() {
            "changed" => body["total"] = json!(6),
            "loop" => body["next_offset"] = json!(offset),
            "duplicate" => body["models"][0]["model"] = json!(format!("{prefix}-{case}-0")),
            "empty" => body["models"] = json!([]),
            "shape" => body = json!({"models":models}),
            _ => {}
        }
    }
    axum::Json(body).into_response()
}

async fn paged_mock(prefix: String) -> (SocketAddr, Calls) {
    let calls: Calls = Arc::default();
    let router = Router::new()
        .route("/{case}", get(page))
        .with_state(Source {
            prefix,
            calls: calls.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (addr, calls)
}

#[tokio::test]
async fn sync_follows_numeric_pages_and_retains_filters_without_following_next_urls() {
    let env = setup().await;
    let (mock, calls) = paged_mock(env.new_model.clone()).await;
    let (status, body) = post(
        &env,
        "/admin/pricing/sync/fetch",
        json!({"sources":[{"name":"paged","url":format!("http://{mock}/ok?q=keep%25&offset=0")}]}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["sources"][0]["status"], "ok", "{body}");
    assert_eq!(body["sources"][0]["models"], 5);
    assert_eq!(body["differences"].as_object().unwrap().len(), 5);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            ("ok".into(), "keep%:0".into()),
            ("ok".into(), "keep%:2".into()),
            ("ok".into(), "keep%:4".into())
        ]
    );
    let suffix = env.ratio_model.strip_prefix("rs-ratio-").unwrap();
    let (_,own)=post(&env,"/admin/pricing/sync/fetch",json!({"sources":[{"name":"own","url":format!("http://{}/api/pricing?q={suffix}&limit=1&group_limit=1",env.console)}]})).await;
    assert_eq!(own["sources"][0]["status"], "ok", "{own}");
    assert_eq!(own["sources"][0]["models"], 2);
    assert_eq!(own["differences"], json!({}));
}

#[tokio::test]
async fn sync_discards_partial_sources_when_later_pages_change_or_fail() {
    let env = setup().await;
    let (mock, _) = paged_mock(env.new_model.clone()).await;
    let cases = [
        "late-status",
        "changed",
        "loop",
        "duplicate",
        "empty",
        "shape",
    ];
    let sources: Vec<_> = cases
        .iter()
        .map(|case| json!({"name":case,"url":format!("http://{mock}/{case}")}))
        .collect();
    let (_, body) = post(
        &env,
        "/admin/pricing/sync/fetch",
        json!({"sources":sources}),
    )
    .await;
    assert_eq!(
        body["differences"],
        json!({}),
        "partial prices must never be offered for apply"
    );
    for source in body["sources"].as_array().unwrap() {
        assert_eq!(source["status"], "error", "{source}");
        assert_eq!(source["models"], 0);
        assert!(source["error"].as_str().is_some_and(|s| matches!(
            s,
            "upstream_status" | "pagination_changed" | "pagination_invalid"
        )));
    }
}

#[tokio::test]
async fn sync_has_streaming_body_and_whole_source_deadline_limits() {
    let env = setup().await;
    let (mock, _) = paged_mock(env.new_model.clone()).await;
    let (_,body)=post(&env,"/admin/pricing/sync/fetch",json!({"timeout_secs":1,"sources":[{"name":"slow","url":format!("http://{mock}/slow")},{"name":"oversize","url":format!("http://{mock}/oversize")}]})).await;
    assert_eq!(body["differences"], json!({}));
    let sources = body["sources"].as_array().unwrap();
    assert_eq!(
        sources.iter().find(|s| s["name"] == "slow").unwrap()["error"],
        "timeout"
    );
    assert_eq!(
        sources.iter().find(|s| s["name"] == "oversize").unwrap()["error"],
        "body_too_large"
    );
}

#[tokio::test]
async fn sync_bounds_total_source_bytes_and_page_count_without_returning_partial_prices() {
    let env = setup().await;
    let (mock, calls) = paged_mock(env.new_model.clone()).await;
    let (_,body) = post(&env,"/admin/pricing/sync/fetch",json!({"timeout_secs":60,"sources":[{"name":"many","url":format!("http://{mock}/many")},{"name":"aggregate","url":format!("http://{mock}/aggregate")}]})).await;
    assert_eq!(body["differences"], json!({}));
    let sources = body["sources"].as_array().unwrap();
    assert_eq!(
        sources.iter().find(|s| s["name"] == "many").unwrap()["error"],
        "pagination_limit"
    );
    assert_eq!(
        sources.iter().find(|s| s["name"] == "aggregate").unwrap()["error"],
        "source_too_large"
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|(case, _)| case == "many").count(), 512);
    let reads = calls.iter().filter(|(case, _)| case == "aggregate").count();
    assert!((2..=17).contains(&reads));
}
