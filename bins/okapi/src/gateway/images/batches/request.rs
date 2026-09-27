use super::{AppError, hash};
use base64::Engine as _;
use bytes::Bytes;
use okapi_providers::{batch::MAX_INPUT_BYTES, image_store::fetch::content_type};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    pub model: String,
    #[serde(default)]
    pub task_name: String,
    #[serde(default)]
    pub parent_batch_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub items: Vec<Item>,
    #[serde(default)]
    pub aspect_ratio: Option<String>,
    #[serde(default)]
    pub image_size: Option<String>,
    #[serde(default)]
    pub response_mime_type: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Item {
    pub custom_id: String,
    pub prompt: String,
    #[serde(default = "one")]
    pub output_count: u32,
    #[serde(default)]
    pub reference_images: Vec<Reference>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reference {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    mime_type: String,
    data: String,
}
const fn one() -> u32 {
    1
}
fn invalid(field: &'static str) -> AppError {
    AppError::bad_request().with_param(field)
}
impl Input {
    pub fn read(bytes: &[u8]) -> Result<(Self, u32, String), AppError> {
        let input: Self = serde_json::from_slice(bytes).map_err(|_| invalid("body"))?;
        if input.model.trim().is_empty()
            || input.model.len() > 128
            || input.task_name.len() > 256
            || input.task_name.chars().any(char::is_control)
            || input.items.is_empty()
            || input.items.len() > 200
            || input
                .provider
                .as_deref()
                .is_some_and(|p| !matches!(p, "gemini" | "vertex"))
        {
            return Err(invalid("batch_request"));
        }
        if input.aspect_ratio.as_deref().is_some_and(|v| {
            !matches!(
                v,
                "1:1"
                    | "1:4"
                    | "1:8"
                    | "2:3"
                    | "3:2"
                    | "3:4"
                    | "4:1"
                    | "4:3"
                    | "4:5"
                    | "5:4"
                    | "8:1"
                    | "9:16"
                    | "16:9"
                    | "21:9"
            )
        }) || input
            .image_size
            .as_deref()
            .is_some_and(|v| !matches!(v, "512" | "1K" | "2K" | "4K"))
        {
            return Err(invalid("image_config"));
        }
        // GenerateContent's imageConfig has no output MIME switch. Do not silently ignore one.
        if input
            .response_mime_type
            .as_deref()
            .is_some_and(|v| v != "image/png")
        {
            return Err(invalid("response_mime_type"));
        }
        if input.metadata.len() > 32
            || input
                .metadata
                .iter()
                .any(|(k, v)| k.len() > 128 || v.len() > 1024)
        {
            return Err(invalid("metadata"));
        }
        let mut ids = HashSet::new();
        let mut units = 0_u32;
        let mut references = 0_usize;
        for item in &input.items {
            if item.custom_id.is_empty()
                || item.custom_id.len() > 128
                || item.custom_id.chars().any(char::is_control)
                || !ids.insert(&item.custom_id)
                || item.prompt.trim().is_empty()
                || item.prompt.chars().count() > 8000
                || !(1..=4).contains(&item.output_count)
            {
                return Err(invalid("items"));
            }
            units = units
                .checked_add(item.output_count)
                .ok_or_else(|| invalid("output_count"))?;
            references = references
                .checked_add(item.reference_images.len())
                .ok_or_else(|| invalid("reference_images"))?;
            for reference in &item.reference_images {
                reference.validate()?;
            }
        }
        if units > 200 || references > 1000 {
            return Err(invalid("batch_capacity"));
        }
        let digest = hash(&serde_json::to_vec(&input).map_err(|_| invalid("body"))?);
        Ok((input, units, digest))
    }
    pub fn jsonl(&self, id: Uuid) -> Result<Bytes, AppError> {
        self.jsonl_bounded(id, MAX_INPUT_BYTES)
    }
    fn jsonl_bounded(&self, id: Uuid, limit: usize) -> Result<Bytes, AppError> {
        let mut bytes = Vec::new();
        let mut slot = 0_u32;
        for item in &self.items {
            let mut parts = vec![json!({"text":item.prompt})];
            for reference in &item.reference_images {
                parts.push(
                    json!({"inlineData":{"mimeType":reference.mime_type,"data":reference.data}}),
                );
            }
            let mut config = json!({"responseModalities":["TEXT","IMAGE"]});
            let mut image_config = serde_json::Map::new();
            if let Some(ratio) = &self.aspect_ratio {
                image_config.insert("aspectRatio".into(), json!(ratio));
            }
            if let Some(size) = &self.image_size {
                image_config.insert("imageSize".into(), json!(size));
            }
            if !image_config.is_empty() {
                config["imageConfig"] = Value::Object(image_config);
            }
            let body =
                json!({"contents":[{"role":"user","parts":parts}],"generationConfig":config});
            for _ in 0..item.output_count {
                #[derive(Serialize)]
                struct Row<'a> {
                    key: String,
                    request: &'a Value,
                }
                // Serialize one slot at a time. Expanding all references into a Vec first
                // could allocate several times the permitted 128 MiB before rejecting it.
                let mut writer = Bounded {
                    bytes: &mut bytes,
                    limit,
                };
                serde_json::to_writer(
                    &mut writer,
                    &Row {
                        key: slot_key(id, slot),
                        request: &body,
                    },
                )
                .map_err(|_| invalid("batch_input_size"))?;
                std::io::Write::write_all(&mut writer, b"\n")
                    .map_err(|_| invalid("batch_input_size"))?;
                slot = slot.checked_add(1).ok_or_else(|| invalid("output_count"))?;
            }
        }
        Ok(bytes.into())
    }
}
struct Bounded<'a> {
    bytes: &'a mut Vec<u8>,
    limit: usize,
}
impl std::io::Write for Bounded<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("batch_input_size"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Reference {
    fn validate(&self) -> Result<(), AppError> {
        if self.id.as_ref().is_some_and(|s| s.len() > 128)
            || self
                .kind
                .as_deref()
                .is_some_and(|s| !matches!(s, "base64" | "inline"))
            || self.data.len() > 14 * 1024 * 1024
            || !matches!(
                self.mime_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
        {
            return Err(invalid("reference_images"));
        }
        let data = base64::prelude::BASE64_STANDARD
            .decode(&self.data)
            .map_err(|_| invalid("reference_images"))?;
        if data.len() > 10 * 1024 * 1024 || content_type(&data) != Some(self.mime_type.as_str()) {
            return Err(invalid("reference_images"));
        }
        Ok(())
    }
}
pub(super) fn slot_key(id: Uuid, slot: u32) -> String {
    format!("{}:{slot}", id.simple())
}
pub(super) fn slot_from_key(id: Uuid, key: &str, count: i32) -> Result<u32, AppError> {
    let suffix = key
        .strip_prefix(&format!("{}:", id.simple()))
        .ok_or_else(|| invalid("batch_result_key"))?;
    let slot = suffix
        .parse::<u32>()
        .map_err(|_| invalid("batch_result_key"))?;
    if slot.to_string() != suffix || i64::from(slot) >= i64::from(count) {
        return Err(invalid("batch_result_key"));
    }
    Ok(slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expanded_inputs_are_bounded_and_preserve_each_slot_configuration() {
        let (input,_,_)=Input::read(&serde_json::to_vec(&json!({"model":"image","aspect_ratio":"16:9","image_size":"2K","items":[{"custom_id":"one","prompt":"x".repeat(1000),"output_count":4}]})).unwrap()).unwrap();
        assert_eq!(
            input
                .jsonl_bounded(Uuid::nil(), 1500)
                .unwrap_err()
                .param
                .as_deref(),
            Some("batch_input_size")
        );
        let bytes = input.jsonl(Uuid::nil()).unwrap();
        let rows: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(rows.len(), 4);
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(
                row["key"],
                slot_key(Uuid::nil(), u32::try_from(index).unwrap())
            );
            assert_eq!(
                row["request"]["generationConfig"]["imageConfig"],
                json!({"aspectRatio":"16:9","imageSize":"2K"})
            );
        }
    }
}
