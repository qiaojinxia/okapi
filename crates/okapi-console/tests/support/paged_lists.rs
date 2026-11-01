//! Test callers that need a complete list must follow its pagination contract.
use serde_json::Value;
use std::net::SocketAddr;

pub async fn get_all(addr: SocketAddr, path: &str, token: &str) -> (u16, Value) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let mut result = Value::Null;
    let mut rows = Vec::new();
    let mut offset = 0;
    loop {
        let mut url = reqwest::Url::parse(&format!("http://{addr}{path}")).unwrap();
        url.query_pairs_mut()
            .append_pair("limit", "200")
            .append_pair("offset", &offset.to_string());
        let response = client.get(url).bearer_auth(token).send().await.unwrap();
        let status = response.status().as_u16();
        let mut page: Value = response.json().await.unwrap();
        assert_eq!(status, 200, "{path}: {page}");
        if result.is_null() {
            result = page.clone();
        }
        assert_eq!(
            page["total"], result["total"],
            "fixture changed during pagination"
        );
        let data = page["data"].as_array_mut().unwrap();
        assert!(data.len() <= 200, "server page must remain bounded");
        let total = usize::try_from(page["total"].as_u64().unwrap()).unwrap();
        let data = page["data"].as_array_mut().unwrap();
        assert!(
            !data.is_empty() || offset == total,
            "pagination made no progress"
        );
        offset += data.len();
        rows.append(data);
        if offset == total {
            result["data"] = Value::Array(rows);
            return (status, result);
        }
        assert!(
            offset < total && offset < 2_000_000,
            "invalid pagination total"
        );
    }
}
