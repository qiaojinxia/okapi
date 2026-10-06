//! The same gateway bill must reach retained tool statistics without synthetic receipts.
use super::{
    Env, Protocol, anthropic_usage, record, report, request_with_tools, server_tool_fees,
    setup_with_ch_database,
};
use futures::FutureExt as _;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Copy)]
struct Amounts {
    payment: Triple,
    search: Triple,
}

#[derive(Clone, Copy)]
struct Triple {
    amount: i64,
    original: i64,
    discount: i64,
}

impl Triple {
    const ZERO: Self = Self {
        amount: 0,
        original: 0,
        discount: 0,
    };

    fn json(self) -> Value {
        json!({"amount_micro":self.amount,"original_amount_micro":self.original,"discount_micro":self.discount})
    }
}

const NORMAL: Amounts = Amounts {
    payment: Triple {
        amount: 21600,
        original: 21600,
        discount: 0,
    },
    search: Triple {
        amount: 20000,
        original: 20000,
        discount: 0,
    },
};

fn usage(fetch: bool) -> Value {
    let mut value = anthropic_usage::fixture();
    value["server_tool_use"] = json!({"web_search_requests":2,"code_execution_requests":1});
    if fetch {
        value["server_tool_use"]["web_fetch_requests"] = json!(3);
    }
    json!({"final":value,"start":{"input_tokens":100,"output_tokens":1,
        "server_tool_use":{"web_search_requests":0,"code_execution_requests":0}},
        "updates":[value,value,{"output_tokens":50}]})
}

fn tools(fetch: bool) -> Value {
    let mut value = vec![
        json!({"type":"web_search_20250305","name":"web_search","max_uses":5}),
        json!({"type":"code_execution_20250825","name":"code_execution"}),
    ];
    if fetch {
        value.push(json!({"type":"web_fetch_20250910","name":"web_fetch","max_uses":5}));
    }
    json!(value)
}

async fn isolated(ingress: Protocol, stream: bool, fetch: bool) {
    isolated_price(ingress, stream, fetch, "1", NORMAL).await;
}

async fn isolated_price(
    ingress: Protocol,
    stream: bool,
    fetch: bool,
    multiplier: &str,
    amounts: Amounts,
) {
    let database = format!("okapi_tool_gateway_{}", Uuid::new_v4().simple());
    let url = std::env::var("OKAPI_CLICKHOUSE_URL").unwrap();
    let ch = ChClient::new(&url, &database).unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        ch.ensure_schema().await.unwrap();
        let env = setup_with_ch_database(usage(fetch), &database).await;
        sqlx::query("UPDATE users SET price_multiplier=$2::text::numeric WHERE id=$1")
            .bind(env.user)
            .bind(multiplier)
            .execute(&env.state.pg)
            .await
            .unwrap();
        exercise(&env, ingress, stream, fetch, amounts).await;
    })
    .catch_unwind()
    .await;
    ch.execute(&format!("DROP DATABASE IF EXISTS {database} SYNC"))
        .await
        .unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn gateway_receipts_retain_tools_across_expiry_price_changes_and_refunds() {
    for ingress in [
        Protocol::Anthropic,
        Protocol::Chat,
        Protocol::Responses,
        Protocol::Gemini,
    ] {
        for stream in [false, true] {
            isolated(ingress, stream, true).await;
        }
    }
}

#[tokio::test]
async fn omitted_unrequested_counter_stays_unknown_with_complete_zero_fee() {
    for stream in [false, true] {
        isolated(Protocol::Anthropic, stream, false).await;
    }
}

#[tokio::test]
async fn signed_tool_discounts_survive_actual_gateway_expiry_and_refunds() {
    for (multiplier, amounts) in [
        (
            "2",
            Amounts {
                payment: Triple {
                    amount: 43200,
                    original: 21600,
                    discount: -21600,
                },
                search: Triple {
                    amount: 40000,
                    original: 20000,
                    discount: -20000,
                },
            },
        ),
        (
            "0.5",
            Amounts {
                payment: Triple {
                    amount: 10800,
                    original: 21600,
                    discount: 10800,
                },
                search: Triple {
                    amount: 10000,
                    original: 20000,
                    discount: 10000,
                },
            },
        ),
    ] {
        for stream in [false, true] {
            isolated_price(Protocol::Anthropic, stream, true, multiplier, amounts).await;
        }
    }
}

