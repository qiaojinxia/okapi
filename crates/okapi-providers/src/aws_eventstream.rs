//! AWS event-stream 二进制帧解码（`application/vnd.amazon.eventstream`，Bedrock
//! InvokeModelWithResponseStream 的响应体格式，IMPLEMENTATION §11.35）。
//!
//! 帧 = 12 字节 prelude（总长 / 头长 / prelude CRC，均 u32 BE），接着是头（名长 u8 + 名 +
//! 类型 u8 + 值）、载荷、4 字节消息 CRC。头只关心字符串类型（`:message-type` / `:event-type` /
//! `:exception-type` / `:content-type`），其余类型按长度跳过。CRC 不校验：帧在 TLS 之上，
//! 完整性由传输层保证，这里只做切分。

use crate::error::UpstreamError;

const PRELUDE_LEN: usize = 12;
const TRAILER_LEN: usize = 4;
/// 单帧上限：Bedrock 事件载荷是几 KB 级的 JSON，16MB 已远超；防坏字节把缓冲撑爆。
const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// 一帧：字符串头 + 载荷原文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub headers: Vec<(String, String)>,
    pub payload: Vec<u8>,
}

impl Frame {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// 增量解码器：字节流按任意边界切分喂入，返回本次凑齐的完整帧。
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Frame>, UpstreamError> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            if self.buf.len() < PRELUDE_LEN {
                break;
            }
            let total = read_u32(&self.buf, 0) as usize;
            let headers_len = read_u32(&self.buf, 4) as usize;
            if !(PRELUDE_LEN + TRAILER_LEN..=MAX_FRAME_LEN).contains(&total)
                || headers_len > total - PRELUDE_LEN - TRAILER_LEN
            {
                return Err(UpstreamError::Stream("eventstream_bad_prelude".to_owned()));
            }
            if self.buf.len() < total {
                break;
            }
            let frame_bytes: Vec<u8> = self.buf.drain(..total).collect();
            let headers = parse_headers(&frame_bytes[PRELUDE_LEN..PRELUDE_LEN + headers_len])?;
            let payload = frame_bytes[PRELUDE_LEN + headers_len..total - TRAILER_LEN].to_vec();
            frames.push(Frame { headers, payload });
        }
        Ok(frames)
    }
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn parse_headers(mut bytes: &[u8]) -> Result<Vec<(String, String)>, UpstreamError> {
    let bad = || UpstreamError::Stream("eventstream_bad_header".to_owned());
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let name_len = usize::from(bytes[0]);
        let name = bytes.get(1..1 + name_len).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(name).into_owned();
        let kind = *bytes.get(1 + name_len).ok_or_else(bad)?;
        let rest = &bytes[2 + name_len..];
        let (value, consumed) = match kind {
            // bool true / bool false：无值字节
            0 | 1 => (None, 0),
            2 => (None, 1),
            3 => (None, 2),
            4 => (None, 4),
            5 | 8 => (None, 8),
            // byte array（6）与 string（7）：u16 长度前缀
            6 | 7 => {
                let len = usize::from(u16::from_be_bytes([
                    *rest.first().ok_or_else(bad)?,
                    *rest.get(1).ok_or_else(bad)?,
                ]));
                let value = rest.get(2..2 + len).ok_or_else(bad)?;
                (
                    (kind == 7).then(|| String::from_utf8_lossy(value).into_owned()),
                    2 + len,
                )
            }
            9 => (None, 16),
            _ => return Err(bad()),
        };
        if rest.len() < consumed {
            return Err(bad());
        }
        if let Some(v) = value {
            out.push((name, v));
        }
        bytes = &rest[consumed..];
    }
    Ok(out)
}

/// 组帧（测试与 mock 上游用）：CRC 位填零，解码器不校验。
#[must_use]
pub fn encode_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut hdr = Vec::new();
    for (name, value) in headers {
        hdr.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
        hdr.extend_from_slice(name.as_bytes());
        hdr.push(7);
        hdr.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_be_bytes());
        hdr.extend_from_slice(value.as_bytes());
    }
    let total = PRELUDE_LEN + hdr.len() + payload.len() + TRAILER_LEN;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&u32::try_from(total).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&u32::try_from(hdr.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&hdr);
    out.extend_from_slice(payload);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_frames_split_at_arbitrary_boundaries() {
        let a = encode_frame(
            &[(":message-type", "event"), (":event-type", "chunk")],
            br#"{"bytes":"e30="}"#,
        );
        let b = encode_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "throttlingException"),
            ],
            br#"{"message":"slow down"}"#,
        );
        let all: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
        let mut dec = Decoder::new();
        let mut frames = Vec::new();
        // 每 5 字节一喂，帧边界必然被切开
        for piece in all.chunks(5) {
            frames.extend(dec.push(piece).unwrap());
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].header(":event-type"), Some("chunk"));
        assert_eq!(frames[0].payload, br#"{"bytes":"e30="}"#.to_vec());
        assert_eq!(frames[1].header(":message-type"), Some("exception"));
        assert_eq!(
            frames[1].header(":exception-type"),
            Some("throttlingException")
        );
    }

    #[test]
    fn rejects_garbage_prelude() {
        let mut dec = Decoder::new();
        assert!(dec.push(&[0xFF; 12]).is_err());
    }

    #[test]
    fn skips_non_string_header_types() {
        // 手工拼一帧：一个 timestamp 头（类型 8，8 字节）+ 一个字符串头
        let mut hdr = Vec::new();
        hdr.push(2);
        hdr.extend_from_slice(b":t");
        hdr.push(8);
        hdr.extend_from_slice(&[0; 8]);
        hdr.push(11);
        hdr.extend_from_slice(b":event-type");
        hdr.push(7);
        hdr.extend_from_slice(&5u16.to_be_bytes());
        hdr.extend_from_slice(b"chunk");
        let total = PRELUDE_LEN + hdr.len() + TRAILER_LEN;
        let mut frame = Vec::new();
        frame.extend_from_slice(&u32::try_from(total).unwrap().to_be_bytes());
        frame.extend_from_slice(&u32::try_from(hdr.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(&[0; 4]);
        frame.extend_from_slice(&hdr);
        frame.extend_from_slice(&[0; 4]);
        let frames = Decoder::new().push(&frame).unwrap();
        assert_eq!(
            frames[0].headers,
            vec![(":event-type".to_owned(), "chunk".to_owned())]
        );
        assert!(frames[0].payload.is_empty());
    }
}
