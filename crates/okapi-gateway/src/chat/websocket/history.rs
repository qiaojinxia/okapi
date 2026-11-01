//! Connection-local replay state. Whole opaque items are retained, never summarized or pruned.
use super::{AppError, Bytes, Lane, MAX_BUFFER, MAX_MESSAGE, Semaphore, StatusCode, Value, codes};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

pub(super) const LOCAL_PREFIX: &str = "resp_okapi_warmup_";
const MAX_ITEMS: usize = 4096;

pub(super) struct Context {
    id: String,
    input: Vec<Value>,
    root: Option<String>,
    _bytes: OwnedSemaphorePermit,
}

pub(super) struct History {
    latest: HashMap<Lane, Arc<Context>>,
    active: HashMap<String, Uuid>,
    pub budget: Arc<Semaphore>,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self {
            latest: HashMap::new(),
            active: HashMap::new(),
            budget: Arc::new(Semaphore::new(limit.min(MAX_BUFFER))),
        }
    }

    pub fn lookup(&self, body: &Bytes) -> Result<Option<Arc<Context>>, AppError> {
        let value: Value = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
        let Some(id) = value.get("previous_response_id").and_then(Value::as_str) else {
            return Ok(None);
        };
        if self.active.contains_key(id) {
            return Err(AppError::new(StatusCode::CONFLICT, codes::BAD_REQUEST)
                .with_param("previous_response_in_progress"));
        }
        let context = self
            .latest
            .values()
            .find(|context| context.id == id)
            .cloned();
        if context.is_none() && id.starts_with(LOCAL_PREFIX) {
            return Err(AppError::new(StatusCode::NOT_FOUND, codes::NOT_FOUND)
                .with_param("previous_response_id"));
        }
        Ok(context)
    }

    pub fn finish(&mut self, request: Uuid, lane: &Lane, previous: Option<&str>, failed: bool) {
        self.active.retain(|_, owner| *owner != request);
        if failed
            && self
                .latest
                .get(lane)
                .is_some_and(|c| Some(c.id.as_str()) == previous)
        {
            self.latest.remove(lane);
        }
    }
}

fn limit() -> AppError {
    AppError::new(StatusCode::PAYLOAD_TOO_LARGE, codes::BAD_REQUEST)
        .with_param("responses_ws_context_limit")
}

fn invalid() -> AppError {
    AppError::new(StatusCode::BAD_GATEWAY, codes::UPSTREAM_ERROR)
        .with_param("responses_http_history")
}

fn input(value: Option<&Value>) -> Result<Vec<Value>, AppError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![serde_json::json!({"role":"user","content":text})]),
        Some(Value::Array(items)) if items.len() <= MAX_ITEMS => Ok(items.clone()),
        _ => Err(AppError::bad_request().with_param("input")),
    }
}

