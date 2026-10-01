//! Public catalog with bounded, independent model/group/vendor pages.
use super::{AppError, AppState};
use crate::console::query::Query;
use crate::gateway::ingress::Ingress;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, Method, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{
    Value,
    value::{RawValue, to_raw_value},
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

mod groups;
mod paging;
mod query;
pub use groups::public_groups;
pub use query::public_statistics;
use query::{CatalogQuery, PageMeta, Selection};

#[cfg(test)]
mod tests;

#[derive(sqlx::FromRow, Serialize)]
struct PricingRow {
    #[serde(rename = "model")]
    model_name: String,
    display_name: Option<String>,
    vendor: Option<String>,
    capabilities: Value,
    context_window: Option<i32>,
    max_output: Option<i32>,
    catalog_config: Value,
    #[serde(rename = "mode")]
    pricing_mode: String,
    model_ratio: Option<String>,
    completion_ratio: Option<String>,
    cache_ratio: Option<String>,
    cache_write_ratio: Option<String>,
    audio_ratio: Option<String>,
    audio_completion_ratio: Option<String>,
    image_ratio: Option<String>,
    modality_ratios: Option<Value>,
    per_call_price_micro: Option<i64>,
}
#[derive(sqlx::FromRow)]
struct Route {
    model_name: String,
    pool_code: String,
    provider: String,
    upstream_model: String,
    responses_native: Option<bool>,
    capabilities: Value,
}
#[derive(sqlx::FromRow, Serialize)]
struct Group {
    code: String,
    name: Option<String>,
    ratio: String,
    #[serde(skip)]
    pool_code: String,
    self_select: bool,
    is_default: bool,
    #[serde(skip)]
    fallback_pool_code: Option<String>,
}
struct Visibility {
    groups: Box<RawValue>,
    endpoints: Box<RawValue>,
}
#[derive(Serialize)]
struct PublicModel<'a> {
    base_price_per_1m_micro: i64,
    #[serde(flatten)]
    model: &'a PricingRow,
    groups: &'a RawValue,
    chat_endpoints_by_group: &'a RawValue,
}
#[derive(Serialize)]
struct Catalog<'a> {
    models: Vec<PublicModel<'a>>,
    pricing_epoch: i64,
    groups: &'a [Group],
    #[serde(flatten)]
    page: &'a PageMeta,
    groups_page: &'a paging::Meta,
}

type Signature = Vec<(String, u8)>;
fn signatures(routes: &[Route]) -> HashMap<&str, BTreeMap<&str, u8>> {
    let mut index: HashMap<&str, BTreeMap<&str, u8>> = HashMap::new();
    for route in routes {
        let native =
            okapi_store::channels::responses_native_for(&route.provider, route.responses_native);
        let mask = Ingress::ALL
            .into_iter()
            .enumerate()
            .fold(0, |mask, (i, ingress)| {
                if ingress.supports(
                    &route.provider,
                    &route.upstream_model,
                    native,
                    &route.capabilities,
                ) {
                    mask | (1 << i)
                } else {
                    mask
                }
            });
        *index
            .entry(&route.model_name)
            .or_default()
            .entry(&route.pool_code)
            .or_default() |= mask;
    }
    index
}
fn visibility(signature: &Signature, groups: &[Group]) -> Result<Visibility, AppError> {
    let pools: BTreeMap<_, _> = signature.iter().map(|(p, m)| (p.as_str(), *m)).collect();
    let mut usable = Vec::new();
    let mut endpoints = BTreeMap::new();
    for group in groups {
        let primary = pools.get(group.pool_code.as_str()).copied();
        let fallback = group
            .fallback_pool_code
            .as_deref()
            .and_then(|p| pools.get(p))
            .copied();
        if primary.is_some() || fallback.is_some() {
            usable.push(&group.code);
        }
        let mask = primary.unwrap_or(0) | fallback.unwrap_or(0);
        let supported: Vec<_> = Ingress::ALL
            .into_iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, v)| v.endpoint())
            .collect();
        endpoints.insert(&group.code, supported);
    }
    Ok(Visibility {
        groups: to_raw_value(&usable).map_err(|_| AppError::internal())?,
        endpoints: to_raw_value(&endpoints).map_err(|_| AppError::internal())?,
    })
}

async fn routes(
    conn: &mut sqlx::PgConnection,
    models: Option<&[String]>,
    pools: Option<&[String]>,
) -> Result<Vec<Route>, AppError> {
    Ok(sqlx::query_as(
        r"SELECT DISTINCT mn.name AS model_name, pc.pool_code,
                  c.provider, COALESCE(c.model_mapping ->> mn.name, mn.name) AS upstream_model,
                  (c.settings ->> 'responses_native')::boolean AS responses_native, c.capabilities
           FROM channels c
           CROSS JOIN LATERAL jsonb_array_elements_text(c.models) AS mn(name)
           JOIN pool_channels pc ON pc.channel_id = c.id
           WHERE c.status = 1 AND c.deleted_at IS NULL
             AND ($1::text[] IS NULL OR (c.models ?| $1 AND mn.name = ANY($1)))
             AND ($2::text[] IS NULL OR pc.pool_code = ANY($2))",
    )
    .bind(models)
    .bind(pools)
    .fetch_all(conn)
    .await
    .map_err(okapi_store::StoreError::from)?)
}

