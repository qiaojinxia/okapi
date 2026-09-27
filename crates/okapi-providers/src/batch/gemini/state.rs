use super::super::{BatchError, Job, JobState, Output, resource};
use serde_json::Value;

pub(super) fn parse(value: &Value, expected: Option<&str>) -> Result<Job, BatchError> {
    let invalid = || BatchError::invalid("batch_job_response");
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    resource(name, "batches")?;
    if expected.is_some_and(|v| v != name) {
        return Err(BatchError::invalid("batch_job_identity"));
    }
    let mut state = None;
    for raw in [value.get("state"), value.pointer("/metadata/state")]
        .into_iter()
        .flatten()
    {
        let next = JobState::parse(raw.as_str().ok_or_else(invalid)?)?;
        if state.is_some_and(|v| v != next) {
            return Err(invalid());
        }
        state = Some(next);
    }
    let done = value
        .get("done")
        .map(|v| v.as_bool().ok_or_else(invalid))
        .transpose()?;
    if done == Some(false) && (value.get("response").is_some() || value.get("error").is_some()) {
        return Err(invalid());
    }
    let error_code = value
        .get("error")
        .map(|v| {
            v.get("code")
                .and_then(Value::as_i64)
                .filter(|v| *v > 0)
                .ok_or_else(invalid)
        })
        .transpose()?;
    let output = output(value)?;
    // Operation.error and Operation.response are mutually exclusive. Metadata is
    // independent: a cancelled job can retain already-produced partial outputs.
    if error_code.is_some()
        && (value.get("response").is_some()
            || value.get("dest").is_some()
            || value.get("output").is_some())
    {
        return Err(invalid());
    }
    let inferred = if error_code == Some(1) {
        JobState::Cancelled
    } else if error_code.is_some() {
        JobState::Failed
    } else if done == Some(true) {
        JobState::Succeeded
    } else {
        JobState::Pending
    };
    let state = state.unwrap_or(inferred);
    if done.is_some_and(|v| v != state.terminal())
        || (!state.terminal() && (output.is_some() || error_code.is_some()))
        || (matches!(state, JobState::Succeeded | JobState::PartiallySucceeded)
            && (output.is_none() || error_code.is_some()))
        || (error_code == Some(1) && state != JobState::Cancelled)
    {
        return Err(invalid());
    }
    Ok(Job {
        name: name.to_owned(),
        state,
        output,
        error_code,
    })
}

fn output(value: &Value) -> Result<Option<Output>, BatchError> {
    let mut found = None;
    for node in [
        value.get("response"),
        value.get("dest"),
        value.get("output"),
        value.pointer("/metadata/output"),
    ]
    .into_iter()
    .flatten()
    {
        if !node.is_object() {
            return Err(BatchError::invalid("batch_output_shape"));
        }
        for key in ["responsesFile", "responses_file", "fileName", "file_name"] {
            if let Some(value) = node.get(key) {
                let name = value
                    .as_str()
                    .ok_or_else(|| BatchError::invalid("batch_output_file"))?;
                resource(name, "files")?;
                merge(&mut found, Output::File(name.to_owned()))?;
            }
        }
        for key in ["inlinedResponses", "inlined_responses"] {
            if let Some(value) = node.get(key) {
                let rows = value
                    .as_array()
                    .or_else(|| value.get(key).and_then(Value::as_array))
                    .filter(|rows| {
                        !rows.is_empty()
                            && rows.len() <= 10_000
                            && rows.iter().all(Value::is_object)
                    })
                    .ok_or_else(|| BatchError::invalid("batch_output_inline"))?;
                merge(&mut found, Output::Inline(rows.clone()))?;
            }
        }
    }
    Ok(found)
}
fn merge(found: &mut Option<Output>, next: Output) -> Result<(), BatchError> {
    if found.as_ref().is_some_and(|v| *v != next) {
        return Err(BatchError::invalid("batch_output_conflict"));
    }
    *found = Some(next);
    Ok(())
}
