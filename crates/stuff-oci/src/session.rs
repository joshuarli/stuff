#![forbid(unsafe_code)]

use crate::error::{Error, ErrorCode};
use sha2::{Digest as _, Sha256};

pub const DEFAULT_SESSION_TTL_SECS: u64 = 3600;
const VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken {
    pub repository: String,
    pub upload_id: String,
    pub expires_at: u64,
}

impl SessionToken {
    pub fn new(repository: impl Into<String>, upload_id: impl Into<String>, expires_at: u64) -> Self {
        Self {
            repository: repository.into(),
            upload_id: upload_id.into(),
            expires_at,
        }
    }

    pub fn encode(&self, secret: &[u8]) -> String {
        let repository = hex(self.repository.as_bytes());
        let upload = hex(self.upload_id.as_bytes());
        let payload = format!("{VERSION}.{repository}.{upload}.{}", self.expires_at);
        let mac = hex(&hmac_sha256(secret, payload.as_bytes()));
        format!("{payload}.{mac}")
    }

    pub fn verify(text: &str, secret: &[u8], now: u64, repository: &str) -> Result<Self, Error> {
        let parts: Vec<&str> = text.split('.').collect();
        if parts.len() != 5 || parts[0] != VERSION {
            return Err(denied("invalid upload session token"));
        }
        let repository_bytes =
            unhex(parts[1]).ok_or_else(|| denied("invalid upload session repository"))?;
        let repository_text = String::from_utf8(repository_bytes)
            .map_err(|_| denied("invalid upload session repository"))?;
        let upload_id = String::from_utf8(unhex(parts[2]).ok_or_else(|| denied("invalid upload session id"))?)
            .map_err(|_| denied("invalid upload session id"))?;
        let expires_at: u64 = parts[3]
            .parse()
            .map_err(|_| denied("invalid upload session expiry"))?;
        let provided = unhex(parts[4]).ok_or_else(|| denied("invalid upload session signature"))?;
        let payload = format!("{VERSION}.{}.{}.{expires_at}", parts[1], parts[2]);
        let expected = hmac_sha256(secret, payload.as_bytes());
        if provided.len() != expected.len() || !constant_time_eq(&provided, &expected) {
            return Err(denied("invalid upload session signature"));
        }
        if repository_text != repository {
            return Err(denied("upload session belongs to another repository"));
        }
        if now > expires_at {
            return Err(denied("upload session has expired"));
        }
        if upload_id.is_empty() {
            return Err(denied("upload session has no upload id"));
        }
        Ok(Self { repository: repository_text, upload_id, expires_at })
    }
}

fn denied(message: &str) -> Error {
    Error::new(ErrorCode::Denied, message)
}

pub fn parse_basic_auth(header: &str) -> Option<(String, String)> {
    use base64::Engine as _;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, password) = text.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}

pub fn hmac_sha256(secret: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 64];
    if secret.len() > 64 {
        let digest = Sha256::digest(secret);
        key[..32].copy_from_slice(&digest);
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut inner = Sha256::new();
    for byte in &key {
        inner.update([byte ^ 0x36]);
    }
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    for byte in &key {
        outer.update([byte ^ 0x5c]);
    }
    outer.update(inner);
    outer.finalize().into()
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.as_chunks::<2>().0 {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        out.push(high * 16 + low);
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_auth() {
        assert_eq!(
            parse_basic_auth("Basic c3R1ZmY6c2VjcmV0"),
            Some(("stuff".to_string(), "secret".to_string()))
        );
        assert_eq!(parse_basic_auth("Bearer token"), None);
        assert_eq!(parse_basic_auth("Basic !!"), None);
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hex(&hmac_sha256(b"key", b"The quick brown fox jumps over the lazy dog")),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn sessions_round_trip_and_expire() {
        let token = SessionToken::new("example/app", "upload-1", 1000);
        let text = token.encode(b"secret");
        assert_eq!(
            SessionToken::verify(&text, b"secret", 999, "example/app").unwrap(),
            token
        );
        assert_eq!(
            SessionToken::verify(&text, b"secret", 1001, "example/app").unwrap_err().code,
            ErrorCode::Denied
        );
        assert_eq!(
            SessionToken::verify(&text, b"secret", 999, "example/other").unwrap_err().code,
            ErrorCode::Denied
        );
        assert_eq!(
            SessionToken::verify(&text, b"other", 999, "example/app").unwrap_err().code,
            ErrorCode::Denied
        );
        let mut tampered = text.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'0' { b'1' } else { b'0' };
        assert!(SessionToken::verify(&String::from_utf8(tampered).unwrap(), b"secret", 999, "example/app").is_err());
        assert!(SessionToken::verify("v1.bad", b"secret", 999, "example/app").is_err());
    }
}
