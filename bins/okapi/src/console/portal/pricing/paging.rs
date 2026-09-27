use super::AppError;
use axum::{http::HeaderValue, response::Response};
use okapi_store::listing::{Slice, len_as_total};
use serde::Serialize;

pub(super) fn bounded(
    limit: Option<i64>,
    offset: Option<i64>,
    limit_key: &str,
    offset_key: &str,
) -> Result<Slice, AppError> {
    if limit.is_some_and(|v| v < 1) {
        return Err(AppError::bad_request().with_param(limit_key));
    }
    if offset.is_some_and(|v| v < 0) {
        return Err(AppError::bad_request().with_param(offset_key));
    }
    Ok(Slice::new(
        Some(limit.unwrap_or(20).min(100)),
        offset.unwrap_or(0),
    ))
}

pub(super) fn trimmed(
    value: Option<String>,
    max: usize,
    param: &str,
) -> Result<Option<String>, AppError> {
    let value = value.map(|s| s.trim().to_owned());
    if value.as_ref().is_some_and(|s| s.chars().count() > max) {
        return Err(AppError::bad_request().with_param(param));
    }
    Ok(value)
}

pub(super) fn pattern(value: Option<String>) -> Option<String> {
    value.filter(|q| !q.is_empty()).map(|q| {
        format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    })
}

#[derive(Serialize)]
pub(super) struct Meta {
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    has_more: bool,
    next_offset: Option<i64>,
}
impl Meta {
    pub fn new(total: i64, slice: Slice, count: usize) -> Self {
        let next = slice.offset.saturating_add(len_as_total(count));
        Self {
            total,
            limit: slice.capped_limit(),
            offset: slice.offset,
            has_more: next < total,
            next_offset: (next < total).then_some(next),
        }
    }
    pub fn headers(&self, response: &mut Response) -> Result<(), AppError> {
        for (name, value) in [
            ("x-total-count", self.total),
            ("x-page-limit", self.limit),
            ("x-page-offset", self.offset),
        ] {
            response.headers_mut().insert(
                name,
                HeaderValue::from_str(&value.to_string()).map_err(|_| AppError::internal())?,
            );
        }
        Ok(())
    }
}
