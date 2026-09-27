use super::{
    AppError, AppState, Response, StatusCode, enabled, map_store, not_found, parse_id, public,
    public_id, respond, store,
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::{HeaderMap, Method},
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    #[serde(default = "twenty")]
    limit: u32,
    #[serde(default)]
    cursor: Option<String>,
}
const fn twenty() -> u32 {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(default = "twenty")]
    limit: u32,
    cursor: Option<String>,
    status: Option<String>,
    q: Option<String>,
    created_from: Option<i64>,
    created_before: Option<i64>,
    downloaded: Option<bool>,
}
impl ListQuery {
    fn filters(&self) -> Result<store::Filters<'_>, AppError> {
        let time = |v: Option<i64>| {
            v.map(|v| {
                chrono::DateTime::from_timestamp(v, 0)
                    .filter(|_| v >= 0)
                    .ok_or_else(|| AppError::bad_request().with_param("batch_time"))
            })
            .transpose()
        };
        Ok(store::Filters {
            status: self
                .status
                .clone()
                .map(store::State::try_from)
                .transpose()
                .map_err(map_store)?,
            name: self.q.as_deref(),
            created_from: time(self.created_from)?,
            created_before: time(self.created_before)?,
            downloaded: self.downloaded,
        })
    }
}
pub async fn list(State(state): State<AppState>, req: Request) -> Response {
    let rid = Uuid::new_v4();
    let (parts, _) = req.into_parts();
    let result=async{
        let key=crate::gateway::auth::authenticate_data_plane(&state,&parts.headers).await?;
        let Query(query)=Query::<ListQuery>::try_from_uri(&parts.uri).map_err(|_|AppError::bad_request().with_param("query"))?;
        let cursor=query.cursor.as_deref().map(parse_id).transpose()?;
        let page=store::list_filtered(&state.pg,key.user_id,key.key_id,cursor,query.limit,query.filters()?).await.map_err(map_store)?;
        let next=page.has_more.then(||page.data.last().map(|b|public_id(b.id))).flatten();
        Ok(respond(json!({"object":"list","data":page.data.iter().map(public).collect::<Vec<_>>(),"has_more":page.has_more,"next_cursor":next}),StatusCode::OK,rid))
    }.await;
    result.unwrap_or_else(|e: AppError| e.into_response_with(Some(rid)))
}
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    one(state, id, headers, Action::Read).await
}
pub async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    one(state, id, headers, Action::Cancel).await
}
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    one(state, id, headers, Action::Delete).await
}
enum Action {
    Read,
    Cancel,
    Delete,
}
async fn one(state: AppState, id: String, headers: HeaderMap, action: Action) -> Response {
    let rid = Uuid::new_v4();
    let result = async {
        let key = crate::gateway::auth::authenticate_data_plane(&state, &headers).await?;
        let id = parse_id(&id)?;
        let row = match action {
            Action::Read => store::owned(&state.pg, id, key.user_id, key.key_id).await,
            Action::Cancel => store::cancel(&state.pg, id, key.user_id, key.key_id).await,
            Action::Delete => store::request_delete(&state.pg, id, key.user_id, key.key_id).await,
        }
        .map_err(map_store)?
        .ok_or_else(not_found)?;
        let value = if matches!(action, Action::Delete) {
            json!({"id":public_id(id),"deleted":true,"cleanup_pending":!row.cleanup_done})
        } else {
            public(&row)
        };
        Ok(respond(value, StatusCode::OK, rid))
    }
    .await;
    result.unwrap_or_else(|e: AppError| e.into_response_with(Some(rid)))
}
pub async fn items(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let rid = Uuid::new_v4();
    let (parts, _) = req.into_parts();
    let result=async{
        let key=crate::gateway::auth::authenticate_data_plane(&state,&parts.headers).await?;
        let id=parse_id(&id)?;
        let Query(query)=Query::<PageQuery>::try_from_uri(&parts.uri).map_err(|_|AppError::bad_request().with_param("query"))?;
        let after=query.cursor.as_deref().map(str::parse::<i32>).transpose().map_err(|_|AppError::bad_request().with_param("cursor"))?.unwrap_or(-1);
        let row=store::owned(&state.pg,id,key.user_id,key.key_id).await.map_err(map_store)?.ok_or_else(not_found)?;
        let page=store::items_owned(&state.pg,id,key.user_id,key.key_id,after,query.limit).await.map_err(map_store)?.ok_or_else(not_found)?;
        let next=page.has_more.then(||page.data.last().map(|i|i.ordinal.to_string())).flatten();
        let data:Vec<_>=page.data.into_iter().map(|item|{
            let mut outputs=item.outputs;
            if let Some(images)=outputs.as_array_mut(){for image in images {
                if row.state.terminal()&&row.expires_at.is_some_and(|t|t>chrono::Utc::now())&&image["status"]=="succeeded" {
                    image["url"]=json!(format!("/v1/images/batches/{}/content/{}",public_id(id),image["slot"]));
                }
            }}
            json!({"custom_id":item.custom_id,"prompt_preview":item.prompt_preview,"output_count":item.output_count,"outputs":outputs})
        }).collect();
        Ok(respond(json!({"object":"list","data":data,"has_more":page.has_more,"next_cursor":next}),StatusCode::OK,rid))
    }.await;
    result.unwrap_or_else(|e: AppError| e.into_response_with(Some(rid)))
}
pub async fn content(
    State(state): State<AppState>,
    Path((id, slot)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let rid = Uuid::new_v4();
    let result = async {
        let key = crate::gateway::auth::authenticate_data_plane(&state, &headers).await?;
        let id = parse_id(&id)?;
        let slot = slot.parse::<u32>().map_err(|_| not_found())?;
        if method == Method::HEAD {
            let (len, mime) =
                store::content_info_owned(&state.pg, id, key.user_id, key.key_id, slot)
                    .await
                    .map_err(map_store)?
                    .ok_or_else(not_found)?;
            return Response::builder()
                .header("content-type", mime)
                .header("content-length", len)
                .header("cache-control", "private, no-store")
                .header("content-disposition", "attachment")
                .header("x-content-type-options", "nosniff")
                .body(Body::empty())
                .map(|r| crate::gateway::error::with_request_id(r, rid))
                .map_err(|_| AppError::internal());
        }
        let permit = state
            .image_download_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| {
                AppError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    okapi_api::codes::RATE_LIMITED,
                )
                .with_param("image_download_capacity")
            })?;
        let (data, mime) = store::content_owned(&state.pg, id, key.user_id, key.key_id, slot)
            .await
            .map_err(map_store)?
            .ok_or_else(not_found)?;
        let len = data.len();
        let stream = futures::stream::unfold(
            (Bytes::from(data), permit),
            |(mut bytes, permit)| async move {
                if bytes.is_empty() {
                    None
                } else {
                    let chunk = bytes.split_to(bytes.len().min(64 * 1024));
                    Some((Ok::<_, std::convert::Infallible>(chunk), (bytes, permit)))
                }
            },
        );
        store::mark_downloaded(&state.pg, id, key.user_id, key.key_id)
            .await
            .map_err(map_store)?;
        Response::builder()
            .header("content-type", mime)
            .header("content-length", len)
            .header("cache-control", "private, no-store")
            .header("content-disposition", "attachment")
            .header("x-content-type-options", "nosniff")
            .body(Body::from_stream(stream))
            .map(|r| crate::gateway::error::with_request_id(r, rid))
            .map_err(|_| AppError::internal())
    }
    .await;
    result.unwrap_or_else(|e: AppError| e.into_response_with(Some(rid)))
}
pub async fn models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let rid = Uuid::new_v4();
    let result = async {
        let key = crate::gateway::auth::authenticate_data_plane(&state, &headers).await?;
        if !enabled(&state).await {
            return Err(not_found());
        }
        let names = okapi_store::pricing::list_active_models(&state.pg).await?;
        let mut data = Vec::new();
        for name in names {
            if !key.allows_model(&name)
                || super::super::prepare_model(&state, &key, &name, 1)
                    .await
                    .is_err()
            {
                continue;
            }
            let rows = okapi_store::channels::candidates_for_model(
                &state.pg,
                &name,
                &key.pool_chain(),
                state.master_key.as_deref(),
            )
            .await?;
            let providers: std::collections::BTreeSet<_> = rows
                .iter()
                .filter(|c| super::binding::eligible(c, None))
                .map(|c| c.provider.as_str())
                .collect();
            for provider in providers {
                data.push(json!({"id":name,"object":"model","provider":provider}));
            }
        }
        Ok(respond(
            json!({"object":"list","data":data}),
            StatusCode::OK,
            rid,
        ))
    }
    .await;
    result.unwrap_or_else(|e: AppError| e.into_response_with(Some(rid)))
}