/// Expand only a known complete snapshot. An external stored parent remains a parent.
pub(super) fn expand(
    body: &Bytes,
    context: Option<&Context>,
) -> Result<(Value, Vec<Value>, Option<String>), AppError> {
    let mut value: Value = serde_json::from_slice(body).map_err(|_| AppError::bad_request())?;
    let object = value.as_object_mut().ok_or_else(AppError::bad_request)?;
    let additions = input(object.get("input"))?;
    let mut items = context.map_or_else(Vec::new, |c| c.input.clone());
    if items.len().saturating_add(additions.len()) > MAX_ITEMS {
        return Err(limit());
    }
    items.extend(additions);
    let root = if let Some(context) = context {
        match &context.root {
            Some(id) => {
                object.insert("previous_response_id".into(), id.clone().into());
            }
            None => {
                object.remove("previous_response_id");
            }
        }
        context.root.clone()
    } else {
        object
            .get("previous_response_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    object.insert("input".into(), Value::Array(items.clone()));
    Ok((value, items, root))
}

pub(super) fn estimate(body: &Bytes, context: Option<&Context>) -> Result<Bytes, AppError> {
    if context.is_none() {
        return Ok(body.clone());
    }
    let (value, _, _) = expand(body, context)?;
    let bytes = value.to_string();
    if bytes.len() > MAX_MESSAGE {
        return Err(limit());
    }
    Ok(Bytes::from(bytes))
}

pub(super) struct Capture {
    input: Vec<Value>,
    root: Option<String>,
    id: Option<String>,
    items: BTreeMap<u64, Value>,
    bytes: Option<OwnedSemaphorePermit>,
    budget: Arc<Semaphore>,
    // Account for rendered replay HTTP requests independently from cached snapshots.
    _request: OwnedSemaphorePermit,
}

impl Capture {
    pub fn new(
        input: Vec<Value>,
        root: Option<String>,
        budget: Arc<Semaphore>,
        request: OwnedSemaphorePermit,
    ) -> Result<Self, AppError> {
        let mut capture = Self {
            input,
            root,
            id: None,
            items: BTreeMap::new(),
            bytes: None,
            budget,
            _request: request,
        };
        capture.reserve(&[])?;
        Ok(capture)
    }

    fn reserve(&mut self, output: &[Value]) -> Result<(), AppError> {
        let size = serde_json::to_vec(&(&self.input, output, &self.root))
            .map_err(|_| invalid())?
            .len();
        if size > MAX_MESSAGE || self.input.len().saturating_add(output.len()) > MAX_ITEMS {
            return Err(limit());
        }
        let held = self
            .bytes
            .as_ref()
            .map_or(0, OwnedSemaphorePermit::num_permits);
        if size > held {
            let more = self
                .budget
                .clone()
                .try_acquire_many_owned(u32::try_from(size - held).map_err(|_| limit())?)
                .map_err(|_| limit())?;
            if let Some(bytes) = &mut self.bytes {
                bytes.merge(more);
            } else {
                self.bytes = Some(more);
            }
        }
        Ok(())
    }

    pub fn event(
        &mut self,
        history: &mut History,
        lane: &Lane,
        request: Uuid,
        raw: &str,
    ) -> Result<(), AppError> {
        let value: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if let Some(id) = value
            .pointer("/response/id")
            .or_else(|| value.get("response_id"))
        {
            let id = id
                .as_str()
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= 512
                        && !id.chars().any(|c| c.is_whitespace() || c.is_control())
                })
                .ok_or_else(invalid)?;
            if self.id.as_deref().is_some_and(|known| known != id)
                || history.latest.values().any(|context| context.id == id)
                || history
                    .active
                    .get(id)
                    .is_some_and(|owner| *owner != request)
            {
                return Err(invalid());
            }
            self.id = Some(id.to_owned());
            history.active.insert(id.to_owned(), request);
        }
        if kind == "response.output_item.done" {
            let index = value
                .get("output_index")
                .and_then(Value::as_u64)
                .filter(|v| *v < MAX_ITEMS as u64)
                .ok_or_else(invalid)?;
            let item = value
                .get("item")
                .filter(|item| item.is_object())
                .ok_or_else(invalid)?;
            if self.items.get(&index).is_some_and(|old| old != item) {
                return Err(invalid());
            }
            self.items.insert(index, item.clone());
            self.reserve(&self.items.values().cloned().collect::<Vec<_>>())?;
        }
        if matches!(kind, "response.completed" | "response.incomplete") {
            let id = self.id.clone().ok_or_else(invalid)?;
            let output = match value.pointer("/response/output") {
                Some(Value::Array(output)) if !output.is_empty() || self.items.is_empty() => {
                    output.clone()
                }
                None | Some(Value::Array(_)) if !self.items.is_empty() => {
                    self.items.values().cloned().collect()
                }
                _ => return Err(invalid()),
            };
            if output.iter().any(|item| !item.is_object())
                || self.items.iter().any(|(index, item)| {
                    usize::try_from(*index).ok().and_then(|i| output.get(i)) != Some(item)
                })
            {
                return Err(invalid());
            }
            self.reserve(&output)?;
            self.input.extend(output);
            let context = Context {
                id,
                input: std::mem::take(&mut self.input),
                root: self.root.take(),
                _bytes: self.bytes.take().ok_or_else(invalid)?,
            };
            history.latest.insert(lane.clone(), Arc::new(context));
            history.active.retain(|_, owner| *owner != request);
        }
        Ok(())
    }
}
