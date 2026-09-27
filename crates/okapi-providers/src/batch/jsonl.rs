//! Bounded, incremental JSONL reads. The owner must correlate all keys before settling a batch.
use super::{BatchError, MAX_INPUT_BYTES, Request};
use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

/// Supports bounded multi-image jobs without requiring the entire file in memory.
pub const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub line_bytes: usize,
    pub total_bytes: usize,
    pub rows: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            line_bytes: 64 * 1024 * 1024,
            total_bytes: 1024 * 1024 * 1024,
            rows: 10_000,
        }
    }
}
impl Limits {
    pub(super) fn validate(self) -> Result<(), BatchError> {
        if self.line_bytes == 0
            || self.line_bytes > 64 * 1024 * 1024
            || self.total_bytes < self.line_bytes
            || self.total_bytes > MAX_RESULT_BYTES
            || self.rows == 0
            || self.rows > 10_000
        {
            return Err(BatchError::invalid("batch_result_limits"));
        }
        Ok(())
    }
}

pub fn encode(requests: &[Request]) -> Result<Bytes, BatchError> {
    validate(requests)?;
    let mut bytes = Vec::new();
    for request in requests {
        #[derive(Serialize)]
        struct Row<'a> {
            key: &'a str,
            request: &'a Value,
        }
        let available = MAX_INPUT_BYTES
            .checked_sub(bytes.len() + 1)
            .ok_or_else(|| BatchError::invalid("batch_input_size"))?;
        let row = super::transport::serialize(
            &Row {
                key: &request.key,
                request: &request.request,
            },
            available,
        )?;
        bytes.extend_from_slice(&row);
        bytes.push(b'\n');
    }
    Ok(bytes.into())
}
pub(super) fn validate(requests: &[Request]) -> Result<(), BatchError> {
    if requests.is_empty() || requests.len() > 10_000 {
        return Err(BatchError::invalid("batch_request_count"));
    }
    let mut keys = HashSet::new();
    for request in requests {
        if request.key.is_empty()
            || request.key.len() > 256
            || request.key.chars().any(char::is_control)
            || !keys.insert(&request.key)
            || !request.request.is_object()
            || request
                .request
                .get("contents")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        {
            return Err(BatchError::invalid("batch_request"));
        }
    }
    Ok(())
}

pub struct Reader {
    response: reqwest::Response,
    pending: Bytes,
    line: Vec<u8>,
    limits: Limits,
    total: usize,
    rows: usize,
    eof: bool,
    finished: bool,
}
impl Reader {
    /// Includes whitespace and every received body chunk, not just decoded JSON rows.
    #[must_use]
    pub const fn bytes_read(&self) -> usize {
        self.total
    }
    pub(super) fn new(response: reqwest::Response, limits: Limits) -> Result<Self, BatchError> {
        limits.validate()?;
        if response
            .content_length()
            .is_some_and(|n| n > limits.total_bytes as u64)
        {
            return Err(BatchError::invalid("batch_result_size"));
        }
        Ok(Self {
            response,
            pending: Bytes::new(),
            line: Vec::new(),
            limits,
            total: 0,
            rows: 0,
            eof: false,
            finished: false,
        })
    }
    /// A malformed/oversized row ends this reader permanently. Dropping it cancels the download.
    pub async fn next(&mut self) -> Result<Option<Value>, BatchError> {
        if self.finished {
            return Ok(None);
        }
        let result = self.read_next().await;
        if result.is_err() || matches!(result, Ok(None)) {
            self.finished = true;
        }
        result
    }
    async fn read_next(&mut self) -> Result<Option<Value>, BatchError> {
        loop {
            if self.pending.is_empty() && !self.eof {
                match self
                    .response
                    .chunk()
                    .await
                    .map_err(|_| BatchError::invalid("batch_result_transport"))?
                {
                    Some(chunk) => {
                        if chunk.len() > self.limits.total_bytes.saturating_sub(self.total) {
                            return Err(BatchError::invalid("batch_result_size"));
                        }
                        self.total += chunk.len();
                        self.pending = chunk;
                    }
                    None => self.eof = true,
                }
            }
            let newline = self.pending.iter().position(|b| *b == b'\n');
            let size = newline.unwrap_or(self.pending.len());
            if size > self.limits.line_bytes.saturating_sub(self.line.len()) {
                return Err(BatchError::invalid("batch_result_line_size"));
            }
            self.line.extend_from_slice(&self.pending.split_to(size));
            if newline.is_some() {
                let _ = self.pending.split_to(1);
            }
            if newline.is_some() || self.eof {
                if self.line.iter().all(u8::is_ascii_whitespace) {
                    self.line.clear();
                    if self.eof {
                        return Ok(None);
                    }
                    continue;
                }
                if self.rows == self.limits.rows {
                    return Err(BatchError::invalid("batch_result_rows"));
                }
                let value: Value = serde_json::from_slice(&self.line)
                    .map_err(|_| BatchError::invalid("batch_result_json"))?;
                if !value.is_object() {
                    return Err(BatchError::invalid("batch_result_json"));
                }
                self.rows += 1;
                self.line.clear();
                return Ok(Some(value));
            }
        }
    }
}
