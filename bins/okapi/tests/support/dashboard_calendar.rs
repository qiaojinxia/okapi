//! Overview and drill-down use the same calendar; today/yesterday remain actual references.
use super::{Env, chsink, get, payload, setup_with_ch_database};
use chrono::{Days, NaiveDate};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn dashboard_overview_custom_calendar_matches_tokens_and_keeps_actual_yesterday() {
    let database = format!("okapi_dashboard_calendar_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let ch = env
        .state
        .ch
        .as_ref()
        .expect("calendar regression requires ClickHouse");
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_calendar(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_calendar(env: &Env, ch: &ChClient) {
    let metadata = ch
        .query_json_each_row("SELECT toString(today()) AS today,timezone() AS timezone")
        .await
        .unwrap();
    let today =
        NaiveDate::parse_from_str(metadata[0]["today"].as_str().unwrap(), "%Y-%m-%d").unwrap();
    let historic = today - Days::new(9);
    let yesterday = today - Days::new(1);
    let mut rows = Vec::new();
    for (day, tokens, amount) in [(historic, 100, 1_000), (yesterday, 300, 3_000)] {
        // Inserts are UTC, while the requested calendar is the host's IANA timezone.
        let dates = ch
            .query_json_each_row(&format!(
                "SELECT toString(toTimeZone(toDateTime('{day} 12:00:00',timezone()),'UTC')) AS ts",
            ))
            .await
            .unwrap();
        let mut value = payload(env, amount, 0, 100, false);
        value["ts"] = dates[0]["ts"].clone();
        value["prompt_tokens"] = json!(tokens);
        value["completion_tokens"] = json!(tokens / 10);
        value["upstream_usage"] = json!({"prompt_tokens":tokens,"completion_tokens":tokens/10});
        rows.push(chsink::js_payload_to_ch_row(&value));
    }
    ch.insert_json_each_row("request_log_raw", &rows, &Uuid::new_v4().to_string())
        .await
        .unwrap();
    let path = format!("/admin/stats/overview?days=7&start_date={historic}&end_date={historic}");
    let (status, single) = get(env, &path, &env.super_token).await;
    assert_eq!(status, 200, "{single}");
    assert_eq!(single["days"], 1);
    assert_eq!(single["calendar"]["start_date"], historic.to_string());
    assert_eq!(single["calendar"]["end_date"], historic.to_string());
    assert_eq!(single["calendar"]["today"], today.to_string());
    assert_eq!(single["calendar"]["timezone"], metadata[0]["timezone"]);
    assert_eq!(single["window"]["requests"], 1);
    assert_eq!(single["window"]["tokens"], 110);
    assert_eq!(single["window"]["amount_micro"], 1_000);
    assert_eq!(single["today"]["requests"], 0);
    assert_eq!(single["yesterday"]["requests"], 1);
    assert_eq!(single["yesterday"]["tokens"], 330);

    let path = format!("/admin/stats/overview?start_date={historic}&end_date={yesterday}");
    let (status, combined) = get(env, &path, &env.super_token).await;
    assert_eq!(status, 200, "{combined}");
    assert_eq!(combined["days"], 9);
    assert_eq!(combined["window"]["requests"], 2);
    assert_eq!(combined["window"]["tokens"], 440);
    assert_eq!(combined["window"]["active_users"], 1);
    assert_eq!(combined["window"]["amount_micro"], 4_000);
    let trend_path = format!(
        "/admin/stats/trend?fields=core&compare=false&start_date={historic}&end_date={yesterday}"
    );
    let (status, trend) = get(env, &trend_path, &env.super_token).await;
    assert_eq!(status, 200, "{trend}");
    assert_eq!(trend["total"]["tokens"], combined["window"]["tokens"]);
    assert_eq!(trend["total"]["requests"], combined["window"]["requests"]);

    for query in [
        format!("start_date={historic}"),
        format!("start_date={today}&end_date={historic}"),
        format!("start_date={today}&end_date={}", today + Days::new(1)),
        format!("start_date={}&end_date={today}", today - Days::new(366)),
        "start_date=2026-02-30&end_date=2026-03-01".to_owned(),
    ] {
        let (status, body) = get(
            env,
            &format!("/admin/stats/overview?{query}"),
            &env.super_token,
        )
        .await;
        assert_eq!(status, 400, "{query}: {body}");
    }
    let (status, default) = get(env, "/admin/stats/overview", &env.super_token).await;
    assert_eq!(status, 200, "{default}");
    assert_eq!(default["days"], 7);
    assert_eq!(default["window"]["tokens"], 330);
}
