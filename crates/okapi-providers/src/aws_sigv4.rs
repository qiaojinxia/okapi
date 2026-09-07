//! AWS Signature Version 4（IMPLEMENTATION §11.35，Bedrock 上游用）。
//!
//! 只实现 Bedrock 需要的子集：单块载荷、路径段 RFC 3986 编码、签 host / x-amz-date 与调用方给的
//! 若干头。不做 S3 的双重编码与分块签名。纯函数，时间戳由调用方传入以便测试。

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 静态 AWS 凭证。渠道凭证串 `ACCESS_KEY_ID:SECRET[:SESSION_TOKEN]` 解析而来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl AwsCredentials {
    /// 形态判定：第一段是 16–128 位大写字母数字（AKIA… / ASIA…）且带 `:` 分隔的才当 SigV4 凭证；
    /// 其它形态（Bedrock API key 是 base64 样式，不含 `:`）交给调用方按 Bearer 处理。
    #[must_use]
    pub fn parse(credential: &str) -> Option<Self> {
        let mut parts = credential.trim().splitn(3, ':');
        let access_key_id = parts.next()?;
        let secret_access_key = parts.next()?;
        let looks_like_key_id = (16..=128).contains(&access_key_id.len())
            && access_key_id
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !looks_like_key_id || secret_access_key.is_empty() {
            return None;
        }
        Some(Self {
            access_key_id: access_key_id.to_owned(),
            secret_access_key: secret_access_key.to_owned(),
            session_token: parts.next().filter(|t| !t.is_empty()).map(str::to_owned),
        })
    }
}

/// 一次签名的输入。`headers` 是除 host / x-amz-date（自动加入）外要参与签名的头，
/// 调用方必须把它们原样设到真实请求上。
pub struct SignParams<'a> {
    pub method: &'a str,
    pub url: &'a reqwest::Url,
    pub region: &'a str,
    pub service: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    /// 载荷 SHA-256 十六进制（空载荷也要算）。
    pub payload_hash: &'a str,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// 计算签名，返回要设到请求上的头：`x-amz-date`、`authorization`，有会话令牌时再加
/// `x-amz-security-token`（同样参与签名）。
#[must_use]
pub fn sign(creds: &AwsCredentials, p: &SignParams<'_>) -> Vec<(String, String)> {
    let amz_date = p.timestamp.format("%Y%m%dT%H%M%SZ").to_string();
    let date = p.timestamp.format("%Y%m%d").to_string();

    let mut headers: Vec<(String, String)> = p
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), normalize_header_value(v)))
        .collect();
    headers.push(("host".to_owned(), host_header(p.url)));
    headers.push(("x-amz-date".to_owned(), amz_date.clone()));
    if let Some(token) = &creds.session_token {
        headers.push(("x-amz-security-token".to_owned(), token.clone()));
    }
    headers.sort();
    headers.dedup_by(|a, b| a.0 == b.0);

    let canonical_headers = headers.iter().fold(String::new(), |mut acc, (k, v)| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "{k}:{v}");
        acc
    });
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        p.method.to_ascii_uppercase(),
        canonical_uri(p.url),
        canonical_query(p.url),
        canonical_headers,
        signed_headers,
        p.payload_hash
    );
    let scope = format!("{date}/{}/{}/aws4_request", p.region, p.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let signing_key = derive_signing_key(&creds.secret_access_key, &date, p.region, p.service);
    let signature = hex::encode(hmac(&signing_key, string_to_sign.as_bytes()));

    let mut out = vec![
        ("x-amz-date".to_owned(), amz_date),
        (
            "authorization".to_owned(),
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                creds.access_key_id
            ),
        ),
    ];
    if let Some(token) = &creds.session_token {
        out.push(("x-amz-security-token".to_owned(), token.clone()));
    }
    out
}

