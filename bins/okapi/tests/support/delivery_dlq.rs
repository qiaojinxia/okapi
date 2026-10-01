//! HTTP requeue/discard must operate on the original immutable batch.
use super::{Env, setup};
use okapi::worker::chsink;
use okapi_store::ChClient;
use serde_json::{Value, json};
use uuid::Uuid;

async fn post(env: &Env, path: &str, ids: &[i64]) -> Value {
    let response = reqwest::Client::new()
        .post(format!("http://{}{path}", env.addr))
        .bearer_auth(&env.super_token)
        .json(&json!({"ids":ids}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

async fn drain(env: &Env) {
    for _ in 0..100 {
        if chsink::process_once(&env.pg, env.state.ch.as_ref().unwrap())
            .await
            .unwrap()
            == 0
        {
            return;
        }
    }
    panic!("queue did not drain");
}

async fn failing_batch(env: &Env, ambiguous: bool) -> (Uuid, Vec<i64>, Value) {
    drain(env).await;
    let mut outbox = Vec::new();
    for amount in [10_i64, 20] {
        let p = json!({"request_id":Uuid::new_v4(),"user_id":env.user_id,"api_key_id":env.user_key_id,
            "group":"default","model":env.model,"channel_id":env.channel_id,"log_type":2,
            "status":20,"prompt_tokens":100,"completion_tokens":20,"amount_micro":amount,
            "original_amount_micro":amount,"discount_micro":0,"pricing_epoch":1,
            "latency_ms":12,"ttft_ms":5,"is_stream":true,"node":"delivery-dlq-test"});
        outbox.push(sqlx::query_scalar!(
            "INSERT INTO billing_outbox (topic, payload) VALUES ('billing.completed', $1) RETURNING id", p
        ).fetch_one(&env.pg).await.unwrap());
    }
    if ambiguous {
        let name = format!("dlq_fault_{}", Uuid::new_v4().simple());
        let id = outbox[0];
        // Only an internally generated identifier and this fixture's integer ID are interpolated.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected mark failure'; END $$;
             CREATE TRIGGER {name} BEFORE UPDATE ON billing_outbox FOR EACH ROW
             WHEN (NEW.id={id} AND NEW.status=1) EXECUTE FUNCTION {name}();"
        ))).execute(&env.pg).await.unwrap();
        let attempted = chsink::process_once(&env.pg, env.state.ch.as_ref().unwrap()).await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER {name} ON billing_outbox; DROP FUNCTION {name}();"
        )))
        .execute(&env.pg)
        .await
        .unwrap();
        assert!(attempted.is_err());
    }
    let bad = ChClient::new("http://127.0.0.1:9", "okapi").unwrap();
    chsink::process_once(&env.pg, &bad).await.unwrap();
    let batch = sqlx::query!(
        "SELECT b.id,b.rows FROM billing_ch_batches b JOIN billing_outbox o ON o.ch_batch_id=b.id WHERE o.id=$1",
        outbox[0]
    ).fetch_one(&env.pg).await.unwrap();
    for _ in 0..4 {
        sqlx::query!(
            "UPDATE billing_ch_batches SET next_retry_at=now()-interval '1 second' WHERE id=$1",
            batch.id
        )
        .execute(&env.pg)
        .await
        .unwrap();
        chsink::process_once(&env.pg, &bad).await.unwrap();
    }
    let ids = sqlx::query_scalar!(
        "SELECT id FROM billing_dlq WHERE ch_batch_id=$1 ORDER BY id",
        batch.id
    )
    .fetch_all(&env.pg)
    .await
    .unwrap();
    assert_eq!(ids.len(), 2);
    (batch.id, ids, batch.rows)
}

#[tokio::test]
async fn selecting_one_dlq_member_retries_the_original_complete_batch() {
    let env = setup().await;
    let (batch, ids, frozen) = failing_batch(&env, true).await;
    let body = post(&env, "/admin/dlq/requeue", &ids[..1]).await;
    assert_eq!(body["requeued"], 2, "actual batch members must be reported");
    let (status, diagnose) = super::get(&env, "/admin/diagnose", &env.super_token).await;
    assert_eq!(status, 200);
    assert_eq!(
        diagnose["delivery"]["pending_events"], 2,
        "do not count linked outbox twice"
    );
    assert_eq!(diagnose["delivery"]["ch_pending_events"], 2);
    let restored = sqlx::query!(
        "SELECT status,retry_count,rows FROM billing_ch_batches WHERE id=$1",
        batch
    )
    .fetch_one(&env.pg)
    .await
    .unwrap();
    assert_eq!(restored.status, 0);
    assert_eq!(restored.retry_count, 0);
    assert_eq!(
        restored.rows, frozen,
        "requeue must keep exact CH data and ordering"
    );
    drain(&env).await;
    let raw = env.state.ch.as_ref().unwrap().query_json_each_row(&format!(
        "SELECT count() AS n,sum(prompt_tokens+completion_tokens) AS tokens,sum(amount_micro) AS amount FROM request_log_raw WHERE user_id={}", env.user_id
    )).await.unwrap();
    assert_eq!(raw[0]["n"].as_str(), Some("2"));
    assert_eq!(raw[0]["tokens"].as_str(), Some("240"));
    assert_eq!(raw[0]["amount"].as_str(), Some("30"));
    let mv = env.state.ch.as_ref().unwrap().query_json_each_row(&format!(
        "SELECT countMerge(requests) AS n,sumMerge(tokens) AS tokens,sumMerge(amount) AS amount FROM mv_user_day WHERE user_id={}", env.user_id
    )).await.unwrap();
    assert_eq!(mv[0], raw[0]);
    assert_eq!(post(&env, "/admin/dlq/requeue", &ids).await["requeued"], 0);
}

#[tokio::test]
async fn partial_discard_is_rejected_before_explicit_whole_batch_discard() {
    let env = setup().await;
    let (batch, ids, _) = failing_batch(&env, false).await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/admin/dlq/discard", env.addr))
        .bearer_auth(&env.super_token)
        .json(&json!({"ids":ids[..1]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        400,
        "must not silently discard other selected batch members"
    );
    let pending = sqlx::query_scalar!("SELECT status FROM billing_dlq WHERE ch_batch_id=$1", batch)
        .fetch_all(&env.pg)
        .await
        .unwrap();
    assert_eq!(pending, vec![0, 0]);
    let (status, list) = super::get(
        &env,
        &format!("/admin/dlq?batch_id={batch}"),
        &env.super_token,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(list["data"].as_array().unwrap().len(), 2);
    assert_eq!(list["data"][0]["delivery_batch_size"], 2);
    assert_eq!(post(&env, "/admin/dlq/discard", &ids).await["discarded"], 2);
    let status = sqlx::query_scalar!("SELECT status FROM billing_dlq WHERE ch_batch_id=$1", batch)
        .fetch_all(&env.pg)
        .await
        .unwrap();
    assert_eq!(status, vec![2, 2]);
    assert_eq!(
        post(&env, "/admin/dlq/requeue", &ids[1..]).await["requeued"],
        0
    );
    drain(&env).await;
    let raw = env
        .state
        .ch
        .as_ref()
        .unwrap()
        .query_json_each_row(&format!(
            "SELECT count() AS n FROM request_log_raw WHERE user_id={}",
            env.user_id
        ))
        .await
        .unwrap();
    assert_eq!(raw[0]["n"].as_str(), Some("0"));
}
