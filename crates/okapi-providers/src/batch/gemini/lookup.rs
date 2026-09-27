use super::super::{LookupPage, display_name, lookup};
use super::{BatchError, GeminiBatch, Job, Method, Value, resource, segment, transport};

const DISPLAY: &[&str] = &["/metadata/displayName", "/displayName"];
const MODEL: &[&str] = &["/metadata/model", "/model"];
const INPUT: &[&str] = &["/metadata/inputConfig/fileName", "/inputConfig/fileName"];

impl GeminiBatch {
    /// Developer API does not support the SDK's displayName filter. Scan all pages
    /// within the frozen account, then verify the sole candidate with a fresh GET.
    pub async fn lookup_page(
        &self,
        display: &str,
        model: &str,
        input: &str,
        cursor: Option<&str>,
    ) -> Result<LookupPage, BatchError> {
        validate(display, model, input)?;
        let mut url = self.url("/v1beta/batches");
        lookup::query(&mut url, cursor)?;
        let value = transport::json(self.http.request(Method::GET, url), false).await?;
        let (rows, mut page) = lookup::page(&value, "operations", cursor)?;
        for row in rows {
            if !lookup::has_text(row, DISPLAY, display) {
                continue;
            }
            let name = row.get("name").and_then(Value::as_str);
            if !identity(row, display, model, input, false)
                || name.is_none_or(|n| resource(n, "batches").is_err())
            {
                page.conflict = true;
                continue;
            }
            if let Some(name) = name {
                page.names.push(name.to_owned());
            }
        }
        Ok(page)
    }
    /// No state/output can be adopted from an unverified list row or guessed name.
    pub async fn get_verified(
        &self,
        name: &str,
        display: &str,
        model: &str,
        input: &str,
    ) -> Result<Job, BatchError> {
        resource(name, "batches")?;
        validate(display, model, input)?;
        let value = transport::json(
            self.http
                .request(Method::GET, self.url(&format!("/v1beta/{name}"))),
            false,
        )
        .await?;
        if !identity(&value, display, model, input, true) {
            return Err(BatchError::invalid("batch_lookup_identity"));
        }
        super::state::parse(&value, Some(name))
    }
}
fn validate(display: &str, model: &str, input: &str) -> Result<(), BatchError> {
    display_name(display)?;
    resource(input, "files")?;
    if !segment(model.strip_prefix("models/").unwrap_or(model)) {
        return Err(BatchError::invalid("batch_model"));
    }
    Ok(())
}
fn identity(value: &Value, display: &str, model: &str, input: &str, required: bool) -> bool {
    let model = model.strip_prefix("models/").unwrap_or(model);
    lookup::texts_match(value, DISPLAY, true, |s| s == display)
        && lookup::texts_match(value, MODEL, required, |s| {
            s.strip_prefix("models/").unwrap_or(s) == model
        })
        && lookup::texts_match(value, INPUT, required, |s| s == input)
}