/// 载荷哈希（十六进制小写）。
#[must_use]
pub fn payload_hash(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

/// `kSigning = HMAC(HMAC(HMAC(HMAC("AWS4"+secret, date), region), service), "aws4_request")`。
#[must_use]
pub fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长度密钥，构造不会失败");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// RFC 3986 非保留字符不编，其余按字节 `%XX`（大写）。
#[must_use]
pub fn uri_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// 规范 URI：URL 里的路径已由调用方按段编码（`uri_encode`），此处原样使用；空路径为 `/`。
fn canonical_uri(url: &reqwest::Url) -> String {
    let path = url.path();
    if path.is_empty() {
        "/".to_owned()
    } else {
        path.to_owned()
    }
}

/// 规范查询串：键值各自编码后按键（再按值）排序，`&` 连接；无查询串为空行。
fn canonical_query(url: &reqwest::Url) -> String {
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (uri_encode(&k), uri_encode(&v)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// host 头与 reqwest 实际发出的一致：非默认端口带端口。
fn host_header(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// 头值：去首尾空白，连续空白压成一个空格（SigV4 规范要求）。
fn normalize_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    #[test]
    fn derived_signing_key_matches_aws_documentation_vector() {
        let key = derive_signing_key(SECRET, "20150830", "us-east-1", "iam");
        assert_eq!(
            hex::encode(key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    /// AWS SigV4 测试套件 `get-vanilla`：GET / 只签 host 与 x-amz-date。
    #[test]
    fn get_vanilla_signature_matches_test_suite() {
        let creds = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".to_owned(),
            secret_access_key: SECRET.to_owned(),
            session_token: None,
        };
        let url = reqwest::Url::parse("https://example.amazonaws.com/").unwrap();
        let ts = chrono::DateTime::parse_from_rfc3339("2015-08-30T12:36:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let headers = sign(
            &creds,
            &SignParams {
                method: "GET",
                url: &url,
                region: "us-east-1",
                service: "service",
                headers: &[],
                payload_hash: &payload_hash(b""),
                timestamp: ts,
            },
        );
        let auth = headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "x-amz-date" && v == "20150830T123600Z")
        );
    }

    #[test]
    fn session_token_is_signed_and_emitted() {
        let creds = AwsCredentials::parse("AKIDEXAMPLEAKIDEXAMP:secret:tok").unwrap();
        assert_eq!(creds.session_token.as_deref(), Some("tok"));
        let url = reqwest::Url::parse(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/x%3A0/invoke",
        )
        .unwrap();
        let headers = sign(
            &creds,
            &SignParams {
                method: "POST",
                url: &url,
                region: "us-east-1",
                service: "bedrock",
                headers: &[("content-type", "application/json")],
                payload_hash: &payload_hash(b"{}"),
                timestamp: chrono::Utc::now(),
            },
        );
        let auth = headers.iter().find(|(k, _)| k == "authorization").unwrap();
        assert!(
            auth.1
                .contains("SignedHeaders=content-type;host;x-amz-date;x-amz-security-token")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "x-amz-security-token" && v == "tok")
        );
    }

    #[test]
    fn credential_shapes() {
        assert!(AwsCredentials::parse("AKIAIOSFODNN7EXAMPLE:wJalr/K7MDENG").is_some());
        // Bedrock API key：无冒号 → 不是 SigV4 凭证
        assert!(AwsCredentials::parse("ABSKQmVkcm9ja0FQSUtleS1hYmM").is_none());
        // 小写段不是 access key id
        assert!(AwsCredentials::parse("akiaiosfodnn7example:secret").is_none());
        assert!(AwsCredentials::parse("AKIAIOSFODNN7EXAMPLE:").is_none());
    }

    #[test]
    fn uri_encoding_and_host_port() {
        assert_eq!(
            uri_encode("us.anthropic.claude-v1:0"),
            "us.anthropic.claude-v1%3A0"
        );
        assert_eq!(uri_encode("a b/c~"), "a%20b%2Fc~");
        let url = reqwest::Url::parse("http://127.0.0.1:8123/x").unwrap();
        assert_eq!(host_header(&url), "127.0.0.1:8123");
        let url = reqwest::Url::parse("https://h.example.com/x?b=2&a=1").unwrap();
        assert_eq!(host_header(&url), "h.example.com");
        assert_eq!(canonical_query(&url), "a=1&b=2");
    }
}
