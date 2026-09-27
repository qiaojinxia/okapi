use super::super::{LookupPage, lookup};
use super::{
    BatchError, Job, Method, Value, VertexBatch, display_name, gcs::GcsStore, segment, transport,
};

impl VertexBatch {
    pub async fn lookup_page(
        &self,
        display: &str,
        model: &str,
        files: &GcsStore,
        cursor: Option<&str>,
    ) -> Result<LookupPage, BatchError> {
        validate(display, model)?;
        let mut url = self.url(&format!("{}/batchPredictionJobs", self.parent), "");
        lookup::query(&mut url, cursor)?;
        // JSON string escaping prevents display names from injecting list expressions.
        let quoted = serde_json::to_string(display)
            .map_err(|_| BatchError::invalid("batch_display_name"))?;
        url.query_pairs_mut()
            .append_pair("filter", &format!("displayName={quoted}"));
        let value = transport::json(self.http.request(Method::GET, url), false).await?;
        let (rows, mut page) = lookup::page(&value, "batchPredictionJobs", cursor)?;
        for row in rows {
            if !lookup::has_text(row, &["/displayName"], display) {
                continue;
            }
            let name = row.get("name").and_then(Value::as_str);
            if !self.identity(row, display, model, files, false)
                || name.is_none_or(|n| self.job_name(n).is_err())
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
    pub async fn get_verified(
        &self,
        name: &str,
        display: &str,
        model: &str,
        files: &GcsStore,
    ) -> Result<Job, BatchError> {
        self.job_name(name)?;
        validate(display, model)?;
        let value =
            transport::json(self.http.request(Method::GET, self.url(name, "")), false).await?;
        if !self.identity(&value, display, model, files, true) {
            return Err(BatchError::invalid("batch_lookup_identity"));
        }
        self.parse(&value, Some(name), files)
    }
    fn identity(
        &self,
        value: &Value,
        display: &str,
        model: &str,
        files: &GcsStore,
        required: bool,
    ) -> bool {
        let model = model
            .strip_prefix("publishers/google/models/")
            .unwrap_or(model);
        let expected = format!("publishers/google/models/{model}");
        let regional = format!("{}/{expected}", self.parent);
        if !lookup::texts_match(value, &["/displayName"], true, |s| s == display)
            || !lookup::texts_match(value, &["/model"], required, |s| {
                s == expected || s == regional
            })
            || !lookup::texts_match(value, &["/inputConfig/instancesFormat"], required, |s| {
                s == "jsonl"
            })
            || !lookup::texts_match(value, &["/instanceConfig/keyField"], false, |s| s == "key")
        {
            return false;
        }
        match value.pointer("/inputConfig/gcsSource/uris") {
            None => !required,
            Some(v) => v
                .as_array()
                .is_some_and(|a| a.len() == 1 && a[0].as_str() == Some(files.input_uri().as_str())),
        }
    }
}
fn validate(display: &str, model: &str) -> Result<(), BatchError> {
    display_name(display)?;
    if !segment(
        model
            .strip_prefix("publishers/google/models/")
            .unwrap_or(model),
    ) {
        return Err(BatchError::invalid("batch_model"));
    }
    Ok(())
}
