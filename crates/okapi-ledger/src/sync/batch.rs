//! 结算微批组提交（Group Commit）：把多个并发成功请求的落账合并为一次 PG 事务 + 一次
//! 并发 Redis `commit_in_source` + 一次批量 `DELETE FROM billing_sync`。
//!
//! - 通过 `SET LOCAL okapi.skip_channel_trigger = 'on'` 跳过逐行渠道触发器，改为在内存按主键升序
//!   聚合 `users`、`api_keys`、`channel_token_totals`、`channel_usage_buckets` 后于 `COMMIT` 前一次性更新，
//!   消除高并发命中主力渠道时的行锁串行化与死锁风险。
//! - 通过非阻塞 `pg_try_advisory_xact_lock` 与 `UserGuard` 互斥：被锁定的热点用户立刻退回调用方单笔路径
//!   （`BatchOutcome::Fallback`），绝不拖累同批其他用户。

use crate::holds::{LOCK_NAMESPACE, MAXIMUM_MICROS, UserGuard};
use crate::pg::{
    OwnedSettlementInput, SETTLEMENT_LOCK_NS, bounded_upstream_id, build_settlement_payloads,
    token_i32,
};
use crate::{BalanceLedger, CommitOutcome, LedgerError, Pool};
use okapi_domain::{BillingState, Money};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::{BTreeMap, HashSet};
use tokio::task::JoinSet;
use uuid::Uuid;

/// 微批单条结果：`Inserted` 表示已完成 PG 持久化与 Redis 核销（或已交恢复）；
/// `Fallback` 表示该条需由调用方走单笔 `sync::record`（用户锁竞争 / 订阅窗 / 重复 ID / 批次异常隔离）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOutcome {
    Inserted,
    Fallback,
}

struct PreparedRow {
    index: usize,
    prompt_tokens: i32,
    cached_tokens: i32,
    completion_tokens: i32,
    reasoning_tokens: i32,
    usage_details: Value,
    event_payload: Value,
    outbox_payload: Value,
}

#[derive(Default)]
struct ChannelAgg {
    requests: i64,
    tokens: i64,
    cost_micro: i64,
    unknown_cost_requests: i64,
}

/// 批量提交一组普通成功结算；每条输入对应返回数组中的同下标结果。
pub async fn record_batch(
    pg: &PgPool,
    ledger: &BalanceLedger,
    items: &[OwnedSettlementInput],
) -> Vec<Result<BatchOutcome, LedgerError>> {
    let mut results: Vec<Option<Result<BatchOutcome, LedgerError>>> =
        (0..items.len()).map(|_| None).collect();
    let mut prepared = Vec::with_capacity(items.len());
    let mut seen_requests = HashSet::with_capacity(items.len());

    for (index, item) in items.iter().enumerate() {
        let input = item.as_input();
        if input.state != BillingState::Committed
            || input.log_type != 2
            || input.event_type != "commit"
            || !(0..=MAXIMUM_MICROS).contains(&input.amount.as_micros())
            || input.delta_micro != input.amount.as_micros().saturating_neg()
        {
            results[index] = Some(Err(LedgerError::InvalidSettlement));
            continue;
        }
        if item.source_window.is_some() || !seen_requests.insert(item.request_id) {
            results[index] = Some(Ok(BatchOutcome::Fallback));
            continue;
        }
        match prepare_row(index, &input) {
            Ok(row) => prepared.push(row),
            Err(err) => results[index] = Some(Err(err)),
        }
    }

    if prepared.is_empty() {
        return finalize_results(results);
    }

    let committed = match commit_pg_batch(pg, items, &prepared, &mut results).await {
        Ok(rows) => rows,
        Err(err) => {
            // 事务级连接/瞬时故障：未决条目统一回退单笔（若为 PG 故障，单笔重试循环会按既定策略退避/进 journal）
            tracing::warn!(%err, batch_len = prepared.len(), "settlement batch PG commit failed; falling back per item");
            for row in &prepared {
                if results[row.index].is_none() {
                    results[row.index] = Some(Ok(BatchOutcome::Fallback));
                }
            }
            return finalize_results(results);
        }
    };

    if committed.is_empty() {
        return finalize_results(results);
    }

    finish_redis_batch(pg, ledger, items, &committed).await;
    for &idx in &committed {
        results[idx] = Some(Ok(BatchOutcome::Inserted));
    }
    finalize_results(results)
}

