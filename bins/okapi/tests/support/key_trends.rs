use super::{mk_user, setup};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn key_trends_batch_is_owned_paginated_utc_and_zero_filled() {
    let env = setup().await;
    let (user, token) = mk_user(&env.pg).await;
    let (other, other_token) = mk_user(&env.pg).await;
    let first: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id = $1")
        .bind(user)
        .fetch_one(&env.pg)
        .await
        .unwrap();
    let second = okapi_store::provision::create_api_key(
        &env.pg,
        user,
        &Uuid::new_v4().simple().to_string(),
        "sk-trend-empty",
    )
    .await
    .unwrap();
    let third = okapi_store::provision::create_api_key(
        &env.pg,
        user,
        &Uuid::new_v4().simple().to_string(),
        "sk-trend-off",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE api_keys SET status = 2 WHERE id = $1")
        .bind(third)
        .execute(&env.pg)
        .await
        .unwrap();
    let today = Utc::now().date_naive();
    let start = (today - Duration::days(6))
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let tomorrow = (today + Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    for (owner, key, time, status, input, output) in [
        (
            user,
            first,
            start - Duration::milliseconds(1),
            20_i16,
            9000,
            9000,
        ),
        (user, first, start, 20, 100, 50),
        (user, first, start + Duration::hours(23), 30, 100, 50),
        (user, first, start + Duration::days(1), 20, 100, 50),
        (
            user,
            first,
            tomorrow - Duration::milliseconds(1),
            20,
            100,
            50,
        ),
        (user, first, tomorrow, 20, 9000, 9000),
        (user, third, start + Duration::days(2), 20, 200, 20),
        // Even a malformed foreign-owner record attached to this key must not leak in.
        (other, first, start, 20, 9000, 9000),
    ] {
        sqlx::query("INSERT INTO billing_records (request_id,user_id,api_key_id,model_name,status,prompt_tokens,cached_tokens,completion_tokens,reasoning_tokens,created_at) VALUES ($1,$2,$3,'key-trend-fixture',$4,$5,80,$6,10,$7)")
            .bind(Uuid::new_v4()).bind(owner).bind(key).bind(status).bind(input).bind(output).bind(time)
            .execute(&env.pg).await.unwrap();
    }
    let client = reqwest::Client::new();
    let url = format!("http://{}/api/me/keys", env.addr);
    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    let body = key_page(&client, &url, &token).await;
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["id"], first);
    let trend = &rows[0]["usage_trend"];
    assert_eq!(trend["timezone"], "UTC");
    assert_eq!(trend["days"].as_array().unwrap().len(), 7);
    assert_eq!(trend["days"][0], (today - Duration::days(6)).to_string());
    assert_eq!(trend["days"][6], today.to_string());
    assert_eq!(trend["tokens"], json!([300, 150, 0, 0, 0, 0, 150]));
    assert_eq!(rows[1]["id"], second);
    assert_eq!(
        rows[1]["usage_trend"]["tokens"],
        json!([0, 0, 0, 0, 0, 0, 0])
    );
    assert_eq!(rows[2]["status"], 2);
    assert_eq!(
        rows[2]["usage_trend"]["tokens"],
        json!([0, 0, 220, 0, 0, 0, 0])
    );
    let page = key_page(&client, &format!("{url}?limit=1&offset=1"), &token).await;
    assert_eq!(page["total"], 3);
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"][0], rows[1]);
    let foreign = key_page(&client, &url, &other_token).await;
    assert_eq!(foreign["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        foreign["data"][0]["usage_trend"]["tokens"],
        json!([0, 0, 0, 0, 0, 0, 0])
    );
}

async fn key_page(client: &reqwest::Client, url: &str, token: &str) -> Value {
    let response = client.get(url).bearer_auth(token).send().await.unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
