use super::paging::{Meta, bounded, pattern, trimmed};
use super::{AppError, Ingress, PricingRow};
use okapi_store::listing::Slice;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

#[derive(Default, Deserialize)]
pub struct CatalogQuery {
    #[serde(default)]
    #[serde(rename = "paged")]
    _paged: bool,
    limit: Option<i64>,
    offset: Option<i64>,
    q: Option<String>,
    vendor: Option<String>,
    model: Option<String>,
    capability: Option<String>,
    group: Option<String>,
    endpoint: Option<String>,
    mode: Option<String>,
    available: Option<bool>,
    availability_group: Option<String>,
    vendors: Option<String>,
    sort: Option<String>,
    group_limit: Option<i64>,
    group_offset: Option<i64>,
    group_q: Option<String>,
    vendor_limit: Option<i64>,
    vendor_offset: Option<i64>,
    vendor_q: Option<String>,
}

pub(super) struct Selection {
    pub slice: Slice,
    pub filter: Filter,
    pub groups: super::groups::Selection,
    pub vendor_slice: Slice,
    pub vendor_pattern: Option<String>,
    pub sort: Sort,
}

#[derive(Default)]
pub(super) enum Sort {
    #[default]
    Configured,
    Name,
    Input,
    Output,
    Context,
}

#[derive(Default)]
pub(super) struct Filter {
    pub user_id: Option<i64>,
    pattern: Option<String>,
    vendor: Option<String>,
    capability: Option<String>,
    model: Option<String>,
    pub group: Option<String>,
    endpoint: Option<&'static str>,
    mode: Option<String>,
    available: bool,
    vendors: Option<Vec<String>>,
    price_group: Option<String>,
}

impl CatalogQuery {
    pub(super) fn validate(self) -> Result<Selection, AppError> {
        let slice = bounded(self.limit, self.offset, "limit", "offset")?;
        let capability = trimmed(self.capability, 32, "capability")?;
        if capability
            .as_deref()
            .is_some_and(|c| !okapi_store::model_config::CAPABILITIES.contains(&c))
        {
            return Err(AppError::bad_request().with_param("capability"));
        }
        let endpoint = self
            .endpoint
            .map(|value| {
                Ingress::ALL
                    .into_iter()
                    .find(|v| v.endpoint() == value.trim())
                    .ok_or_else(|| AppError::bad_request().with_param("endpoint"))
            })
            .transpose()?;
        let group = trimmed(self.group, 32, "group")?;
        let mode = trimmed(self.mode, 32, "mode")?;
        if mode
            .as_deref()
            .is_some_and(|v| !["ratio", "per_call", "tiered"].contains(&v))
        {
            return Err(AppError::bad_request().with_param("mode"));
        }
        let sort = match self.sort.as_deref() {
            None => Sort::Configured,
            Some("name") => Sort::Name,
            Some("input") => Sort::Input,
            Some("output") => Sort::Output,
            Some("context") => Sort::Context,
            _ => return Err(AppError::bad_request().with_param("sort")),
        };
        let vendors = vendor_aliases(self.vendors)?;
        Ok(Selection {
            slice,
            sort,
            groups: super::groups::Selection {
                slice: bounded(
                    self.group_limit,
                    self.group_offset,
                    "group_limit",
                    "group_offset",
                )?,
                pattern: pattern(trimmed(self.group_q, 256, "group_q")?),
                code: group.clone(),
                model: None,
            },
            vendor_slice: bounded(
                self.vendor_limit,
                self.vendor_offset,
                "vendor_limit",
                "vendor_offset",
            )?,
            vendor_pattern: pattern(trimmed(self.vendor_q, 128, "vendor_q")?),
            filter: Filter {
                user_id: None,
                pattern: pattern(trimmed(self.q, 256, "q")?),
                vendor: trimmed(self.vendor, 128, "vendor")?,
                model: trimmed(self.model, 256, "model")?,
                capability,
                group,
                endpoint: endpoint.map(Ingress::endpoint),
                mode,
                available: self.available.unwrap_or(false),
                vendors,
                price_group: trimmed(self.availability_group, 32, "availability_group")?,
            },
        })
    }
}

// Shared by page, total and vendor facets. Values are always bound parameters.
const FROM: &str = include_str!("filter.sql");

