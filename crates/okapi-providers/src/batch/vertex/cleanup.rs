use super::{BatchError, Method, Value, VertexBatch, segment, transport};

pub struct DeleteOperation {
    pub name: String,
    pub done: bool,
    pub failed: bool,
}
impl VertexBatch {
    fn operation_name(&self, job: &str, name: &str) -> Result<(), BatchError> {
        self.job_name(job)?;
        if name.len() > 1024
            || ![
                format!("{}/operations/", self.parent),
                format!("{job}/operations/"),
            ]
            .iter()
            .any(|p| name.strip_prefix(p).is_some_and(segment))
        {
            return Err(BatchError::invalid("batch_delete_operation_identity"));
        }
        Ok(())
    }
    fn parse_delete(
        &self,
        job: &str,
        expected: Option<&str>,
        value: &Value,
    ) -> Result<DeleteOperation, BatchError> {
        let invalid = || BatchError::invalid("batch_delete_operation");
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        self.operation_name(job, name)?;
        if expected.is_some_and(|n| n != name) {
            return Err(invalid());
        }
        let done = value
            .get("done")
            .map(|v| v.as_bool().ok_or_else(invalid))
            .transpose()?
            .unwrap_or(false);
        let failed = value
            .get("error")
            .map(|v| {
                v.get("code")
                    .and_then(Value::as_i64)
                    .filter(|v| *v > 0)
                    .ok_or_else(invalid)
            })
            .transpose()?
            .is_some();
        if (failed && (!done || value.get("response").is_some()))
            || (!done && value.get("response").is_some())
        {
            return Err(invalid());
        }
        Ok(DeleteOperation {
            name: name.to_owned(),
            done,
            failed,
        })
    }
    /// None means this exact resource returned 404. A successful DELETE otherwise
    /// returns an asynchronous operation, never proof that artifacts can be removed.
    pub async fn delete_job(&self, name: &str) -> Result<Option<DeleteOperation>, BatchError> {
        self.job_name(name)?;
        let value = match transport::json(
            self.http.request(Method::DELETE, self.url(name, "")),
            true,
        )
        .await
        {
            Ok(v) => v,
            Err(e) if e.status == Some(404) => return Ok(None),
            Err(e) => return Err(e),
        };
        self.parse_delete(name, None, &value)
            .map(Some)
            .map_err(|e| e.uncertain(true))
    }
    pub async fn deletion(
        &self,
        job: &str,
        operation: &str,
    ) -> Result<Option<DeleteOperation>, BatchError> {
        self.operation_name(job, operation)?;
        let value = match transport::json(
            self.http.request(Method::GET, self.url(operation, "")),
            false,
        )
        .await
        {
            Ok(v) => v,
            Err(e) if e.status == Some(404) => return Ok(None),
            Err(e) => return Err(e),
        };
        self.parse_delete(job, Some(operation), &value).map(Some)
    }
}
