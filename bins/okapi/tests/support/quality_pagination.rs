use super::ttft_statistics::{insert, row};
use super::{Env, get, setup_with_ch_database};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

#[tokio::test]
async fn quality_lists_page_all_groups_with_stable_order_and_window_shares() {
    let database = format!("okapi_quality_pages_{}", Uuid::new_v4().simple());
    let env = setup_with_ch_database(&database).await;
    let Some(ch) = env.state.ch.as_ref() else {
        eprintln!("跳过：未配置 OKAPI_CLICKHOUSE_URL");
        return;
    };
    ch.ensure_schema().await.unwrap();
    let result = std::panic::AssertUnwindSafe(check_pages(&env, ch))
        .catch_unwind()
        .await;
    ch.execute(&format!("DROP DATABASE {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn check_pages(env: &Env, ch: &ChClient) {
    let samples: Vec<Value> = (0..53)
        .map(|i| {
            let mut sample = row(env, json!(100), true);
            sample["model"] = json!(format!("quality-page-{i:03}"));
            sample["channel_id"] = json!(700_000_000 + i);
            sample["client_type"] = json!(format!("quality-page-{i:03}"));
            sample["error_code"] = json!(format!("quality-page-{i:03}"));
            sample["is_error"] = json!(1);
            sample["log_type"] = json!(5);
            sample
        })
        .collect();
    insert(ch, &samples).await;

    for (kind, field) in [
        ("channels", "channel_id"),
        ("models", "model"),
        ("errors", "error_code"),
        ("clients", "client_type"),
    ] {
        let expected: Vec<Value> = samples.iter().map(|row| row[field].clone()).collect();
        let mut seen = Vec::new();
        for offset in [0, 20, 40, 60] {
            let path = format!("/admin/stats/{kind}?days=7&limit=20&offset={offset}");
            let (status, body) = get(env, &path, &env.super_token).await;
            assert_eq!(status, 200, "{path}: {body}");
            assert_eq!(body["total_items"], 53, "{path}: {body}");
            assert_eq!(body["offset"], offset);
            let rows = body["data"].as_array().unwrap();
            assert!(rows.len() <= 20);
            seen.extend(rows.iter().map(|row| row[field].clone()));
            if kind == "errors" || kind == "clients" {
                let total_field = if kind == "errors" {
                    "total"
                } else {
                    "total_requests"
                };
                assert_eq!(body[total_field], 53, "{path}: {body}");
                for row in rows {
                    assert_eq!(
                        row["share_bp"],
                        10_000 / 53,
                        "share must use the full window: {row}"
                    );
                }
            }
        }
        assert_eq!(
            seen, expected,
            "{kind}: tied rows must not repeat or disappear across pages"
        );

        let (status, negative) = get(
            env,
            &format!("/admin/stats/{kind}?days=7&limit=0&offset=-9"),
            &env.super_token,
        )
        .await;
        assert_eq!(status, 200, "{negative}");
        assert_eq!(negative["limit"], 1);
        assert_eq!(negative["offset"], 0);
        assert_eq!(negative["data"][0][field], expected[0]);

        let (status, empty) = get(
            env,
            &format!("/admin/stats/{kind}?days=7&limit=999&offset=999"),
            &env.super_token,
        )
        .await;
        assert_eq!(status, 200, "{empty}");
        assert_eq!(empty["limit"], 100);
        assert_eq!(empty["total_items"], 53);
        assert_eq!(empty["data"], json!([]));
    }
}
