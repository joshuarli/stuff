#![forbid(unsafe_code)]

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreIdent {
    Numeric(u64),
    Alpha(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<PreIdent>,
}

fn no_leading_zeros(s: &str) -> bool {
    s.len() <= 1 || !s.starts_with('0')
}

fn parse_u64_strict(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || !no_leading_zeros(s) {
        return None;
    }
    s.parse::<u64>().ok()
}

impl Semver {
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() || s.len() > 256 {
            return None;
        }
        let (core_pre, _) = match s.split_once('+') {
            Some((a, _)) => (a, true),
            None => (s, false),
        };
        let (core, pre) = match core_pre.split_once('-') {
            Some((a, b)) => (a, Some(b)),
            None => (core_pre, None),
        };
        let mut core_iter = core.split('.');
        let maj_s = core_iter.next().unwrap_or("");
        let min_s = core_iter.next().unwrap_or("");
        let pat_s = core_iter.next().unwrap_or("");
        if core_iter.next().is_some() {
            return None;
        }
        let major = parse_u64_strict(maj_s)?;
        let minor = parse_u64_strict(min_s)?;
        let patch = parse_u64_strict(pat_s)?;
        let mut pre_ids = Vec::new();
        if let Some(p) = pre {
            if p.is_empty() {
                return None;
            }
            for part in p.split('.') {
                if part.is_empty() || !part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
                    return None;
                }
                if part.bytes().all(|c| c.is_ascii_digit()) {
                    if !no_leading_zeros(part) {
                        return None;
                    }
                    pre_ids.push(PreIdent::Numeric(part.parse::<u64>().ok()?));
                } else {
                    pre_ids.push(PreIdent::Alpha(part.to_string()));
                }
            }
        }
        Some(Semver { major, minor, patch, pre: pre_ids })
    }

    pub fn is_stable(&self) -> bool {
        self.pre.is_empty()
    }

    pub fn cmp_precedence(&self, other: &Self) -> Ordering {
        match self.major.cmp(&other.major) {
            Ordering::Equal => {}
            o => return o,
        }
        match self.minor.cmp(&other.minor) {
            Ordering::Equal => {}
            o => return o,
        }
        match self.patch.cmp(&other.patch) {
            Ordering::Equal => {}
            o => return o,
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (a, b) in self.pre.iter().zip(other.pre.iter()) {
                    let o = match (a, b) {
                        (PreIdent::Numeric(x), PreIdent::Numeric(y)) => x.cmp(y),
                        (PreIdent::Numeric(_), PreIdent::Alpha(_)) => Ordering::Less,
                        (PreIdent::Alpha(_), PreIdent::Numeric(_)) => Ordering::Greater,
                        (PreIdent::Alpha(x), PreIdent::Alpha(y)) => x.cmp(y),
                    };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                self.pre.len().cmp(&other.pre.len())
            }
        }
    }
}

pub fn compare_semver(a: &str, b: &str) -> Ordering {
    match (Semver::parse(a), Semver::parse(b)) {
        (Some(x), Some(y)) => x.cmp_precedence(&y),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => a.cmp(b),
    }
}

pub fn compare_npm_version(a: &str, b: &str) -> Ordering {
    compare_semver(a, b)
}

pub fn is_stable_semver(version: &str) -> bool {
    Semver::parse(version).is_some_and(|v| v.is_stable())
}

pub fn parse_pep440(version: &str) -> Option<pep440_rs::Version> {
    version.parse::<pep440_rs::Version>().ok().filter(|v| v.to_string() == version)
}

pub fn is_stable_pep440(version: &pep440_rs::Version) -> bool {
    version.is_stable()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_orders_prerelease_below_release() {
        assert_eq!(compare_semver("1.0.0-alpha", "1.0.0"), Ordering::Less);
        assert_eq!(compare_semver("1.0.0", "1.0.0+build"), Ordering::Equal);
        assert_eq!(compare_semver("2.0.0", "10.0.0"), Ordering::Less);
    }

    #[test]
    fn stable_detection() {
        assert!(is_stable_semver("1.2.3"));
        assert!(!is_stable_semver("1.2.3-alpha.1"));
    }

    #[test]
    fn pep440_round_trip() {
        assert!(parse_pep440("1.0").is_some());
        assert!(parse_pep440("1.0a").is_none());
    }
}