/// Both public entry points default to bounded pages; paged=false is not an unbounded export.
pub async fn public_pricing(
    State(state): State<AppState>,
    Query(query): Query<CatalogQuery>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response, AppError> {
    fetch(
        &state,
        query.validate()?,
        viewer(&state, &headers).await?,
        method,
    )
    .await
}

/// Public configured availability, with server-side filters and default 20/max 100 models.
pub async fn public_models(
    State(state): State<AppState>,
    Query(query): Query<CatalogQuery>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response, AppError> {
    fetch(
        &state,
        query.validate()?,
        viewer(&state, &headers).await?,
        method,
    )
    .await
}

// Match the portal's API-key identity. No credentials means public tiers only;
// invalid credentials must never silently downgrade to an anonymous response.
async fn viewer(state: &AppState, headers: &HeaderMap) -> Result<Option<i64>, AppError> {
    if [
        header::AUTHORIZATION.as_str(),
        "x-api-key",
        "x-goog-api-key",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
    {
        return Ok(Some(super::authenticate(state, headers).await?.user_id));
    }
    Ok(None)
}

fn private_response(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    response.headers_mut().insert(
        header::VARY,
        "Authorization, x-api-key, x-goog-api-key".parse().unwrap(),
    );
}

async fn snapshot(
    state: &AppState,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, AppError> {
    let mut tx = state
        .pg
        .begin()
        .await
        .map_err(okapi_store::StoreError::from)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(okapi_store::StoreError::from)?;
    Ok(tx)
}

fn empty_response() -> Response {
    let empty = futures::stream::empty::<Result<bytes::Bytes, std::convert::Infallible>>();
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        axum::body::Body::from_stream(empty),
    )
        .into_response()
}

async fn fetch(
    state: &AppState,
    mut selection: Selection,
    user_id: Option<i64>,
    method: Method,
) -> Result<Response, AppError> {
    selection.filter.user_id = user_id;
    let mut tx = snapshot(state).await?;
    let publication = okapi_store::pricing::published_pricing(&mut tx).await?;
    let published = serde_json::to_value(&publication.source).map_err(|_| AppError::internal())?;
    let (groups, groups_page) =
        groups::read(&mut tx, &selection.groups, user_id, &published).await?;
    let mut models = query::models(
        &mut tx,
        &selection.filter,
        selection.slice,
        &selection.sort,
        &published,
    )
    .await?;
    let page = query::metadata(&mut tx, &selection, models.len(), &published).await?;
    let names: Vec<_> = models.iter().map(|m| m.model_name.clone()).collect();
    let pools: Vec<_> = groups
        .iter()
        .flat_map(|g| std::iter::once(g.pool_code.clone()).chain(g.fallback_pool_code.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let served = if method == Method::HEAD || names.is_empty() || pools.is_empty() {
        Vec::new()
    } else {
        routes(&mut tx, Some(&names), Some(&pools)).await?
    };
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    let mut response = if method == Method::HEAD {
        empty_response()
    } else {
        let base = crate::gateway::pricing_loader::publication_base_price(&publication)?;
        catalog(
            &mut models,
            &groups,
            &served,
            &page,
            &groups_page,
            base,
            publication.source.epoch,
        )?
    };
    page.page.headers(&mut response)?;
    private_response(&mut response);
    Ok(response)
}

fn catalog(
    models: &mut [PricingRow],
    groups: &[Group],
    served: &[Route],
    page: &PageMeta,
    groups_page: &paging::Meta,
    base_price_per_1m_micro: i64,
    pricing_epoch: i64,
) -> Result<Response, AppError> {
    let index = signatures(served);
    let mut cache = HashMap::<Signature, Visibility>::new();
    let model_signatures: Vec<Signature> = models
        .iter()
        .map(|m| {
            index
                .get(m.model_name.as_str())
                .into_iter()
                .flat_map(|pools| pools.iter().map(|(p, m)| (p.to_string(), *m)))
                .collect()
        })
        .collect();
    for signature in &model_signatures {
        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(signature.clone()) {
            entry.insert(visibility(signature, groups)?);
        }
    }
    for model in models.iter_mut() {
        model.capabilities = Value::Object(
            okapi_store::model_config::CAPABILITIES
                .iter()
                .copied()
                .filter_map(|key| {
                    model
                        .capabilities
                        .get(key)
                        .and_then(Value::as_bool)
                        .map(|v| (key.to_owned(), Value::Bool(v)))
                })
                .collect(),
        );
        model.context_window = model.context_window.filter(|v| *v > 0);
        model.max_output = model.max_output.filter(|v| *v > 0);
    }
    let data = models
        .iter()
        .zip(&model_signatures)
        .map(|(model, signature)| {
            let v = &cache[signature];
            PublicModel {
                base_price_per_1m_micro,
                model,
                groups: &v.groups,
                chat_endpoints_by_group: &v.endpoints,
            }
        })
        .collect();
    Ok(Json(Catalog {
        models: data,
        pricing_epoch,
        groups,
        page,
        groups_page,
    })
    .into_response())
}
