use super::{AppError, AppState, Group, Query, empty_response, paging, snapshot};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, Method},
    response::{IntoResponse, Response},
};
use okapi_store::listing::Slice;
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize)]
pub struct GroupQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    q: Option<String>,
    code: Option<String>,
    model: Option<String>,
}

pub(super) struct Selection {
    pub slice: Slice,
    pub pattern: Option<String>,
    pub code: Option<String>,
    pub model: Option<String>,
}

const FROM: &str = r"FROM price_groups g
    LEFT JOIN channel_pools p ON p.pool_code = g.pool_code
    JOIN jsonb_to_recordset($5::jsonb -> 'groups') AS pg(group_code text, ratio_scaled bigint)
        ON pg.group_code = g.group_code
    WHERE ($1::text IS NULL OR g.group_code ILIKE $1 ESCAPE E'\\' OR g.description ILIKE $1 ESCAPE E'\\')
    AND ($2::text IS NULL OR g.group_code = $2)
    AND (g.self_select OR g.is_default OR EXISTS (
        SELECT 1 FROM user_groups ug WHERE ug.group_code = g.group_code AND ug.user_id = $4
    ))
    AND ($3::text IS NULL OR EXISTS (
        SELECT 1 FROM models m
        JOIN jsonb_to_recordset($5::jsonb -> 'models') AS mp(model_name text)
            ON mp.model_name = m.model_name
        JOIN channels c ON c.models ? m.model_name
        JOIN pool_channels pc ON pc.channel_id = c.id
        WHERE m.model_name = $3 AND m.status = 1 AND c.status = 1 AND c.deleted_at IS NULL
        AND pc.pool_code IN (g.pool_code, p.fallback_pool_code)
    ))";

pub(super) async fn read(
    conn: &mut sqlx::PgConnection,
    selection: &Selection,
    user_id: Option<i64>,
    published: &serde_json::Value,
) -> Result<(Vec<Group>, paging::Meta), AppError> {
    let mut count_query = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT COUNT(*) ");
    count_query.push(FROM);
    let total: i64 = count_query
        .build_query_scalar()
        .bind(&selection.pattern)
        .bind(&selection.code)
        .bind(&selection.model)
        .bind(user_id)
        .bind(published)
        .fetch_one(&mut *conn)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let mut data_query = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT g.group_code AS code, g.description AS name, (pg.ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS ratio, g.pool_code, g.self_select, g.is_default, p.fallback_pool_code ",
    );
    data_query
        .push(FROM)
        .push(" ORDER BY g.sort_order, g.group_code LIMIT $6 OFFSET $7");
    let groups: Vec<Group> = data_query
        .build_query_as()
        .bind(&selection.pattern)
        .bind(&selection.code)
        .bind(&selection.model)
        .bind(user_id)
        .bind(published)
        .bind(selection.slice.capped_limit())
        .bind(selection.slice.offset)
        .fetch_all(conn)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let page = paging::Meta::new(total, selection.slice, groups.len());
    Ok((groups, page))
}

#[derive(Serialize)]
struct Catalog {
    groups: Vec<Group>,
    pricing_epoch: i64,
    #[serde(flatten)]
    page: paging::Meta,
}

/// Searchable public groups, optionally restricted to one configured model's pool chain.
pub async fn public_groups(
    State(state): State<AppState>,
    Query(query): Query<GroupQuery>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response, AppError> {
    let selection = Selection {
        slice: paging::bounded(query.limit, query.offset, "limit", "offset")?,
        pattern: paging::pattern(paging::trimmed(query.q, 256, "q")?),
        code: paging::trimmed(query.code, 32, "code")?,
        model: paging::trimmed(query.model, 256, "model")?,
    };
    let user_id = super::viewer(&state, &headers).await?;
    let mut tx = snapshot(&state).await?;
    let publication = okapi_store::pricing::published_pricing(&mut tx).await?;
    let published = serde_json::to_value(&publication.source).map_err(|_| AppError::internal())?;
    let (groups, page) = read(&mut tx, &selection, user_id, &published).await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    let body = Catalog {
        groups,
        page,
        pricing_epoch: publication.source.epoch,
    };
    let mut response = if method == Method::HEAD {
        empty_response()
    } else {
        Json(&body).into_response()
    };
    body.page.headers(&mut response)?;
    super::private_response(&mut response);
    Ok(response)
}
