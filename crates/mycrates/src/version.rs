#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreIdent {
    Numeric(u64),
    Alpha(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<PreIdent>,
    pub build: Vec<String>,
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

impl Version {
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() || s.len() > 256 {
            return Err(Error::Input(format!("invalid version '{s}'")));
        }
        let (core_pre, build) = match s.split_once('+') {
            Some((a, b)) => (a, Some(b)),
            None => (s, None),
        };
        let build_ids = match build {
            None => Vec::new(),
            Some(b) => {
                if b.is_empty() {
                    return Err(Error::Input(format!("invalid version '{s}'")));
                }
                let mut out = Vec::new();
                for part in b.split('.') {
                    if part.is_empty()
                        || !part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    {
                        return Err(Error::Input(format!("invalid version '{s}'")));
                    }
                    out.push(part.to_string());
                }
                out
            }
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
            return Err(Error::Input(format!("invalid version '{s}'")));
        }
        let major = parse_u64_strict(maj_s)
            .ok_or_else(|| Error::Input(format!("invalid version '{s}'")))?;
        let minor = parse_u64_strict(min_s)
            .ok_or_else(|| Error::Input(format!("invalid version '{s}'")))?;
        let patch = parse_u64_strict(pat_s)
            .ok_or_else(|| Error::Input(format!("invalid version '{s}'")))?;
        let mut pre_ids = Vec::new();
        if let Some(p) = pre {
            if p.is_empty() {
                return Err(Error::Input(format!("invalid version '{s}'")));
            }
            for part in p.split('.') {
                if part.is_empty()
                    || !part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                {
                    return Err(Error::Input(format!("invalid version '{s}'")));
                }
                if part.bytes().all(|c| c.is_ascii_digit()) {
                    if !no_leading_zeros(part) {
                        return Err(Error::Input(format!("invalid version '{s}'")));
                    }
                    pre_ids.push(PreIdent::Numeric(part.parse::<u64>().map_err(|_| {
                        Error::Input(format!("invalid version '{s}'"))
                    })?));
                } else {
                    pre_ids.push(PreIdent::Alpha(part.to_string()));
                }
            }
        }
        Ok(Version { major, minor, patch, pre: pre_ids, build: build_ids })
    }

    pub fn key(&self) -> String {
        self.to_string()
    }

    pub fn uniqueness_key(&self) -> String {
        let mut v = self.clone();
        v.build.clear();
        v.to_string()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-")?;
            for (i, id) in self.pre.iter().enumerate() {
                if i > 0 {
                    write!(f, ".")?;
                }
                match id {
                    PreIdent::Numeric(n) => write!(f, "{n}")?,
                    PreIdent::Alpha(s) => write!(f, "{s}")?,
                }
            }
        }
        if !self.build.is_empty() {
            write!(f, "+")?;
            write!(f, "{}", self.build.join("."))?;
        }
        Ok(())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct Predicate {
    op: Op,
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Vec<PreIdent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Caret,
    Tilde,
    Exact,
    Greater,
    GreaterEq,
    Less,
    LessEq,
    Wildcard,
}

type Partial = (Option<u64>, Option<u64>, Option<u64>, Vec<PreIdent>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReq {
    predicates: Vec<Predicate>,
}

fn parse_partial(s: &str) -> Result<Partial> {
    let s = s.trim();
    if s.is_empty() || s == "*" {
        return Ok((None, None, None, Vec::new()));
    }
    let lower = s.to_lowercase();
    if lower == "x" {
        return Ok((None, None, None, Vec::new()));
    }
    let (core_pre, _build) = match s.split_once('+') {
        Some((a, _)) => (a, true),
        None => (s, false),
    };
    let (core, pre_s) = match core_pre.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (core_pre, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() > 3 {
        return Err(Error::Input(format!("invalid version_req '{s}'")));
    }
    let mut nums: Vec<Option<u64>> = Vec::new();
    for p in &parts {
        let pl = p.to_lowercase();
        if p.is_empty() {
            return Err(Error::Input(format!("invalid version_req '{s}'")));
        }
        if *p == "*" || pl == "x" {
            nums.push(None);
            continue;
        }
        if !p.bytes().all(|b| b.is_ascii_digit()) || !no_leading_zeros(p) {
            return Err(Error::Input(format!("invalid version_req '{s}'")));
        }
        nums.push(Some(p.parse::<u64>().map_err(|_| {
            Error::Input(format!("invalid version_req '{s}'"))
        })?));
    }
    while nums.len() < 3 {
        if nums.iter().any(|n| n.is_none()) {
            nums.push(None);
        } else {
            break;
        }
    }
    if nums.len() == 1 {
        nums.push(None);
        nums.push(None);
    } else if nums.len() == 2 {
        nums.push(None);
    }
    for i in 1..nums.len() {
        if nums[i].is_some() && nums[i - 1].is_none() {
            return Err(Error::Input(format!("invalid version_req '{s}'")));
        }
    }
    let mut pre = Vec::new();
    if let Some(p) = pre_s {
        if p.is_empty() {
            return Err(Error::Input(format!("invalid version_req '{s}'")));
        }
        for part in p.split('.') {
            if part.is_empty()
                || !part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return Err(Error::Input(format!("invalid version_req '{s}'")));
            }
            if part.bytes().all(|c| c.is_ascii_digit()) {
                if !no_leading_zeros(part) {
                    return Err(Error::Input(format!("invalid version_req '{s}'")));
                }
                pre.push(PreIdent::Numeric(part.parse::<u64>().map_err(|_| {
                    Error::Input(format!("invalid version_req '{s}'"))
                })?));
            } else {
                pre.push(PreIdent::Alpha(part.to_string()));
            }
        }
    }
    Ok((nums[0], nums[1], nums[2], pre))
}

impl VersionReq {
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            return Err(Error::Input("empty version_req".to_string()));
        }
        if s == "*" {
            return Ok(VersionReq {
                predicates: vec![Predicate { op: Op::Wildcard, major: None, minor: None, patch: None, pre: Vec::new() }],
            });
        }
        if let Some((a, b)) = s.split_once(" - ") {
            let lo = a.trim();
            let hi = b.trim();
            if lo.is_empty() || hi.is_empty() {
                return Err(Error::Input(format!("invalid version_req '{s}'")));
            }
            let (maj, min, pat, pre) = parse_partial(lo)?;
            let lo_pred = Predicate { op: Op::GreaterEq, major: maj, minor: min, patch: pat, pre };
            let (maj2, min2, pat2, pre2) = parse_partial(hi)?;
            let hi_pred = Predicate { op: Op::LessEq, major: maj2, minor: min2, patch: pat2, pre: pre2 };
            return Ok(VersionReq { predicates: vec![lo_pred, hi_pred] });
        }
        let mut predicates = Vec::new();
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(Error::Input(format!("invalid version_req '{s}'")));
            }
            let (op, rest) = if let Some(r) = part.strip_prefix("^") {
                (Op::Caret, r.trim())
            } else if let Some(r) = part.strip_prefix('~') {
                (Op::Tilde, r.trim())
            } else if let Some(r) = part.strip_prefix(">=") {
                (Op::GreaterEq, r.trim())
            } else if let Some(r) = part.strip_prefix("<=") {
                (Op::LessEq, r.trim())
            } else if let Some(r) = part.strip_prefix('>') {
                (Op::Greater, r.trim())
            } else if let Some(r) = part.strip_prefix('<') {
                (Op::Less, r.trim())
            } else if let Some(r) = part.strip_prefix('=') {
                (Op::Exact, r.trim())
            } else {
                (Op::Caret, part)
            };
            if rest.is_empty() {
                return Err(Error::Input(format!("invalid version_req '{s}'")));
            }
            let (maj, min, pat, pre) = parse_partial(rest)?;
            if maj.is_none() && (op != Op::Wildcard) {
                if rest == "*" || rest.to_lowercase() == "x" {
                    predicates.push(Predicate { op: Op::Wildcard, major: None, minor: None, patch: None, pre: Vec::new() });
                    continue;
                }
                return Err(Error::Input(format!("invalid version_req '{s}'")));
            }
            predicates.push(Predicate { op, major: maj, minor: min, patch: pat, pre });
        }
        if predicates.is_empty() {
            return Err(Error::Input(format!("invalid version_req '{s}'")));
        }
        Ok(VersionReq { predicates })
    }

    pub fn matches(&self, version: &Version) -> bool {
        if !version.pre.is_empty() {
            let mut allowed = false;
            for p in &self.predicates {
                if !p.pre.is_empty()
                    && p.major == Some(version.major)
                    && p.minor == Some(version.minor)
                    && p.patch == Some(version.patch)
                {
                    allowed = true;
                    break;
                }
            }
            if !allowed {
                return false;
            }
        }
        self.predicates.iter().all(|p| predicate_matches(p, version))
    }
}