pub(super) async fn models(
    conn: &mut PgConnection,
    filter: &Filter,
    slice: Slice,
    sort: &Sort,
    published: &serde_json::Value,
) -> Result<Vec<PricingRow>, AppError> {
    let mut sql = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        r"SELECT m.model_name, m.display_name, m.vendor, p.pricing_mode,
        m.capabilities, m.context_window, m.max_output, m.catalog_config,
        (p.model_ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS model_ratio,
        (p.completion_ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS completion_ratio,
        (p.cache_ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS cache_ratio,
        (p.cache_write_ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS cache_write_ratio,
        (p.audio_ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS audio_ratio,
        (p.audio_completion_ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS audio_completion_ratio,
        (p.image_ratio_scaled / 1000000::numeric)::numeric(12,6)::text AS image_ratio,
        p.modality_ratios, p.server_tool_prices, p.per_call_price_micro
        ",
    );
    sql.push(FROM)
        .push(order(sort))
        .push(" LIMIT $13 OFFSET $14");
    Ok(sql
        .build_query_as()
        .bind(&filter.pattern)
        .bind(&filter.vendor)
        .bind(&filter.capability)
        .bind(&filter.model)
        .bind(&filter.group)
        .bind(filter.endpoint)
        .bind(filter.user_id)
        .bind(published)
        .bind(&filter.mode)
        .bind(filter.available)
        .bind(&filter.vendors)
        .bind(&filter.price_group)
        .bind(slice.capped_limit())
        .bind(slice.offset)
        .fetch_all(conn)
        .await
        .map_err(okapi_store::StoreError::from)?)
}

#[derive(Serialize, sqlx::FromRow)]
pub(super) struct VendorCount {
    vendor: Option<String>,
    count: i64,
}

#[derive(Serialize)]
pub(super) struct PageMeta {
    #[serde(flatten)]
    pub page: Meta,
    vendors: Vec<VendorCount>,
    vendors_page: Meta,
}

pub(super) async fn metadata(
    conn: &mut PgConnection,
    selection: &Selection,
    count: usize,
    published: &serde_json::Value,
) -> Result<PageMeta, AppError> {
    let filter = &selection.filter;
    let mut count_query = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT COUNT(*) ");
    count_query.push(FROM);
    let total: i64 = count_query
        .build_query_scalar()
        .bind(&filter.pattern)
        .bind(&filter.vendor)
        .bind(&filter.capability)
        .bind(&filter.model)
        .bind(&filter.group)
        .bind(filter.endpoint)
        .bind(filter.user_id)
        .bind(published)
        .bind(&filter.mode)
        .bind(filter.available)
        .bind(&filter.vendors)
        .bind(&filter.price_group)
        .fetch_one(&mut *conn)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let (vendors, vendors_page) = vendors(conn, selection, published).await?;
    Ok(PageMeta {
        page: Meta::new(total, selection.slice, count),
        vendors,
        vendors_page,
    })
}

async fn vendors(
    conn: &mut PgConnection,
    selection: &Selection,
    published: &serde_json::Value,
) -> Result<(Vec<VendorCount>, Meta), AppError> {
    let filter = &selection.filter;
    // Keep all model filters except the selected vendor; facet search and paging are independent.
    let mut count_query = vendor_sql(true);
    let total: i64 = count_query
        .build_query_scalar()
        .bind(&filter.pattern)
        .bind(Option::<&str>::None)
        .bind(&filter.capability)
        .bind(&filter.model)
        .bind(&filter.group)
        .bind(filter.endpoint)
        .bind(filter.user_id)
        .bind(published)
        .bind(&filter.mode)
        .bind(filter.available)
        .bind(Option::<Vec<String>>::None)
        .bind(&filter.price_group)
        .bind(&selection.vendor_pattern)
        .fetch_one(&mut *conn)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let mut data_query = vendor_sql(false);
    let vendors: Vec<VendorCount> = data_query
        .build_query_as()
        .bind(&filter.pattern)
        .bind(Option::<&str>::None)
        .bind(&filter.capability)
        .bind(&filter.model)
        .bind(&filter.group)
        .bind(filter.endpoint)
        .bind(filter.user_id)
        .bind(published)
        .bind(&filter.mode)
        .bind(filter.available)
        .bind(Option::<Vec<String>>::None)
        .bind(&filter.price_group)
        .bind(&selection.vendor_pattern)
        .bind(selection.vendor_slice.capped_limit())
        .bind(selection.vendor_slice.offset)
        .fetch_all(conn)
        .await
        .map_err(okapi_store::StoreError::from)?;
    let page = Meta::new(total, selection.vendor_slice, vendors.len());
    Ok((vendors, page))
}

fn vendor_sql(count: bool) -> sqlx::QueryBuilder<sqlx::Postgres> {
    let mut sql = sqlx::QueryBuilder::new(if count { "SELECT COUNT(*) FROM (" } else { "" });
    sql.push("SELECT NULLIF(lower(btrim(m.vendor)), '') AS vendor, COUNT(*) AS count ")
        .push(FROM)
        .push(r" AND ($13::text IS NULL OR m.vendor ILIKE $13 ESCAPE E'\\') GROUP BY 1");
    sql.push(if count {
        ") facets"
    } else {
        " ORDER BY 1 NULLS LAST LIMIT $14 OFFSET $15"
    });
    sql
}

fn order(sort: &Sort) -> &'static str {
    match sort {
        Sort::Configured => " ORDER BY m.sort_order, m.model_name",
        Sort::Name => {
            " ORDER BY lower(COALESCE(NULLIF(m.display_name, ''), m.model_name)), m.model_name"
        }
        Sort::Context => {
            " ORDER BY NULLIF(GREATEST(m.context_window, 0), 0) DESC NULLS LAST, lower(COALESCE(NULLIF(m.display_name, ''), m.model_name)), m.model_name"
        }
        Sort::Input => include_str!("sort-input.sql"),
        Sort::Output => include_str!("sort-output.sql"),
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct Statistics {
    total: i64,
    has_context: bool,
    capabilities: Vec<String>,
}

#[derive(Serialize)]
struct StatisticsPage {
    #[serde(flatten)]
    summary: Statistics,
    vendors: Vec<VendorCount>,
    vendors_page: Meta,
    pricing_epoch: i64,
}

/// Counts/facets only: never expand model records or their channel routes.
pub async fn public_statistics(
    axum::extract::State(state): axum::extract::State<super::AppState>,
    super::Query(query): super::Query<CatalogQuery>,
    headers: axum::http::HeaderMap,
    method: axum::http::Method,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    let mut selection = query.validate()?;
    selection.filter.user_id = super::viewer(&state, &headers).await?;
    let mut tx = super::snapshot(&state).await?;
    let publication = okapi_store::pricing::published_pricing(&mut tx).await?;
    let published = serde_json::to_value(&publication.source).map_err(|_| AppError::internal())?;
    let summary = statistics(&mut tx, &selection.filter, &published).await?;
    let (vendors, vendors_page) = vendors(&mut tx, &selection, &published).await?;
    tx.commit().await.map_err(okapi_store::StoreError::from)?;
    let body = StatisticsPage {
        summary,
        vendors,
        vendors_page,
        pricing_epoch: publication.source.epoch,
    };
    let mut response = if method == axum::http::Method::HEAD {
        super::empty_response()
    } else {
        axum::Json(body).into_response()
    };
    super::private_response(&mut response);
    Ok(response)
}

async fn statistics(
    conn: &mut PgConnection,
    filter: &Filter,
    published: &serde_json::Value,
) -> Result<Statistics, AppError> {
    let mut sql = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "WITH visible AS (SELECT m.context_window, m.capabilities ",
    );
    sql.push(FROM).push(
        ") SELECT COUNT(*) AS total, COALESCE(BOOL_OR(context_window > 0), false) AS has_context,
         ARRAY(SELECT DISTINCT c.key FROM visible v CROSS JOIN LATERAL jsonb_each(v.capabilities) c
             WHERE c.value = 'true'::jsonb AND c.key = ANY($13::text[]) ORDER BY c.key) AS capabilities FROM visible",
    );
    Ok(sql
        .build_query_as()
        .bind(&filter.pattern)
        .bind(&filter.vendor)
        .bind(&filter.capability)
        .bind(&filter.model)
        .bind(&filter.group)
        .bind(filter.endpoint)
        .bind(filter.user_id)
        .bind(published)
        .bind(&filter.mode)
        .bind(filter.available)
        .bind(&filter.vendors)
        .bind(&filter.price_group)
        .bind(okapi_store::model_config::CAPABILITIES)
        .fetch_one(conn)
        .await
        .map_err(okapi_store::StoreError::from)?)
}

fn vendor_aliases(value: Option<String>) -> Result<Option<Vec<String>>, AppError> {
    let aliases = value
        .map(|v| {
            serde_json::from_str::<Vec<String>>(&v)
                .map_err(|_| AppError::bad_request().with_param("vendors"))
        })
        .transpose()?;
    if aliases
        .as_ref()
        .is_some_and(|v| v.is_empty() || v.len() > 128 || v.iter().any(|s| s.chars().count() > 128))
    {
        return Err(AppError::bad_request().with_param("vendors"));
    }
    Ok(aliases)
}