async fn post(env: &Env, path: &str, body: &Value) -> Value {
    let response = reqwest::Client::new()
        .post(format!("http://{}{path}", env.console))
        .bearer_auth(&env.token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let value: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{path}: {value}");
    value
}

fn integer(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| value.as_str().unwrap().parse().unwrap())
}

async fn deliver(env: &Env, id: &str, kind: i16) -> Value {
    let ch = env.state.ch.as_ref().unwrap();
    for _ in 0..100 {
        okapi::worker::chsink::process_once(&env.state.pg, ch)
            .await
            .unwrap();
        let rows=ch.query_with_params("SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,upstream_cost_known,ratio_snapshot,server_tool_usage FROM request_log_raw WHERE request_id=toUUID({id:String}) AND log_type={kind:Int16}", &[("id",id),("kind",&kind.to_string())]).await.unwrap();
        if let Some(row) = rows.first() {
            assert_eq!(rows.len(), 1);
            return row.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("missing gateway tool statistics receipt")
}

async fn exercise(env: &Env, ingress: Protocol, stream: bool, fetch: bool, amounts: Amounts) {
    let price = server_tool_fees::prices(&json!({
        "billing":"additional","price_per_request_micro":10000
    }));
    let epoch = server_tool_fees::activate(env, &price).await;
    let response = request_with_tools(
        env,
        ingress,
        stream,
        matches!(ingress, Protocol::Gemini),
        Some(tools(fetch)),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert!(!response.text().await.unwrap().contains("upstream_error"));
    let row = record(env).await;
    let id = row["request_id"].as_str().unwrap();
    let stored: (i64, i64, i64, Option<i64>, Value, Value) = sqlx::query_as(
        "SELECT amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,pricing_snapshot,usage_details FROM billing_records WHERE request_id::text=$1",
    )
    .bind(id)
    .fetch_one(&env.state.pg)
    .await
    .unwrap();
    assert_eq!(
        (stored.0, stored.1, stored.2, stored.3),
        (
            amounts.payment.amount,
            amounts.payment.original,
            amounts.payment.discount,
            None
        )
    );
    assert_eq!(row["amount_micro"], amounts.payment.amount);
    assert_eq!(stored.4["epoch"], epoch);
    assert_eq!(stored.4["server_tool_cost_coverage"]["complete"], false);
    assert_eq!(
        stored.5["tokens"]["server_tool_usage"],
        row["usage"]["server_tool_usage"]
    );
    let delivered = deliver(env, id, 2).await;
    for (name, amount) in [
        ("amount_micro", amounts.payment.amount),
        ("original_amount_micro", amounts.payment.original),
        ("discount_micro", amounts.payment.discount),
        ("upstream_cost_micro", 0),
        ("upstream_cost_known", 0),
    ] {
        assert_eq!(integer(&delivered[name]), amount);
    }
    assert_eq!(
        serde_json::from_str::<Value>(delivered["ratio_snapshot"].as_str().unwrap()).unwrap(),
        stored.4
    );
    assert_eq!(
        serde_json::from_str::<Value>(delivered["server_tool_usage"].as_str().unwrap()).unwrap(),
        row["usage"]["server_tool_usage"]
    );
    check_reports(env, fetch, false, amounts).await;
    let ch = env.state.ch.as_ref().unwrap();
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_reports(env, fetch, false, amounts).await;

    publish_changed_price(env, epoch).await;
    check_reports(env, fetch, false, amounts).await;
    let body = json!({"request_id":id,"reason":"retained gateway statistics"});
    assert_eq!(
        post(env, "/admin/billing/refund", &body).await["refunded_micro"],
        amounts.payment.amount
    );
    assert_eq!(
        post(env, "/admin/billing/refund", &body).await["outcome"],
        "already_refunded"
    );
    let reversal = deliver(env, id, 6).await;
    assert_eq!(integer(&reversal["amount_micro"]), -amounts.payment.amount);
    assert_eq!(
        integer(&reversal["original_amount_micro"]),
        -amounts.payment.original
    );
    assert_eq!(
        integer(&reversal["discount_micro"]),
        -amounts.payment.discount
    );
    assert_eq!(integer(&reversal["upstream_cost_known"]), 0);
    assert_eq!(reversal["server_tool_usage"], "");
    assert_eq!(
        serde_json::from_str::<Value>(reversal["ratio_snapshot"].as_str().unwrap()).unwrap(),
        stored.4
    );
    check_reports(env, fetch, true, amounts).await;
    ch.execute("TRUNCATE TABLE request_log_raw").await.unwrap();
    check_reports(env, fetch, true, amounts).await;
    assert_eq!(env.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

async fn publish_changed_price(env: &Env, epoch: i64) {
    let draft = post(env, "/admin/models", &json!({
        "model_name":env.model,"model_ratio":"1","completion_ratio":"2",
        "cache_ratio":"0.5","cache_write_ratio":"2",
        "server_tool_prices":server_tool_fees::prices(&json!({"billing":"additional","price_per_request_micro":99999}))
    })).await;
    assert_eq!(draft["requires_publish"], true);
    let published = post(env, "/admin/pricing/publish", &json!({})).await;
    assert!(published["epoch"].as_i64().unwrap() > epoch);
    let book = okapi::gateway::pricing_loader::load_pricebook(&env.state.pg)
        .await
        .unwrap();
    assert!(env.state.pricebook.swap_if_newer(book));
}

async fn check_reports(env: &Env, fetch: bool, refunded: bool, amounts: Amounts) {
    let records = if refunded { 2 } else { 1 };
    let payment = if refunded {
        Triple::ZERO
    } else {
        amounts.payment
    };
    let fee = if refunded {
        Triple::ZERO
    } else {
        amounts.search
    };
    let portal = report(env, "/api/me/stats/tools?days=2").await;
    let admin = report(
        env,
        &format!("/admin/stats/tools?days=2&user_id={}", env.user),
    )
    .await;
    assert_eq!(portal["total"], admin["total"]);
    assert_eq!(portal["data"], admin["data"]);
    let total = &portal["total"];
    assert_eq!(total["calls"], 1, "{portal}");
    assert_eq!(total["financial_records"], records);
    assert_eq!(total["history"]["retained_records"], records);
    assert_eq!(total["history"]["raw_recovered_records"], 0);
    assert_eq!(total["history"]["missing_records"], 0);
    assert_eq!(total["history"]["complete"], true);
    assert_eq!(portal["tokens_included"], false);
    for (name, quantity) in [
        ("web_search", Some(2)),
        ("web_fetch", fetch.then_some(3)),
        ("code_execution", Some(1)),
    ] {
        let axis = &total["tools"][name];
        assert_eq!(axis["quantity"], json!(quantity), "{portal}");
        assert_eq!(axis["observed_quantity"], json!(quantity));
        assert_eq!(axis["complete"], quantity.is_some());
        assert_eq!(axis["observed_calls"], i64::from(quantity.is_some()));
        assert_eq!(
            axis["coverage_bp"],
            if quantity.is_some() { 10000 } else { 0 }
        );
        if name == "code_execution" {
            assert!(axis["fee"].is_null());
            assert!(axis["observed_fee"].is_null());
            assert_eq!(axis["fee_observed_records"], 0);
            assert_eq!(axis["fee_complete"], false);
        } else {
            let expected = if name == "web_search" {
                fee
            } else {
                Triple::ZERO
            };
            assert_eq!(axis["fee"], expected.json());
            assert_eq!(axis["fee_observed_records"], records);
            assert_eq!(axis["fee_coverage_bp"], 10000);
            assert_eq!(axis["fee_complete"], true);
        }
    }
    check_trend(env, payment).await;
    assert_eq!(
        env.state
            .ledger
            .balance(env.user)
            .await
            .unwrap()
            .as_micros(),
        50_000_000 - payment.amount
    );
}

async fn check_trend(env: &Env, payment: Triple) {
    for fields in ["all", "core"] {
        let trend = report(
            env,
            &format!(
                "/admin/stats/trend?days=2&user_id={}&fields={fields}",
                env.user
            ),
        )
        .await;
        assert_eq!(trend["total"]["requests"], 1);
        assert_eq!(trend["total"]["tokens"], 1050);
        assert_eq!(trend["total"]["amount_micro"], payment.amount);
        assert_eq!(trend["total"]["original_amount_micro"], payment.original);
        assert_eq!(trend["total"]["discount_micro"], payment.discount);
        if fields == "all" {
            assert_eq!(trend["total"]["cost_known_records"], 0);
            assert_eq!(trend["total"]["cost_coverage_bp"], 0);
        }
    }
}
