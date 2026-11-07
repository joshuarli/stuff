use std::cmp::Ordering;

use super::{input, Rejection};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalRequirement {
    Empty,
    Bounds {
        lower: Option<Bound>,
        upper: Option<Bound>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub version: VersionKey,
    pub inclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionKey {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<PreIdentifier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PreIdentifier {
    Numeric(String),
    Alpha(String),
}

impl Ord for PreIdentifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (PreIdentifier::Numeric(left), PreIdentifier::Numeric(right)) => {
                left.len().cmp(&right.len()).then_with(|| left.cmp(right))
            }
            (PreIdentifier::Alpha(left), PreIdentifier::Alpha(right)) => left.cmp(right),
            (PreIdentifier::Numeric(_), PreIdentifier::Alpha(_)) => Ordering::Less,
            (PreIdentifier::Alpha(_), PreIdentifier::Numeric(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for PreIdentifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VersionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| pre_cmp(&self.pre, &other.pre))
    }
}

impl PartialOrd for VersionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Caret,
    Tilde,
    Exact,
    GreaterEqual,
    LessEqual,
    Greater,
    Less,
}

struct Partial {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Vec<PreIdentifier>,
    starred: bool,
}

pub fn canonical_requirement(text: &str) -> Result<CanonicalRequirement, Rejection> {
    let stripped = text.trim();
    if stripped.is_empty() {
        return Err(input("empty version_req"));
    }
    let mut lower: Option<Bound> = None;
    let mut upper: Option<Bound> = None;
    for part in stripped.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(input(format!("invalid version_req '{text}'")));
        }
        let (operator, rest) = split_operator(part);
        let partial = parse_partial(rest, text)?;
        let (low, high) = comparator_interval(operator, partial)?;
        lower = tighter_lower(lower, low);
        upper = tighter_upper(upper, high);
    }
    if let (Some(lower), Some(upper)) = (&lower, &upper)
        && (lower.version > upper.version
            || (lower.version == upper.version && !(lower.inclusive && upper.inclusive)))
    {
        return Ok(CanonicalRequirement::Empty);
    }
    Ok(CanonicalRequirement::Bounds { lower, upper })
}

fn split_operator(part: &str) -> (Option<Operator>, &str) {
    const PREFIXES: [(&str, Operator); 7] = [
        ("^", Operator::Caret),
        ("~", Operator::Tilde),
        ("=", Operator::Exact),
        (">=", Operator::GreaterEqual),
        ("<=", Operator::LessEqual),
        (">", Operator::Greater),
        ("<", Operator::Less),
    ];
    for (prefix, operator) in PREFIXES {
        if let Some(rest) = part.strip_prefix(prefix) {
            return (Some(operator), rest.trim_start());
        }
    }
    (None, part.trim_start())
}

fn parse_partial(text: &str, original: &str) -> Result<Partial, Rejection> {
    let invalid = || input(format!("invalid version_req '{original}'"));
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid());
    }
    let text = text.split_once('+').map_or(text, |(core, _)| core);
    let (core, pre_text) = match text.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (text, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() > 3 {
        return Err(invalid());
    }
    let mut numbers: Vec<Option<u64>> = Vec::new();
    let mut starred = false;
    for part in parts {
        if matches!(part, "*" | "x" | "X") {
            starred = true;
            numbers.push(None);
            continue;
        }
        if !part.bytes().all(|byte| byte.is_ascii_digit())
            || part.is_empty()
            || (part.len() > 1 && part.starts_with('0'))
        {
            return Err(invalid());
        }
        let value: u64 = part.parse().map_err(|_| invalid())?;
        numbers.push(Some(value));
    }
    for index in 1..numbers.len() {
        if numbers[index].is_some() && numbers[index - 1].is_none() {
            return Err(invalid());
        }
    }
    while numbers.len() < 3 {
        numbers.push(None);
    }
    let mut pre = Vec::new();
    if let Some(pre_text) = pre_text {
        if numbers[2].is_none() || !prerelease_ok(pre_text) {
            return Err(invalid());
        }
        for part in pre_text.split('.') {
            if part.bytes().all(|byte| byte.is_ascii_digit()) {
                pre.push(PreIdentifier::Numeric(normalize_number(part)));
            } else {
                pre.push(PreIdentifier::Alpha(part.to_string()));
            }
        }
    }
    Ok(Partial {
        major: numbers[0],
        minor: numbers[1],
        patch: numbers[2],
        pre,
        starred,
    })
}