fn cmp_full(maj: u64, min: u64, pat: u64, pre: &[PreIdent], v: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match maj.cmp(&v.major) {
        Ordering::Equal => {}
        o => return o,
    }
    match min.cmp(&v.minor) {
        Ordering::Equal => {}
        o => return o,
    }
    match pat.cmp(&v.patch) {
        Ordering::Equal => {}
        o => return o,
    }
    match (pre.is_empty(), v.pre.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (a, b) in pre.iter().zip(v.pre.iter()) {
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
            pre.len().cmp(&v.pre.len())
        }
    }
}

fn predicate_matches(p: &Predicate, v: &Version) -> bool {
    use std::cmp::Ordering;
    match p.op {
        Op::Wildcard => {
            if let Some(maj) = p.major {
                if v.major != maj {
                    return false;
                }
            } else {
                return true;
            }
            if let Some(min) = p.minor {
                if v.minor != min {
                    return false;
                }
            } else {
                return true;
            }
            if let Some(pat) = p.patch
                && v.patch != pat {
                    return false;
                }
            true
        }
        Op::Exact => {
            let (maj, min, pat) = match (p.major, p.minor, p.patch) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                (Some(a), Some(b), None) => {
                    return v.major == a && v.minor == b;
                }
                (Some(a), None, None) => {
                    return v.major == a;
                }
                _ => return false,
            };
            cmp_full(maj, min, pat, &p.pre, v) == Ordering::Equal
        }
        Op::Greater => {
            let (maj, min, pat) = fill_partial(p, v);
            cmp_full(maj, min, pat, &p.pre, v) == Ordering::Less
        }
        Op::GreaterEq => {
            let (maj, min, pat) = fill_partial(p, v);
            let o = cmp_full(maj, min, pat, &p.pre, v);
            o == Ordering::Less || o == Ordering::Equal
        }
        Op::Less => {
            let (maj, min, pat) = fill_partial(p, v);
            cmp_full(maj, min, pat, &p.pre, v) == Ordering::Greater
        }
        Op::LessEq => {
            let (maj, min, pat) = fill_partial(p, v);
            let o = cmp_full(maj, min, pat, &p.pre, v);
            o == Ordering::Greater || o == Ordering::Equal
        }
        Op::Tilde => {
            let maj = p.major.unwrap_or(0);
            let (lo_maj, lo_min, lo_pat) = fill_partial(p, v);
            if cmp_full(lo_maj, lo_min, lo_pat, &p.pre, v) == Ordering::Greater {
                return false;
            }
            let upper = match (p.minor, p.patch) {
                (None, _) => (maj + 1, 0, 0),
                (Some(min), _) => (maj, min + 1, 0),
            };
            compare_upper(v, &upper)
        }
        Op::Caret => {
            let maj = p.major.unwrap_or(0);
            match (p.minor, p.patch) {
                (Some(min), Some(pat)) => {
                    let upper_exclusive = caret_upper(maj, min, pat);
                    if cmp_full(maj, min, pat, &p.pre, v) == Ordering::Greater {
                        return false;
                    }
                    compare_upper(v, &upper_exclusive)
                }
                (Some(min), None) => {
                    if maj > 0 {
                        if v.major != maj {
                            return false;
                        }
                        if v.minor < min {
                            return false;
                        }
                        true
                    } else if min > 0 {
                        v.major == 0 && v.minor == min
                    } else {
                        v.major == 0 && v.minor == 0
                    }
                }
                (None, None) => {
                    if maj > 0 {
                        v.major == maj
                    } else {
                        v.major == 0
                    }
                }
                _ => false,
            }
        }
    }
}

