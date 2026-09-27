use super::*;

fn query(params: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://peer/v1/images/batches").unwrap();
    url.query_pairs_mut().extend_pairs(params.iter().copied());
    format!("{}?{}", url.path(), url.query().unwrap())
}

#[tokio::test]
async fn batch_filters_apply_before_pagination_and_head_does_not_mark_downloaded() {
    let env = Env::new().await;
    let first = archive::create(&env, "100%_Mixed 图片", 1).await;
    archive::finish(&env, &first).await;
    let second = archive::create(&env, "100%_mixed 再次", 1).await;
    archive::finish(&env, &second).await;
    let queued = archive::create(&env, "unrelated", 1).await;
    for (index, job) in [&first, &second, &queued].iter().enumerate() {
        sqlx::query("UPDATE image_batches SET created_at=to_timestamp($2) WHERE id=$1")
            .bind(id(job))
            .bind(1_700_000_000_i64 + i64::try_from(index).unwrap())
            .execute(&env.state.pg)
            .await
            .unwrap();
    }
    for suffix in ["/content/0", "/download"] {
        let response = env
            .request(reqwest::Method::HEAD, &path(&first, suffix))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.bytes().await.unwrap().is_empty());
    }
    assert!(env.poll(&first).await["downloaded_at"].is_null());
    assert_eq!(env.state.image_download_gate.available_permits(), 4);
    let page = env
        .value(
            reqwest::Method::GET,
            &query(&[
                ("q", "%_MIXED"),
                ("status", "completed"),
                ("limit", "1"),
                ("downloaded", "false"),
            ]),
        )
        .await;
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"][0]["id"], second["id"]);
    assert_eq!(page["has_more"], true);
    let cursor = page["next_cursor"].as_str().unwrap();
    env.value(reqwest::Method::DELETE, &path(&second, "")).await;
    let next = env
        .value(
            reqwest::Method::GET,
            &query(&[
                ("q", "%_mixed"),
                ("status", "completed"),
                ("limit", "1"),
                ("cursor", cursor),
            ]),
        )
        .await;
    assert_eq!(next["data"][0]["id"], first["id"]);
    assert_eq!(next["has_more"], false);
    let range = env
        .value(
            reqwest::Method::GET,
            "/v1/images/batches?created_from=1700000000&created_before=1700000001",
        )
        .await;
    assert_eq!(range["data"].as_array().unwrap().len(), 1);
    assert_eq!(range["data"][0]["id"], first["id"]);
    env.request(reqwest::Method::GET, &path(&first, "/content/0"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let downloaded = env
        .value(reqwest::Method::GET, "/v1/images/batches?downloaded=true")
        .await;
    assert_eq!(downloaded["data"].as_array().unwrap().len(), 1);
    assert_eq!(downloaded["data"][0]["id"], first["id"]);
    let funding = env
        .value(
            reqwest::Method::GET,
            "/v1/images/batches?status=funding&downloaded=false",
        )
        .await;
    assert_eq!(funding["data"][0]["id"], queued["id"]);
    invalid_filters(&env).await;
    env.close().await;
}

async fn invalid_filters(env: &Env) {
    for query in [
        "status=unknown",
        "downloaded=maybe",
        "created_from=-1",
        "created_from=10&created_before=10",
        "created_from=20&created_before=10",
        "created_before=9223372036854775807",
        "q=%0A",
        "limit=0",
        "limit=101",
        "unknown=true",
    ] {
        let response = env
            .request(reqwest::Method::GET, &format!("/v1/images/batches?{query}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{query}");
    }
}
