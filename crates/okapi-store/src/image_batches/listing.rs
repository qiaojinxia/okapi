use super::{Batch, DateTime, Error, Page, PgPool, State, Utc, Uuid, transaction};

#[derive(Default)]
pub struct Filters<'a> {
    pub status: Option<State>,
    pub name: Option<&'a str>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    pub downloaded: Option<bool>,
}
pub async fn list(
    pg: &PgPool,
    uid: i64,
    kid: i64,
    before: Option<Uuid>,
    limit: u32,
) -> Result<Page, Error> {
    list_filtered(pg, uid, kid, before, limit, Filters::default()).await
}
pub async fn list_filtered(
    pg: &PgPool,
    uid: i64,
    kid: i64,
    before: Option<Uuid>,
    limit: u32,
    filter: Filters<'_>,
) -> Result<Page, Error> {
    if !(1..=100).contains(&limit) {
        return Err(Error::Invalid("batch_page_size"));
    }
    if filter.name.is_some_and(|q| {
        q.len() > 1024 || q.chars().count() > 256 || q.chars().any(char::is_control)
    }) || filter
        .created_from
        .zip(filter.created_before)
        .is_some_and(|(a, b)| a >= b)
    {
        return Err(Error::Invalid("batch_filter"));
    }
    let name = filter.name.map(str::trim).filter(|q| !q.is_empty());
    let mut tx = transaction(pg).await?;
    let cursor: Option<(DateTime<Utc>, Uuid)> = if let Some(id) = before {
        Some(sqlx::query_as("SELECT created_at,id FROM image_batches WHERE id=$1 AND user_id=$2 AND api_key_id=$3")
            .bind(id).bind(uid).bind(kid).fetch_optional(&mut *tx).await?.ok_or(Error::Invalid("batch_cursor"))?)
    } else {
        None
    };
    let mut data:Vec<Batch> = sqlx::query_as("SELECT * FROM image_batches WHERE user_id=$1 AND api_key_id=$2 AND NOT delete_requested AND ($3::timestamptz IS NULL OR (created_at,id)<($3,$4)) AND ($6::text IS NULL OR state=$6) AND ($7::text IS NULL OR strpos(lower(task_name),lower($7))>0) AND ($8::timestamptz IS NULL OR created_at >= $8) AND ($9::timestamptz IS NULL OR created_at < $9) AND ($10::boolean IS NULL OR (downloaded_at IS NOT NULL)=$10) ORDER BY created_at DESC,id DESC LIMIT $5")
        .bind(uid).bind(kid).bind(cursor.map(|c|c.0)).bind(cursor.map(|c|c.1)).bind(i64::from(limit)+1).bind(filter.status.map(State::code)).bind(name).bind(filter.created_from).bind(filter.created_before).bind(filter.downloaded).fetch_all(&mut *tx).await?;
    let has_more = data.len() > limit as usize;
    data.truncate(limit as usize);
    tx.commit().await?;
    Ok(Page { data, has_more })
}