fn finalize_results(
    results: Vec<Option<Result<BatchOutcome, LedgerError>>>,
) -> Vec<Result<BatchOutcome, LedgerError>> {
    results
        .into_iter()
        .map(|slot| slot.unwrap_or(Ok(BatchOutcome::Fallback)))
        .collect()
}

fn prepare_row(
    index: usize,
    input: &crate::SettlementInput<'_>,
) -> Result<PreparedRow, LedgerError> {
    let prompt_tokens = token_i32(input.usage.prompt_tokens)?;
    let cached_tokens = token_i32(input.usage.cached_tokens)?;
    let completion_tokens = token_i32(input.usage.completion_tokens)?;
    let reasoning_tokens = token_i32(input.usage.reasoning_tokens)?;
    let (usage_details, event_payload, outbox_payload) = build_settlement_payloads(input)?;
    Ok(PreparedRow {
        index,
        prompt_tokens,
        cached_tokens,
        completion_tokens,
        reasoning_tokens,
        usage_details,
        event_payload,
        outbox_payload,
    })
}

async fn commit_pg_batch(
    pg: &PgPool,
    items: &[OwnedSettlementInput],
    prepared: &[PreparedRow],
    results: &mut [Option<Result<BatchOutcome, LedgerError>>],
) -> Result<Vec<usize>, LedgerError> {
    let mut unique_users: Vec<i64> = prepared.iter().map(|r| items[r.index].user_id).collect();
    unique_users.sort_unstable();
    unique_users.dedup();

    let mut tx = pg.begin().await?;
    let lock_rows: Vec<(i64, bool)> = sqlx::query_as(
        "WITH setup AS (
             SELECT set_config('lock_timeout', '5s', true),
                    set_config('statement_timeout', '10s', true),
                    set_config('okapi.skip_channel_trigger', 'on', true),
                    pg_advisory_xact_lock_shared($1)
         )
         SELECT u, pg_try_advisory_xact_lock($2, hashtext(u::text)) AS locked
           FROM setup, UNNEST($3::bigint[]) AS u
          ORDER BY u",
    )
    .bind(okapi_store::history::RETENTION_LOCK)
    .bind(LOCK_NAMESPACE)
    .bind(&unique_users)
    .fetch_all(&mut *tx)
    .await?;

    let locked_users: HashSet<i64> = lock_rows
        .into_iter()
        .filter_map(|(uid, locked)| locked.then_some(uid))
        .collect();

    let mut locked_rows: Vec<&PreparedRow> = Vec::with_capacity(prepared.len());
    for row in prepared {
        if locked_users.contains(&items[row.index].user_id) {
            locked_rows.push(row);
        } else {
            results[row.index] = Some(Ok(BatchOutcome::Fallback));
        }
    }
    if locked_rows.is_empty() {
        let _ = tx.rollback().await;
        return Ok(Vec::new());
    }

    let mut req_id_strs: Vec<String> = locked_rows
        .iter()
        .map(|r| items[r.index].request_id.to_string())
        .collect();
    req_id_strs.sort();
    let req_ids: Vec<Uuid> = locked_rows
        .iter()
        .map(|r| items[r.index].request_id)
        .collect();

    let existing_ids: Vec<Uuid> = sqlx::query_scalar(
        "WITH locked AS (
             SELECT r, pg_advisory_xact_lock($1, hashtext(r))
               FROM UNNEST($2::text[]) AS r
              ORDER BY r
         )
         SELECT DISTINCT b.request_id
           FROM billing_financial_records b
          WHERE b.request_id = ANY($3::uuid[])",
    )
    .bind(SETTLEMENT_LOCK_NS)
    .bind(&req_id_strs)
    .bind(&req_ids)
    .fetch_all(&mut *tx)
    .await?;
    let existing_set: HashSet<Uuid> = existing_ids.into_iter().collect();

    let mut new_rows: Vec<&PreparedRow> = Vec::with_capacity(locked_rows.len());
    for row in locked_rows {
        if existing_set.contains(&items[row.index].request_id) {
            results[row.index] = Some(Ok(BatchOutcome::Fallback));
        } else {
            new_rows.push(row);
        }
    }
    if new_rows.is_empty() {
        let _ = tx.rollback().await;
        return Ok(Vec::new());
    }

    let ok = execute_unnest_write(&mut tx, items, &new_rows).await?;
    if !ok {
        let _ = tx.rollback().await;
        for row in new_rows {
            results[row.index] = Some(Ok(BatchOutcome::Fallback));
        }
        return Ok(Vec::new());
    }

    tx.commit().await?;
    Ok(new_rows.into_iter().map(|r| r.index).collect())
}

