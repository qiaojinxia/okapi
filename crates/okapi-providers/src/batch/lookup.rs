use super::BatchError;
use reqwest::Url;
use serde_json::Value;

pub const PAGE_SIZE: usize = 100;
/// Private account-scoped lookup evidence; callers must finish pagination before adoption.
pub struct LookupPage {
    pub names: Vec<String>,
    pub next_page: Option<String>,
    /// An exact submit label points at an incompatible model/input/account.
    pub conflict: bool,
}
pub(super) fn query(url: &mut Url, cursor: Option<&str>) -> Result<(), BatchError> {
    let mut pairs = url.query_pairs_mut();
    pairs.append_pair("pageSize", &PAGE_SIZE.to_string());
    if let Some(cursor) = cursor {
        if cursor.is_empty() || cursor.len() > 4096 || cursor.chars().any(char::is_control) {
            return Err(BatchError::invalid("batch_lookup_cursor"));
        }
        pairs.append_pair("pageToken", cursor);
    }
    Ok(())
}
pub(super) fn page<'a>(
    value: &'a Value,
    field: &str,
    cursor: Option<&str>,
) -> Result<(&'a [Value], LookupPage), BatchError> {
    let invalid = || BatchError::invalid("batch_lookup_response");
    if !value.is_object() {
        return Err(invalid());
    }
    if let Some(unreachable) = value.get("unreachable")
        && !unreachable.as_array().is_some_and(Vec::is_empty)
    {
        return Err(BatchError::invalid("batch_lookup_incomplete"));
    }
    let rows = match value.get(field) {
        None => &[][..],
        Some(v) => v
            .as_array()
            .filter(|a| a.len() <= PAGE_SIZE && a.iter().all(Value::is_object))
            .ok_or_else(invalid)?
            .as_slice(),
    };
    let next_page = match value.get("nextPageToken") {
        None => None,
        Some(v) => {
            let token = v
                .as_str()
                .filter(|s| s.len() <= 4096 && !s.chars().any(char::is_control))
                .ok_or_else(invalid)?;
            if token.is_empty() {
                None
            } else if Some(token) == cursor {
                return Err(BatchError::invalid("batch_lookup_cursor_cycle"));
            } else {
                Some(token.to_owned())
            }
        }
    };
    Ok((
        rows,
        LookupPage {
            names: Vec::new(),
            next_page,
            conflict: false,
        },
    ))
}
/// Every supplied alias must agree; required fields must appear at least once.
pub(super) fn texts_match(
    value: &Value,
    paths: &[&str],
    required: bool,
    accept: impl Fn(&str) -> bool,
) -> bool {
    let mut present = false;
    for found in paths.iter().filter_map(|path| value.pointer(path)) {
        present = true;
        if !found.as_str().is_some_and(&accept) {
            return false;
        }
    }
    present || !required
}
pub(super) fn has_text(value: &Value, paths: &[&str], expected: &str) -> bool {
    paths
        .iter()
        .any(|p| value.pointer(p).and_then(Value::as_str) == Some(expected))
}
