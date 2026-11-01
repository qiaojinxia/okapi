use super::{mk_user, setup};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
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

/// UTC 当天 0 点往前 `days_ago` 天，再加 `hour` 小时。
fn at(today: NaiveDate, days_ago: i64, hour: i64) -> DateTime<Utc> {
    (today - Duration::days(days_ago))
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        + Duration::hours(hour)
}

async fn seed_series_rows(pg: &PgPool, today: NaiveDate, user: i64, first: i64, other: (i64, i64)) {
    let (other, foreign) = other;
    // (所属用户, 密钥, 时间, status, 输入, 输出, 金额)
    for (owner, key, time, status, input, output, amount) in [
        (user, first, at(today, 2, 1), 20_i16, 100, 50, 500_i64),
        (user, first, at(today, 2, 23), 30, 100, 50, 300), // 已退款：计 token 与请求，不计金额
        (user, first, at(today, 2, 5), 40, 10, 0, 0),      // 失败：计请求与错误
        (user, first, at(today, 0, 0), 20, 10, 5, 70),
        (user, first, at(today, 9, 12), 20, 1000, 500, 9000), // 7 天之外、30 天之内
        (user, first, at(today, 100, 12), 20, 7777, 7777, 7777), // 90 天之外
        // 即便一条外人的账本行挂在这把密钥上，也不能漏进来
        (other, first, at(today, 1, 3), 20, 9000, 9000, 9000),
        // 对方自己的密钥：本人查不到
        (other, foreign, at(today, 1, 3), 20, 4000, 4000, 4000),
    ] {
        sqlx::query("INSERT INTO billing_records (request_id,user_id,api_key_id,model_name,status,prompt_tokens,cached_tokens,completion_tokens,reasoning_tokens,amount_micro,created_at) VALUES ($1,$2,$3,'key-series-fixture',$4,$5,0,$6,0,$7,$8)")
            .bind(Uuid::new_v4()).bind(owner).bind(key).bind(status).bind(input).bind(output).bind(amount).bind(time)
            .execute(pg).await.unwrap();
    }
}

fn assert_week_series(week: &Value, today: NaiveDate) {
    assert_eq!(week["window"]["timezone"], "UTC");
    assert_eq!(
        week["window"]["start_date"],
        (today - Duration::days(6)).to_string()
    );
    assert_eq!(week["window"]["end_date"], today.to_string());
    let data = week["data"].as_array().unwrap();
    assert_eq!(data.len(), 7, "窗口内每天一行，没调用的日子补零");
    for (row, days_ago) in data.iter().zip((0..7).rev()) {
        assert_eq!(row["day"], (today - Duration::days(days_ago)).to_string());
    }
    // today-2（下标 4）：三条账本行；today（下标 6）：一条
    assert_eq!(
        data[4],
        json!({"day": (today - Duration::days(2)).to_string(), "requests": 3, "errors": 1, "tokens": 310, "amount_micro": 500})
    );
    assert_eq!(data[6]["tokens"], 15);
    assert_eq!(data[6]["amount_micro"], 70);
    for i in [0, 1, 2, 3, 5] {
        assert_eq!(
            (
                data[i]["requests"].as_i64(),
                data[i]["tokens"].as_i64(),
                data[i]["amount_micro"].as_i64()
            ),
            (Some(0), Some(0), Some(0)),
            "第 {i} 天应补零"
        );
    }
    assert_eq!(
        week["total"],
        json!({"requests": 4, "errors": 1, "tokens": 325, "amount_micro": 570})
    );
}

/// 密钥用量折线（`/api/me/logs/series`）：与列表迷你折线同一数据源与 UTC 口径，
/// 只含本人该密钥、窗口内逐日补零、金额只计已结算、参数越界被拒。
#[tokio::test]
async fn key_usage_series_is_owned_zero_filled_and_matches_the_trend() {
    let env = setup().await;
    let (user, token) = mk_user(&env.pg).await;
    let (other, other_token) = mk_user(&env.pg).await;
    let key_of = |owner: i64| {
        let pg = env.pg.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT id FROM api_keys WHERE user_id = $1")
                .bind(owner)
                .fetch_one(&pg)
                .await
                .unwrap()
        }
    };
    let (first, foreign) = (key_of(user).await, key_of(other).await);
    let today = Utc::now().date_naive();
    seed_series_rows(&env.pg, today, user, first, (other, foreign)).await;
    let client = reqwest::Client::new();
    let url = |query: &str| format!("http://{}/api/me/logs/series?{query}", env.addr);
    assert_eq!(client.get(url("")).send().await.unwrap().status(), 401);

    let week = series(&client, &url(&format!("api_key_id={first}&days=7")), &token).await;
    assert_week_series(&week, today);

    // 与列表里的迷你折线同一口径：七天 token 逐日相等
    let keys = series(&client, &format!("http://{}/api/me/keys", env.addr), &token).await;
    let trend: Vec<i64> = keys["data"][0]["usage_trend"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    let from_series: Vec<i64> = week["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["tokens"].as_i64().unwrap())
        .collect();
    assert_eq!(trend, from_series);

    // 30 天窗口含 9 天前那条；90 天之外的不计；days 被夹到 1..=90
    let days_of = |body: Value| body["data"].as_array().unwrap().len();
    let month = series(
        &client,
        &url(&format!("api_key_id={first}&days=30")),
        &token,
    )
    .await;
    assert_eq!(days_of(month.clone()), 30);
    assert_eq!(month["total"]["tokens"], 325 + 1500);
    assert_eq!(
        days_of(series(&client, &url(&format!("api_key_id={first}&days=0")), &token).await),
        1
    );
    let max = series(
        &client,
        &url(&format!("api_key_id={first}&days=1000")),
        &token,
    )
    .await;
    assert_eq!(days_of(max.clone()), 90);
    assert_eq!(
        max["total"]["tokens"],
        325 + 1500,
        "100 天前那条不在 90 天窗口内"
    );

    // 归属：别人的密钥 id 查不到任何数据（全零），对方查自己的才有
    let stolen = series(
        &client,
        &url(&format!("api_key_id={foreign}&days=7")),
        &token,
    )
    .await;
    assert_eq!(
        stolen["total"],
        json!({"requests": 0, "errors": 0, "tokens": 0, "amount_micro": 0})
    );
    let theirs = series(
        &client,
        &url(&format!("api_key_id={foreign}&days=7")),
        &other_token,
    )
    .await;
    assert_eq!(theirs["total"]["tokens"], 8000);

    // 参数校验
    for bad in ["api_key_id=0", "api_key_id=-1", "timezone=Not/AZone"] {
        assert_eq!(
            client
                .get(url(bad))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .status(),
            400,
            "{bad}"
        );
    }
}

async fn series(client: &reqwest::Client, url: &str, token: &str) -> Value {
    key_page(client, url, token).await
}
