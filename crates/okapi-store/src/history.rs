//! Retention carries financial facts before dropping detailed partitions.
//! Readers of combined history acquire the shared transaction lock before taking
//! their SQL snapshot. Each partition archive/drop is one exclusive transaction.
use crate::StoreError;
use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use sqlx::{Connection, PgConnection, PgPool, Postgres, Transaction};

const RETENTION_LOCK: i64 = 0x4F4B_4849_5354;

pub async fn read_lock(tx: &mut Transaction<'_, Postgres>) -> Result<(), StoreError> {
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut **tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut **tx)
        .await?;
    sqlx::query!("SELECT pg_advisory_xact_lock_shared($1)", RETENTION_LOCK)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
pub async fn read(pg: &PgPool) -> Result<Transaction<'static, Postgres>, StoreError> {
    let mut tx = pg.begin().await?;
    read_lock(&mut tx).await?;
    Ok(tx)
}
#[derive(Debug, Clone, Copy)]
pub struct Totals {
    pub wallet: i64,
    pub subscription: i64,
}
pub async fn totals(connection: &mut PgConnection, user_id: i64) -> Result<Totals, StoreError> {
    let mut tx = connection.begin().await?;
    read_lock(&mut tx).await?;
    let totals = sqlx::query!(
        r#"SELECT COALESCE(SUM(delta_micro) FILTER (WHERE pool=0),0)::bigint AS "wallet!",
        COALESCE(SUM(delta_micro) FILTER (WHERE pool=1),0)::bigint AS "subscription!"
        FROM billing_balance_totals WHERE user_id=$1"#,
        user_id
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Totals {
        wallet: totals.wallet,
        subscription: totals.subscription,
    })
}

struct Partition {
    oid: i64,
    parent: String,
    name: String,
    schema: String,
    qualified: String,
    definition: Option<String>,
}
async fn partitions(
    connection: &mut PgConnection,
    name: Option<&str>,
) -> Result<Vec<Partition>, StoreError> {
    Ok(sqlx::query_as!(Partition,r#"SELECT c.oid::bigint AS "oid!",p.relname::text AS "parent!",c.relname::text AS "name!",
        n.nspname::text AS "schema!",format('%I.%I',n.nspname,c.relname) AS "qualified!",
        pg_get_expr(c.relpartbound,c.oid) AS "definition?"
        FROM pg_inherits i JOIN pg_class c ON c.oid=i.inhrelid JOIN pg_class p ON p.oid=i.inhparent
        JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE p.oid IN ('billing_records'::regclass,'billing_events'::regclass,'audit_logs'::regclass)
          AND c.relnamespace=p.relnamespace AND c.relkind='r' AND c.relispartition
          AND ($1::text IS NULL OR c.relname=$1)
        ORDER BY c.relname"#,name).fetch_all(connection).await?)
}
fn bounds(part: &Partition, keep_from: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let prefix = format!("{}_y", part.parent);
    let (year, month) = part.name.strip_prefix(&prefix)?.split_once('m')?;
    if year.len() != 4
        || month.len() != 2
        || !year
            .bytes()
            .chain(month.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let start = NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, 1)?
        .and_hms_opt(0, 0, 0)?
        .and_utc();
    let end = start.checked_add_months(Months::new(1))?;
    let expected = format!(
        "FOR VALUES FROM ('{} 00:00:00+00') TO ('{} 00:00:00+00')",
        start.format("%Y-%m-%d"),
        end.format("%Y-%m-%d")
    );
    (end <= keep_from && part.definition.as_deref() == Some(expected.as_str()))
        .then_some((start, end))
}

/// Keeps the current month and preceding months-1 whole calendar months.
pub async fn prune(pg: &PgPool, now: DateTime<Utc>) -> Result<Vec<String>, StoreError> {
    let months = sqlx::query_scalar!(
        r#"SELECT (value #>> '{}')::bigint AS "v!" FROM settings WHERE key='retention_months'"#
    )
    .fetch_optional(pg)
    .await?
    .unwrap_or(0);
    if months <= 0 {
        return Ok(Vec::new());
    }
    let Some(keep_from) = u32::try_from(months - 1).ok().and_then(|months| {
        now.date_naive()
            .with_day(1)?
            .checked_sub_months(Months::new(months))?
            .and_hms_opt(0, 0, 0)
            .map(|v| v.and_utc())
    }) else {
        return Ok(Vec::new());
    };
    let candidates = {
        // Deparsing partition bounds consults the live catalog even when the
        // query's snapshot still contains a concurrently dropped partition.
        // Fence other pruners during discovery, then release before taking the
        // exclusive archive lock. External DDL can still make bounds absent;
        // archive_one rechecks identity and bounds before carrying or dropping.
        let mut tx = read(pg).await?;
        let candidates = partitions(&mut tx, None).await?;
        tx.commit().await?;
        candidates
    };
    let mut dropped = Vec::new();
    for candidate in candidates {
        if archive_one(pg, &candidate, keep_from).await? {
            dropped.push(candidate.name);
        }
    }
    Ok(dropped)
}

async fn archive_one(
    pg: &PgPool,
    candidate: &Partition,
    keep_from: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let mut tx = pg.begin().await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout='10s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL DateStyle='ISO,YMD'")
        .execute(&mut *tx)
        .await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", RETENTION_LOCK)
        .execute(&mut *tx)
        .await?;
    let Some(found) = partitions(&mut tx, Some(&candidate.name))
        .await?
        .into_iter()
        .find(|part| {
            part.schema == candidate.schema
                && part.parent == candidate.parent
                && part.oid == candidate.oid
        })
    else {
        return Ok(false);
    };
    if bounds(&found, keep_from).is_none() {
        return Ok(false);
    }
    // qualified is produced by PostgreSQL format('%I.%I',...), never client text.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "LOCK TABLE ONLY {} IN ACCESS EXCLUSIVE MODE",
        found.qualified
    )))
    .execute(&mut *tx)
    .await?;
    // An external DDL may have detached/replaced it while the lock was awaited.
    let Some(found) = partitions(&mut tx, Some(&found.name))
        .await?
        .into_iter()
        .find(|part| part.oid == found.oid)
    else {
        return Ok(false);
    };
    let Some((start, end)) = bounds(&found, keep_from) else {
        return Ok(false);
    };
    match found.parent.as_str() {
        "billing_events" => carry_events(&mut tx, found.oid, start, end).await?,
        "billing_records" => carry_records(&mut tx, found.oid, start, end).await?,
        "audit_logs" => {}
        _ => return Err(StoreError::InvalidData("retention_parent")),
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP TABLE {}",
        found.qualified
    )))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}
