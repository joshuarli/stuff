#![forbid(unsafe_code)]

use crate::error::Error;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(String);

impl Digest {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let Some(hex) = text.strip_prefix("sha256:") else {
            return Err(Error::digest_invalid(format!("unsupported digest algorithm: {text}")));
        };
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(Error::digest_invalid(format!("not a sha256 hex digest: {text}")));
        }
        Ok(Self(text.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        Self(format!("sha256:{:x}", hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn hex(&self) -> &str {
        &self.0["sha256:".len()..]
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Digest {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn parses_only_lowercase_sha256_and_hashes_bytes() {
        let text = "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
        let digest = Digest::parse(text).unwrap();
        assert_eq!(digest.as_str(), text);
        assert_eq!(digest.hex().len(), 64);
        assert_eq!(Digest::parse("sha512:00").unwrap_err().code, ErrorCode::DigestInvalid);
        assert!(Digest::parse("sha256:00").is_err());
        assert!(Digest::parse("sha256:576055FC159808EBB0B91A55303A512F5730A93E036977F652E59ADC769DEBF4").is_err());
        assert_eq!(
            Digest::from_bytes(b"{}").as_str(),
            "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }
}