fn comparator_interval(
    operator: Option<Operator>,
    partial: Partial,
) -> Result<(Option<Bound>, Option<Bound>), Rejection> {
    let operator = match (partial.starred, operator) {
        (true, None | Some(Operator::Exact)) => Some(Operator::Tilde),
        (_, operator) => operator,
    };
    let wildcard = partial.major.is_none();
    let specified: u8 = if partial.major.is_none() {
        0
    } else if partial.minor.is_none() {
        1
    } else if partial.patch.is_none() {
        2
    } else {
        3
    };
    let floor = || Bound {
        version: VersionKey {
            major: partial.major.unwrap_or(0),
            minor: partial.minor.unwrap_or(0),
            patch: partial.patch.unwrap_or(0),
            pre: partial.pre.clone(),
        },
        inclusive: true,
    };
    let next_after = |level: u8| Bound {
        version: VersionKey {
            major: partial.major.unwrap_or(0) + u64::from(level == 0),
            minor: if level == 0 {
                0
            } else {
                partial.minor.unwrap_or(0) + u64::from(level == 1)
            },
            patch: if level < 2 {
                0
            } else {
                partial.patch.unwrap_or(0) + 1
            },
            pre: Vec::new(),
        },
        inclusive: false,
    };
    if wildcard {
        return match operator {
            None
            | Some(Operator::Caret)
            | Some(Operator::Exact)
            | Some(Operator::Tilde)
            | Some(Operator::GreaterEqual)
            | Some(Operator::LessEqual) => Ok((None, None)),
            Some(Operator::Greater) | Some(Operator::Less) => {
                Err(input("invalid version_req wildcard operator"))
            }
        };
    }
    match operator {
        Some(Operator::Exact) => {
            if specified == 3 {
                let exact = floor();
                Ok((Some(exact.clone()), Some(exact)))
            } else {
                Ok((Some(floor()), Some(next_after(specified - 1))))
            }
        }
        Some(Operator::Greater) => {
            if specified == 3 {
                Ok((
                    Some(Bound {
                        version: floor().version,
                        inclusive: false,
                    }),
                    None,
                ))
            } else {
                let next = next_after(specified - 1);
                Ok((
                    Some(Bound {
                        version: next.version,
                        inclusive: true,
                    }),
                    None,
                ))
            }
        }
        Some(Operator::GreaterEqual) => Ok((Some(floor()), None)),
        Some(Operator::Less) => Ok((
            None,
            Some(Bound {
                version: floor().version,
                inclusive: false,
            }),
        )),
        Some(Operator::LessEqual) => {
            if specified == 3 {
                Ok((
                    None,
                    Some(Bound {
                        version: floor().version,
                        inclusive: true,
                    }),
                ))
            } else {
                Ok((None, Some(next_after(specified - 1))))
            }
        }
        Some(Operator::Tilde) => Ok((
            Some(floor()),
            Some(next_after(if specified == 1 { 0 } else { 1 })),
        )),
        None | Some(Operator::Caret) => {
            if specified == 1 || partial.major.unwrap_or(0) != 0 {
                Ok((Some(floor()), Some(next_after(0))))
            } else if specified == 2 || partial.minor.unwrap_or(0) != 0 {
                Ok((Some(floor()), Some(next_after(1))))
            } else {
                Ok((Some(floor()), Some(next_after(2))))
            }
        }
    }
}

fn tighter_lower(left: Option<Bound>, right: Option<Bound>) -> Option<Bound> {
    match (left, right) {
        (None, right) => right,
        (left, None) => left,
        (Some(left), Some(right)) => Some(if left.version != right.version {
            if left.version > right.version {
                left
            } else {
                right
            }
        } else if !left.inclusive {
            left
        } else {
            right
        }),
    }
}

fn tighter_upper(left: Option<Bound>, right: Option<Bound>) -> Option<Bound> {
    match (left, right) {
        (None, right) => right,
        (left, None) => left,
        (Some(left), Some(right)) => Some(if left.version != right.version {
            if left.version < right.version {
                left
            } else {
                right
            }
        } else if !left.inclusive {
            left
        } else {
            right
        }),
    }
}

fn pre_cmp(left: &[PreIdentifier], right: &[PreIdentifier]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (left, right) in left.iter().zip(right.iter()) {
                let ordering = left.cmp(right);
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            left.len().cmp(&right.len())
        }
    }
}

fn prerelease_ok(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn normalize_number(digits: &str) -> String {
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{CanonicalRequirement, canonical_requirement};

    #[test]
    fn equivalent_requirements_compare_equal_by_interval() {
        let plain = canonical_requirement("1.0").expect("plain requirement");
        let caret = canonical_requirement("^1.0").expect("caret requirement");
        let pair = canonical_requirement(">=1.0.0, <2.0.0").expect("bounded requirement");
        assert_eq!(plain, caret);
        assert_eq!(plain, pair);
        assert_ne!(
            plain,
            canonical_requirement("1.1").expect("different requirement")
        );
    }

    #[test]
    fn wildcards_match_every_version() {
        assert_eq!(
            canonical_requirement("*").expect("wildcard"),
            CanonicalRequirement::Bounds {
                lower: None,
                upper: None
            }
        );
        assert!(canonical_requirement(">*").is_err());
    }

    #[test]
    fn contradictory_bounds_are_empty() {
        assert_eq!(
            canonical_requirement(">=2.0, <1.0").expect("contradiction"),
            CanonicalRequirement::Empty
        );
    }
}
