use std::cmp::Ordering;
use std::fmt;

use super::{input, Rejection};

pub const MAX_LENGTH: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identifier {
    Numeric(u64),
    Alpha(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<Identifier>,
    pub build: Vec<String>,
}

impl Version {
    pub fn parse(text: &str) -> Result<Self, Rejection> {
        let invalid = || input(format!("invalid version '{text}'"));
        if text.is_empty() || text.chars().count() > MAX_LENGTH {
            return Err(invalid());
        }
        let (core_pre, build) = match text.split_once('+') {
            Some((core, build_text)) => {
                if build_text.is_empty() {
                    return Err(invalid());
                }
                let mut build = Vec::new();
                for part in build_text.split('.') {
                    if !identifier_ok(part) {
                        return Err(invalid());
                    }
                    build.push(part.to_string());
                }
                (core, build)
            }
            None => (text, Vec::new()),
        };
        let (core, pre_text) = match core_pre.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (core_pre, None),
        };
        let pieces: Vec<&str> = core.split('.').collect();
        if pieces.len() != 3 {
            return Err(invalid());
        }
        let mut numbers = [0_u64; 3];
        for (slot, piece) in numbers.iter_mut().zip(pieces.iter()) {
            *slot = numeric(piece).ok_or_else(&invalid)?;
        }
        let mut pre = Vec::new();
        if let Some(pre_text) = pre_text {
            if pre_text.is_empty() {
                return Err(invalid());
            }
            for part in pre_text.split('.') {
                if !identifier_ok(part) {
                    return Err(invalid());
                }
                if part.bytes().all(|byte| byte.is_ascii_digit()) {
                    pre.push(Identifier::Numeric(numeric(part).ok_or_else(&invalid)?));
                } else {
                    pre.push(Identifier::Alpha(part.to_string()));
                }
            }
        }
        Ok(Self {
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
            pre,
            build,
        })
    }

    pub fn uniqueness_key(&self) -> String {
        Self {
            major: self.major,
            minor: self.minor,
            patch: self.patch,
            pre: self.pre.clone(),
            build: Vec::new(),
        }
        .to_string()
    }

    pub fn is_stable(&self) -> bool {
        self.pre.is_empty()
    }

    pub fn precedence_cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| pre_cmp(&self.pre, &other.pre))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            formatter.write_str("-")?;
            for (index, identifier) in self.pre.iter().enumerate() {
                if index > 0 {
                    formatter.write_str(".")?;
                }
                match identifier {
                    Identifier::Numeric(number) => write!(formatter, "{number}")?,
                    Identifier::Alpha(text) => formatter.write_str(text)?,
                }
            }
        }
        if !self.build.is_empty() {
            formatter.write_str("+")?;
            formatter.write_str(&self.build.join("."))?;
        }
        Ok(())
    }
}

pub fn compare_text(left: &str, right: &str) -> Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => left.precedence_cmp(&right),
        _ => left.cmp(right),
    }
}

pub fn compare_key(left: &Version, right: &Version) -> Ordering {
    left.precedence_cmp(right)
}

fn numeric(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

fn identifier_ok(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn pre_cmp(left: &[Identifier], right: &[Identifier]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (left, right) in left.iter().zip(right.iter()) {
                let ordering = match (left, right) {
                    (Identifier::Numeric(left), Identifier::Numeric(right)) => left.cmp(right),
                    (Identifier::Alpha(left), Identifier::Alpha(right)) => left.cmp(right),
                    (Identifier::Numeric(_), Identifier::Alpha(_)) => Ordering::Less,
                    (Identifier::Alpha(_), Identifier::Numeric(_)) => Ordering::Greater,
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            left.len().cmp(&right.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Version, compare_text};
    use std::cmp::Ordering;

    #[test]
    fn precedence_orders_prereleases_before_releases_and_ignores_build_metadata() {
        assert_eq!(compare_text("1.0.0-alpha", "1.0.0"), Ordering::Less);
        assert_eq!(compare_text("1.0.0-alpha.1", "1.0.0-alpha"), Ordering::Greater);
        assert_eq!(compare_text("1.0.0-alpha", "1.0.0-beta"), Ordering::Less);
        assert_eq!(compare_text("1.0.0+one", "1.0.0+two"), Ordering::Equal);
        assert_eq!(compare_text("not-a-version", "zzz"), Ordering::Less);
        assert_eq!(compare_text("1.0.0", "not-a-version"), Ordering::Less);
    }

    #[test]
    fn formatting_preserves_build_metadata_and_drops_it_from_the_uniqueness_key() {
        let version = Version::parse("1.2.3-beta.1+meta.2").expect("version parses");
        assert_eq!(version.to_string(), "1.2.3-beta.1+meta.2");
        assert_eq!(version.uniqueness_key(), "1.2.3-beta.1");
        assert!(!version.is_stable());
    }

    #[test]
    fn rejects_leading_zeroes_and_malformed_cores() {
        assert!(Version::parse("1.02.3").is_err());
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3-").is_err());
        assert!(Version::parse("").is_err());
    }
}