#[allow(clippy::too_many_lines)]
async fn execute_unnest_write(
    tx: &mut Transaction<'_, Postgres>,
    items: &[OwnedSettlementInput],
    rows: &[&PreparedRow],
) -> Result<bool, LedgerError> {
    let n = rows.len();
    let mut request_ids = Vec::with_capacity(n);
    let mut upstream_request_ids: Vec<Option<String>> = Vec::with_capacity(n);
    let mut log_types = Vec::with_capacity(n);
    let mut user_ids = Vec::with_capacity(n);
    let mut api_key_ids = Vec::with_capacity(n);
    let mut group_codes = Vec::with_capacity(n);
    let mut model_names = Vec::with_capacity(n);
    let mut channel_ids = Vec::with_capacity(n);
    let mut channel_key_ids = Vec::with_capacity(n);
    let mut statuses = Vec::with_capacity(n);
    let mut prompt_tokens = Vec::with_capacity(n);
    let mut cached_tokens = Vec::with_capacity(n);
    let mut completion_tokens = Vec::with_capacity(n);
    let mut reasoning_tokens = Vec::with_capacity(n);
    let mut amount_micros = Vec::with_capacity(n);
    let mut original_micros = Vec::with_capacity(n);
    let mut discount_micros = Vec::with_capacity(n);
    let mut pricing_epochs = Vec::with_capacity(n);
    let mut pricing_snapshots = Vec::with_capacity(n);
    let mut latency_ms = Vec::with_capacity(n);
    let mut ttft_ms = Vec::with_capacity(n);
    let mut is_streams = Vec::with_capacity(n);
    let mut retry_counts = Vec::with_capacity(n);
    let mut failover_counts = Vec::with_capacity(n);
    let mut upstream_statuses = Vec::with_capacity(n);
    let mut error_codes = Vec::with_capacity(n);
    let mut nodes = Vec::with_capacity(n);
    let mut sticky_layers = Vec::with_capacity(n);
    let mut client_types = Vec::with_capacity(n);
    let mut upstream_cost_micros = Vec::with_capacity(n);
    let mut client_ips = Vec::with_capacity(n);
    let mut pools = Vec::with_capacity(n);
    let mut usage_details = Vec::with_capacity(n);
    let mut event_types = Vec::with_capacity(n);
    let mut delta_micros = Vec::with_capacity(n);
    let mut event_payloads = Vec::with_capacity(n);
    let mut outbox_payloads = Vec::with_capacity(n);

    // 严格按主键升序聚合（BTreeMap），消除同一批内与跨批并发更新时的行锁死锁。
    let mut user_deltas: BTreeMap<i64, i64> = BTreeMap::new();
    let mut key_amounts: BTreeMap<i64, i64> = BTreeMap::new();
    let mut channel_aggs: BTreeMap<i64, ChannelAgg> = BTreeMap::new();

    for row in rows {
        let item = &items[row.index];
        request_ids.push(item.request_id);
        upstream_request_ids.push(
            item.upstream_request_id
                .as_deref()
                .map(|id| bounded_upstream_id(id).to_owned()),
        );
        log_types.push(item.log_type);
        user_ids.push(item.user_id);
        api_key_ids.push(item.api_key_id);
        group_codes.push(item.group_code.clone());
        model_names.push(item.model_name.clone());
        channel_ids.push(item.channel_id);
        channel_key_ids.push(item.channel_key_id);
        statuses.push(item.state.as_i16());
        prompt_tokens.push(row.prompt_tokens);
        cached_tokens.push(row.cached_tokens);
        completion_tokens.push(row.completion_tokens);
        reasoning_tokens.push(row.reasoning_tokens);
        amount_micros.push(item.amount.as_micros());
        original_micros.push(item.original.as_micros());
        discount_micros.push(item.discount.as_micros());
        pricing_epochs.push(item.pricing_epoch);
        pricing_snapshots.push(item.pricing_snapshot.clone());
        latency_ms.push(item.latency_ms);
        ttft_ms.push(item.ttft_ms);
        is_streams.push(item.is_stream);
        retry_counts.push(item.retry_count);
        failover_counts.push(item.failover_count);
        upstream_statuses.push(item.upstream_status);
        error_codes.push(item.error_code.clone());
        nodes.push(item.node.clone());
        sticky_layers.push(item.sticky_layer);
        client_types.push(item.client_type.clone());
        upstream_cost_micros.push(item.upstream_cost.map(Money::as_micros));
        client_ips.push(item.client_ip.clone());
        pools.push(item.pool.as_i16());
        usage_details.push(row.usage_details.clone());
        event_types.push(item.event_type.clone());
        delta_micros.push(item.delta_micro);
        event_payloads.push(row.event_payload.clone());
        outbox_payloads.push(row.outbox_payload.clone());

        if item.pool == Pool::Wallet {
            let entry = user_deltas.entry(item.user_id).or_insert(0);
            let Some(next) = entry.checked_add(item.delta_micro) else {
                return Ok(false);
            };
            *entry = next;
        }
        {
            let entry = key_amounts.entry(item.api_key_id).or_insert(0);
            let Some(next) = entry.checked_add(item.amount.as_micros()) else {
                return Ok(false);
            };
            *entry = next;
        }
        if let Some(channel_id) = item.channel_id {
            let agg = channel_aggs.entry(channel_id).or_default();
            let row_tokens =
                i64::from(row.prompt_tokens.max(0)) + i64::from(row.completion_tokens.max(0));
            let row_cost = item.upstream_cost.map_or(0, |m| m.as_micros().max(0));
            let row_unknown = i64::from(item.upstream_cost.is_none());
            let (Some(next_req), Some(next_tok), Some(next_cost), Some(next_unk)) = (
                agg.requests.checked_add(1),
                agg.tokens.checked_add(row_tokens),
                agg.cost_micro.checked_add(row_cost),
                agg.unknown_cost_requests.checked_add(row_unknown),
            ) else {
                return Ok(false);
            };
            agg.requests = next_req;
            agg.tokens = next_tok;
            agg.cost_micro = next_cost;
            agg.unknown_cost_requests = next_unk;
        }
    }

    let expected_users =
        i64::try_from(user_deltas.len()).map_err(|_| LedgerError::InvalidSettlement)?;
    let expected_keys =
        i64::try_from(key_amounts.len()).map_err(|_| LedgerError::InvalidSettlement)?;

    let (agg_user_ids, agg_user_deltas): (Vec<i64>, Vec<i64>) = user_deltas.into_iter().unzip();
    let (agg_key_ids, agg_key_amounts): (Vec<i64>, Vec<i64>) = key_amounts.into_iter().unzip();
    let mut agg_ch_ids = Vec::with_capacity(channel_aggs.len());
    let mut agg_ch_reqs = Vec::with_capacity(channel_aggs.len());
    let mut agg_ch_tokens = Vec::with_capacity(channel_aggs.len());
    let mut agg_ch_costs = Vec::with_capacity(channel_aggs.len());
    let mut agg_ch_unknowns = Vec::with_capacity(channel_aggs.len());
    for (ch_id, agg) in channel_aggs {
        agg_ch_ids.push(ch_id);
        agg_ch_reqs.push(agg.requests);
        agg_ch_tokens.push(agg.tokens);
        agg_ch_costs.push(agg.cost_micro);
        agg_ch_unknowns.push(agg.unknown_cost_requests);
    }

    let (user_rows, key_rows): (i64, i64) = sqlx::query_as(
        r"
        WITH ins_records AS (
            INSERT INTO billing_records (
                request_id, upstream_request_id, log_type, user_id, api_key_id,
                group_code, model_name, channel_id, channel_key_id, status,
                prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
                amount_micro, original_amount_micro, discount_micro,
                pricing_epoch, pricing_snapshot,
                latency_ms, ttft_ms, is_stream, retry_count, failover_count,
                upstream_status, error_code, node, sticky_layer, client_type,
                upstream_cost_micro, client_ip, pool, usage_details, source_window
            )
            SELECT
                r.request_id, r.upstream_request_id, r.log_type, r.user_id, r.api_key_id,
                r.group_code, r.model_name, r.channel_id, r.channel_key_id, r.status,
                r.prompt_tokens, r.cached_tokens, r.completion_tokens, r.reasoning_tokens,
                r.amount_micro, r.original_amount_micro, r.discount_micro,
                r.pricing_epoch, r.pricing_snapshot,
                r.latency_ms, r.ttft_ms, r.is_stream, r.retry_count, r.failover_count,
                r.upstream_status, r.error_code, r.node, r.sticky_layer, r.client_type,
                r.upstream_cost_micro, r.client_ip::inet, r.pool, r.usage_details, NULL::text
            FROM UNNEST(
                $1::uuid[], $2::text[], $3::smallint[], $4::bigint[], $5::bigint[],
                $6::text[], $7::text[], $8::bigint[], $9::bigint[], $10::smallint[],
                $11::integer[], $12::integer[], $13::integer[], $14::integer[],
                $15::bigint[], $16::bigint[], $17::bigint[],
                $18::bigint[], $19::jsonb[],
                $20::integer[], $21::integer[], $22::boolean[], $23::smallint[], $24::smallint[],
                $25::smallint[], $26::text[], $27::text[], $28::smallint[], $29::text[],
                $30::bigint[], $31::text[], $32::smallint[], $33::jsonb[]
            ) AS r(
                request_id, upstream_request_id, log_type, user_id, api_key_id,
                group_code, model_name, channel_id, channel_key_id, status,
                prompt_tokens, cached_tokens, completion_tokens, reasoning_tokens,
                amount_micro, original_amount_micro, discount_micro,
                pricing_epoch, pricing_snapshot,
                latency_ms, ttft_ms, is_stream, retry_count, failover_count,
                upstream_status, error_code, node, sticky_layer, client_type,
                upstream_cost_micro, client_ip, pool, usage_details
            )
            RETURNING 1
        ),
        ins_events AS (
            INSERT INTO billing_events (
                user_id, request_id, event_type, delta_micro, balance_after_micro, payload, actor, pool
            )
            SELECT e.user_id, e.request_id, e.event_type, e.delta_micro, NULL::bigint, e.payload, 'system:gateway', e.pool
              FROM UNNEST($4::bigint[], $1::uuid[], $34::text[], $35::bigint[], $36::jsonb[], $32::smallint[])
                AS e(user_id, request_id, event_type, delta_micro, payload, pool)
            RETURNING 1
        ),
        ins_outbox AS (
            INSERT INTO billing_outbox (topic, payload)
            SELECT 'billing.completed', p
              FROM UNNEST($37::jsonb[]) AS p
            RETURNING 1
        ),
        ins_sync AS (
            INSERT INTO billing_sync (request_id, user_id, api_key_id, amount_micro, pool, source_window)
            SELECT s.request_id, s.user_id, s.api_key_id, s.amount_micro, s.pool, NULL::text
              FROM UNNEST($1::uuid[], $4::bigint[], $5::bigint[], $15::bigint[], $32::smallint[])
                AS s(request_id, user_id, api_key_id, amount_micro, pool)
            RETURNING 1
        ),
        upd_users AS (
            UPDATE users
               SET balance_micro = balance_micro + d.delta,
                   updated_at = now()
              FROM UNNEST($38::bigint[], $39::bigint[]) AS d(id, delta)
             WHERE users.id = d.id
            RETURNING 1
        ),
        upd_keys AS (
            UPDATE api_keys
               SET used_micro = used_micro + d.amount,
                   last_used_at = now()
              FROM UNNEST($40::bigint[], $41::bigint[]) AS d(id, amount)
             WHERE api_keys.id = d.id
            RETURNING 1
        ),
        upd_ch_totals AS (
            INSERT INTO channel_token_totals(channel_id, tokens, cost_micro)
            SELECT c.channel_id, c.tokens, c.cost_micro
              FROM UNNEST($42::bigint[], $44::bigint[], $45::bigint[]) AS c(channel_id, tokens, cost_micro)
            ON CONFLICT(channel_id) DO UPDATE SET
                tokens = channel_token_totals.tokens + EXCLUDED.tokens,
                cost_micro = channel_token_totals.cost_micro + EXCLUDED.cost_micro
            RETURNING 1
        ),
        upd_ch_buckets AS (
            INSERT INTO channel_usage_buckets(channel_id, bucket_start, requests, tokens, cost_micro, unknown_cost_requests)
            SELECT c.channel_id,
                   date_bin(interval '15 minutes', now(), timestamptz '2000-01-01 00:00:00+00'),
                   c.requests, c.tokens, c.cost_micro, c.unknown_cost_requests
              FROM UNNEST($42::bigint[], $43::bigint[], $44::bigint[], $45::bigint[], $46::bigint[])
                AS c(channel_id, requests, tokens, cost_micro, unknown_cost_requests)
            ON CONFLICT(channel_id, bucket_start) DO UPDATE SET
                requests = channel_usage_buckets.requests + EXCLUDED.requests,
                tokens = channel_usage_buckets.tokens + EXCLUDED.tokens,
                cost_micro = channel_usage_buckets.cost_micro + EXCLUDED.cost_micro,
                unknown_cost_requests = channel_usage_buckets.unknown_cost_requests + EXCLUDED.unknown_cost_requests
            RETURNING 1
        )
        SELECT (SELECT count(*) FROM upd_users),
               (SELECT count(*) FROM upd_keys)
        ",
    )
    .bind(&request_ids)
    .bind(&upstream_request_ids)
    .bind(&log_types)
    .bind(&user_ids)
    .bind(&api_key_ids)
    .bind(&group_codes)
    .bind(&model_names)
    .bind(&channel_ids)
    .bind(&channel_key_ids)
    .bind(&statuses)
    .bind(&prompt_tokens)
    .bind(&cached_tokens)
    .bind(&completion_tokens)
    .bind(&reasoning_tokens)
    .bind(&amount_micros)
    .bind(&original_micros)
    .bind(&discount_micros)
    .bind(&pricing_epochs)
    .bind(&pricing_snapshots)
    .bind(&latency_ms)
    .bind(&ttft_ms)
    .bind(&is_streams)
    .bind(&retry_counts)
    .bind(&failover_counts)
    .bind(&upstream_statuses)
    .bind(&error_codes)
    .bind(&nodes)
    .bind(&sticky_layers)
    .bind(&client_types)
    .bind(&upstream_cost_micros)
    .bind(&client_ips)
    .bind(&pools)
    .bind(&usage_details)
    .bind(&event_types)
    .bind(&delta_micros)
    .bind(&event_payloads)
    .bind(&outbox_payloads)
    .bind(&agg_user_ids)
    .bind(&agg_user_deltas)
    .bind(&agg_key_ids)
    .bind(&agg_key_amounts)
    .bind(&agg_ch_ids)
    .bind(&agg_ch_reqs)
    .bind(&agg_ch_tokens)
    .bind(&agg_ch_costs)
    .bind(&agg_ch_unknowns)
    .fetch_one(&mut **tx)
    .await?;

    Ok(user_rows == expected_users && key_rows == expected_keys)
}