fn fill_partial(p: &Predicate, _v: &Version) -> (u64, u64, u64) {
    (p.major.unwrap_or(0), p.minor.unwrap_or(0), p.patch.unwrap_or(0))
}

fn caret_upper(maj: u64, min: u64, pat: u64) -> (u64, u64, u64) {
    if maj > 0 {
        (maj + 1, 0, 0)
    } else if min > 0 {
        (0, min + 1, 0)
    } else {
        (0, 0, pat + 1)
    }
}

fn compare_upper(v: &Version, upper: &(u64, u64, u64)) -> bool {
    use std::cmp::Ordering;
    let o = cmp_full(upper.0, upper.1, upper.2, &[], v);
    o == Ordering::Greater
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(req: &str, ver: &str) -> bool {
        let r = VersionReq::parse(req).unwrap();
        let v = Version::parse(ver).unwrap();
        r.matches(&v)
    }

    #[test]
    fn caret_basics() {
        assert!(m("1.2.3", "1.5.0"));
        assert!(!m("1.2.3", "2.0.0"));
        assert!(m("^0.2.3", "0.2.9"));
        assert!(!m("^0.2.3", "0.3.0"));
        assert!(m("^0.0.3", "0.0.3"));
        assert!(!m("^0.0.3", "0.0.4"));
        assert!(m("*", "9.9.9"));
        assert!(m("1", "1.9.0"));
        assert!(!m("1", "2.0.0"));
        assert!(m("~1.2.3", "1.2.9"));
        assert!(!m("~1.2.3", "1.3.0"));
        assert!(m(">=1.0, <2.0", "1.5.0"));
        assert!(!m(">=1.0, <2.0", "2.0.0"));
        assert!(m("=1.2.3", "1.2.3"));
        assert!(!m("=1.2.3", "1.2.4"));
    }

    #[test]
    fn version_order() {
        let a = Version::parse("1.0.0-alpha").unwrap();
        let b = Version::parse("1.0.0").unwrap();
        assert!(a < b);
        assert_eq!(Version::parse("1.0.0+build").unwrap().uniqueness_key(), "1.0.0");
    }
}