async fn carry_events(
    tx: &mut Transaction<'_, Postgres>,
    oid: i64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(), StoreError> {
    sqlx::query!("INSERT INTO billing_event_carry(user_id,pool,actor,event_type,delta_micro,event_count)
        SELECT user_id,pool,actor,event_type,SUM(delta_micro),COUNT(*) FROM billing_events
        WHERE tableoid=$1::bigint::oid AND created_at >= $2 AND created_at < $3 GROUP BY user_id,pool,actor,event_type
        ON CONFLICT(user_id,pool,actor,event_type) DO UPDATE SET
          delta_micro=billing_event_carry.delta_micro+EXCLUDED.delta_micro,
          event_count=billing_event_carry.event_count+EXCLUDED.event_count",oid,start,end).execute(&mut **tx).await?;
    Ok(())
}
async fn carry_records(
    tx: &mut Transaction<'_, Postgres>,
    oid: i64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(), StoreError> {
    // A conflicting request ID is corruption, not permission to silently forget
    // either financial fact. A unique violation rolls back both archive and DROP.
    sqlx::query!("INSERT INTO billing_record_receipts(request_id,user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,
        status,amount_micro,original_amount_micro,discount_micro,upstream_cost_micro,is_stream,node,pool,pricing_snapshot,usage_details,created_at,source_window)
        SELECT request_id,user_id,api_key_id,group_code,model_name,channel_id,channel_key_id,status,amount_micro,original_amount_micro,
          discount_micro,upstream_cost_micro,is_stream,node,pool,pricing_snapshot,usage_details,created_at,source_window FROM billing_records
        WHERE tableoid=$1::bigint::oid AND created_at >= $2 AND created_at < $3",oid,start,end).execute(&mut **tx).await?;
    Ok(())
}
