#![forbid(unsafe_code)]

use crate::digest::Digest;
use crate::error::Error;
use std::fmt;

pub const MAX_REPOSITORY_NAME: usize = 255;
pub const MAX_TAG: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepositoryName(String);

impl RepositoryName {
    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.is_empty() || text.len() > MAX_REPOSITORY_NAME {
            return Err(Error::name_invalid("repository name is empty or too long"));
        }
        if !text.split('/').all(valid_component) {
            return Err(Error::name_invalid(text));
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    if bytes.is_empty() || !lower_alnum(bytes[0]) {
        return false;
    }
    let mut i = 1;
    while i < bytes.len() {
        if lower_alnum(bytes[i]) {
            i += 1;
            continue;
        }
        match bytes[i] {
            b'.' => i += 1,
            b'_' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b'_' {
                    i += 1;
                }
            }
            b'-' => {
                while i < bytes.len() && bytes[i] == b'-' {
                    i += 1;
                }
            }
            _ => return false,
        }
        if i >= bytes.len() || !lower_alnum(bytes[i]) {
            return false;
        }
    }
    true
}

fn lower_alnum(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(String);

impl Tag {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let bytes = text.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_TAG {
            return Err(Error::tag_invalid("tag is empty or too long"));
        }
        if !(bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_') {
            return Err(Error::tag_invalid(text));
        }
        if !bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(Error::tag_invalid(text));
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Tag(Tag),
    Digest(Digest),
}

impl Reference {
    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.contains(':') {
            Ok(Self::Digest(Digest::parse(text)?))
        } else {
            Ok(Self::Tag(Tag::parse(text)?))
        }
    }

    pub fn is_digest(&self) -> bool {
        matches!(self, Self::Digest(_))
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Tag(tag) => tag.as_str(),
            Self::Digest(digest) => digest.as_str(),
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn accepts_distribution_repository_names() {
        for name in ["a", "example/app", "a/b/c", "foo-bar", "foo_bar", "foo.bar", "a--b", "a__b"] {
            assert!(RepositoryName::parse(name).is_ok(), "{name}");
        }
        for name in ["", "Example/app", "a//b", "/a", "a/", "a../b", "a..b", "a___b", "a.-b", "a/"] {
            assert!(RepositoryName::parse(name).is_err(), "{name}");
        }
        assert!(RepositoryName::parse(&"a".repeat(MAX_REPOSITORY_NAME)).is_ok());
        assert!(RepositoryName::parse(&"a".repeat(MAX_REPOSITORY_NAME + 1)).is_err());
    }

    #[test]
    fn accepts_distribution_tags() {
        for tag in ["latest", "0.13.2", "_x", "A-B_c.1"] {
            assert!(Tag::parse(tag).is_ok(), "{tag}");
        }
        for tag in ["", ".x", "-x", "a b", "a/b", "é"] {
            assert!(Tag::parse(tag).is_err(), "{tag}");
        }
        assert!(Tag::parse(&"a".repeat(MAX_TAG)).is_ok());
        assert!(Tag::parse(&"a".repeat(MAX_TAG + 1)).is_err());
    }

    #[test]
    fn references_select_tag_or_digest() {
        assert_eq!(Reference::parse("stable").unwrap().as_str(), "stable");
        let digest = "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
        assert!(Reference::parse(digest).unwrap().is_digest());
        assert_eq!(Reference::parse("sha256:00").unwrap_err().code, ErrorCode::DigestInvalid);
        assert_eq!(Reference::parse("-bad").unwrap_err().code, ErrorCode::TagInvalid);
    }
}
