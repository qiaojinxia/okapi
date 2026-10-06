//! The native Claude Code signer (unchanged from 2.1.286 through 2.1.290), verified against
//! an isolated official-client capture.
//! XXH64 hashes the final UTF-8 wire buffer with its own cch=00000 placeholder.
//! Model values and fallback/output-cap members are omitted as native byte ranges.
//! This is a versioned wire checksum, not an authentication or security signature.
use crate::UpstreamError;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::ops::Range;
use xxhash_rust::xxh64::Xxh64;

const SEED: u64 = 0x4d65_9218_e32a_3268;
const PLACEHOLDER: &[u8] = b"cch=00000;";
const MODEL: &[u8] = b"\"model\":\"";
const FALLBACKS: &[u8] = b"\"fallbacks\":[";
const FALLBACK_CREDIT: &[u8] = b"\"fallback_credit_token\":\"";
const MAX_TOKENS: &[u8] = b"\"max_tokens\":";

fn find(bytes: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|s| s == needle)
        .map(|i| i + from)
}

fn member_range(bytes: &[u8], from: usize, mut start: usize, mut end: usize) -> Range<usize> {
    if bytes.get(end) == Some(&b',') {
        end += 1;
    } else if start > from && bytes.get(start - 1) == Some(&b',') {
        start -= 1;
    }
    start..end
}

fn array_end(bytes: &[u8], mut cursor: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut in_string = false;
    while let Some(&byte) = bytes.get(cursor) {
        match byte {
            b'\\' if in_string => cursor += 1,
            b'"' => in_string = !in_string,
            b'[' if !in_string => depth += 1,
            b']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(cursor + 1);
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn omitted_end(bytes: &[u8], start: usize) -> Option<usize> {
    let rest = &bytes[start..];
    if rest.starts_with(FALLBACKS) {
        array_end(bytes, start + FALLBACKS.len())
    } else if rest.starts_with(FALLBACK_CREDIT) {
        find(bytes, b"\"", start + FALLBACK_CREDIT.len()).map(|end| end + 1)
    } else if rest.starts_with(MAX_TOKENS) {
        let value = start + MAX_TOKENS.len();
        let mut end = value;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        (end > value).then_some(end)
    } else {
        None
    }
}

fn checksum(wire: &[u8]) -> u64 {
    let mut hash = Xxh64::new(SEED);
    let mut cursor = 0;
    let mut pending = 0;
    // Native matches literal keys, including nested objects. Scan once rather
    // than repeatedly searching the whole remaining conversation for each key.
    // Omitted bytes still remain in the transmitted body and caller content.
    while cursor < wire.len() {
        if wire[cursor] == b'"' {
            let range = if wire[cursor..].starts_with(MODEL) {
                let value = cursor + MODEL.len();
                find(wire, b"\"", value).map(|end| value..end)
            } else {
                omitted_end(wire, cursor).map(|end| member_range(wire, pending, cursor, end))
            };
            if let Some(range) = range {
                hash.update(&wire[pending..range.start]);
                cursor = range.end;
                pending = cursor;
                continue;
            }
        }
        cursor += 1;
    }
    hash.update(&wire[pending..]);
    hash.digest() & 0x000f_ffff
}

pub(super) fn sign(wire: &mut [u8]) -> Result<(), UpstreamError> {
    #[derive(Deserialize)]
    struct Envelope<'a> {
        #[serde(borrow)]
        system: Vec<&'a RawValue>,
    }
    // Locate only our first top-level system block. A cch literal in user content,
    // nested tool arguments, or later system blocks must never be patched.
    let envelope: Envelope<'_> =
        serde_json::from_slice(wire).map_err(|e| UpstreamError::Build(e.to_string()))?;
    let first = envelope
        .system
        .first()
        .ok_or_else(|| UpstreamError::Build("cch_attribution_missing".into()))?
        .get();
    let local = find(first.as_bytes(), PLACEHOLDER, 0)
        .ok_or_else(|| UpstreamError::Build("cch_placeholder_missing".into()))?;
    let offset = first.as_ptr().addr() - wire.as_ptr().addr() + local + 4;
    let value = format!("{:05x}", checksum(wire));
    wire[offset..offset + 5].copy_from_slice(value.as_bytes());
    Ok(())
}

#[cfg(test)]
#[path = "cch_tests.rs"]
mod tests;