async fn finish_redis_batch(
    pg: &PgPool,
    ledger: &BalanceLedger,
    items: &[OwnedSettlementInput],
    committed: &[usize],
) {
    let mut set = JoinSet::new();
    for &idx in committed {
        let item = &items[idx];
        let ledger = ledger.clone();
        let user_id = item.user_id;
        let api_key_id = item.api_key_id;
        let request_id = item.request_id;
        let amount = item.amount;
        let pool = item.pool;
        set.spawn(async move {
            let outcome = ledger
                .commit_in_source(user_id, api_key_id, request_id, amount, pool, None)
                .await;
            (user_id, request_id, pool, outcome)
        });
    }

    let mut synced_ids = Vec::with_capacity(committed.len());
    let mut synced_users = Vec::with_capacity(committed.len());
    let mut recover_users: HashSet<i64> = HashSet::new();

    while let Some(joined) = set.join_next().await {
        if let Ok((user_id, request_id, expected_pool, outcome)) = joined {
            match outcome {
                Ok(CommitOutcome::Committed { pool, .. }) if pool == expected_pool => {
                    synced_ids.push(request_id);
                    synced_users.push(user_id);
                }
                Ok(CommitOutcome::Committed { .. }) => {
                    tracing::error!(%request_id, user_id, "batch settlement pool mismatch; deferring recovery");
                    recover_users.insert(user_id);
                }
                Ok(CommitOutcome::NoReservation) => {
                    recover_users.insert(user_id);
                }
                Err(error) => {
                    tracing::error!(%request_id, user_id, %error, "batch settlement Redis commit failed; deferring recovery");
                    recover_users.insert(user_id);
                }
            }
        }
    }

    if !synced_ids.is_empty()
        && let Err(error) =
            sqlx::query("DELETE FROM billing_sync WHERE request_id = ANY($1::uuid[])")
                .bind(&synced_ids)
                .execute(pg)
                .await
    {
        tracing::error!(count = synced_ids.len(), %error, "batch billing_sync cleanup failed");
        for uid in synced_users {
            let _ = ledger.invalidate_admission_fast_path(uid).await;
        }
    }

    for user_id in recover_users {
        let _ = ledger.invalidate_admission_fast_path(user_id).await;
        match UserGuard::acquire(pg, user_id).await {
            Ok(mut guard) => {
                if let Err(error) = guard.synchronize(ledger).await {
                    tracing::error!(user_id, %error, "batch durable bill awaiting Redis recovery");
                }
            }
            Err(error) => {
                tracing::error!(user_id, %error, "batch recovery lock deferred");
            }
        }
    }
}
