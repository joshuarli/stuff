#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    NotModified,
    Failed,
    None,
}

pub fn evaluate_preconditions(
    etag: Option<&str>,
    if_none_match: Option<&str>,
    if_match: Option<&str>,
) -> Precondition {
    if let Some(rule) = if_match
        && !rule_matches(rule, etag)
    {
        return Precondition::Failed;
    }
    if let Some(rule) = if_none_match
        && rule_matches(rule, etag)
    {
        return Precondition::NotModified;
    }
    Precondition::None
}

fn rule_matches(rule: &str, current: Option<&str>) -> bool {
    rule.split(',').any(|tag| {
        let tag = tag.trim();
        if tag == "*" {
            return current.is_some();
        }
        match current {
            Some(c) if !c.is_empty() => tag.trim_matches('"') == c.trim_matches('"'),
            _ => false,
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeRequest {
    pub start: u64,
    pub end_inclusive: u64,
}

impl RangeRequest {
    pub fn len(&self) -> u64 {
        self.end_inclusive.saturating_sub(self.start).saturating_add(1)
    }

    pub fn is_empty(&self) -> bool {
        self.end_inclusive < self.start
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    Invalid,
    Unsatisfiable,
}

pub fn parse_range(header: &str, size: u64) -> Result<Vec<RangeRequest>, RangeError> {
    let spec = header.strip_prefix("bytes=").ok_or(RangeError::Invalid)?;
    if size == 0 {
        return Err(RangeError::Unsatisfiable);
    }
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        let (first, last) = part.split_once('-').ok_or(RangeError::Invalid)?;
        let range = if first.is_empty() {
            let suffix: u64 = last.parse().map_err(|_| RangeError::Invalid)?;
            if suffix == 0 {
                return Err(RangeError::Invalid);
            }
            let start = size.saturating_sub(suffix);
            RangeRequest {
                start,
                end_inclusive: size - 1,
            }
        } else {
            let start: u64 = first.parse().map_err(|_| RangeError::Invalid)?;
            if start >= size {
                return Err(RangeError::Unsatisfiable);
            }
            let end = if last.is_empty() {
                size - 1
            } else {
                let end: u64 = last.parse().map_err(|_| RangeError::Invalid)?;
                end.min(size - 1)
            };
            if end < start {
                return Err(RangeError::Invalid);
            }
            RangeRequest {
                start,
                end_inclusive: end,
            }
        };
        out.push(range);
    }
    if out.is_empty() {
        return Err(RangeError::Invalid);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditionals() {
        let etag = Some("\"abc123\"");
        assert_eq!(
            evaluate_preconditions(etag, Some("\"abc123\""), None),
            Precondition::NotModified
        );
        assert_eq!(
            evaluate_preconditions(etag, Some("\"other\""), None),
            Precondition::None
        );
        assert_eq!(
            evaluate_preconditions(etag, Some("*"), None),
            Precondition::NotModified
        );
        assert_eq!(
            evaluate_preconditions(etag, None, Some("\"abc123\"")),
            Precondition::None
        );
        assert_eq!(
            evaluate_preconditions(etag, None, Some("\"other\"")),
            Precondition::Failed
        );
        assert_eq!(
            evaluate_preconditions(None, None, Some("*")),
            Precondition::Failed
        );
        assert_eq!(evaluate_preconditions(None, None, None), Precondition::None);
    }

    #[test]
    fn ranges() {
        assert_eq!(
            parse_range("bytes=0-99", 1000).unwrap(),
            [RangeRequest { start: 0, end_inclusive: 99 }]
        );
        assert_eq!(
            parse_range("bytes=900-", 1000).unwrap(),
            [RangeRequest { start: 900, end_inclusive: 999 }]
        );
        assert_eq!(
            parse_range("bytes=-100", 1000).unwrap(),
            [RangeRequest { start: 900, end_inclusive: 999 }]
        );
        assert_eq!(
            parse_range("bytes=0-0,-1", 1000).unwrap(),
            [
                RangeRequest { start: 0, end_inclusive: 0 },
                RangeRequest { start: 999, end_inclusive: 999 }
            ]
        );
        assert_eq!(parse_range("bytes=0-99", 50).unwrap()[0].end_inclusive, 49);
        assert_eq!(parse_range("bytes=1000-", 1000).unwrap_err(), RangeError::Unsatisfiable);
        assert_eq!(parse_range("items=0-1", 1000).unwrap_err(), RangeError::Invalid);
        assert_eq!(parse_range("bytes=5-2", 1000).unwrap_err(), RangeError::Invalid);
        assert_eq!(parse_range("bytes=0-99", 0).unwrap_err(), RangeError::Unsatisfiable);
    }
}
