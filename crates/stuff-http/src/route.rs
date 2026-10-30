#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    pub segments: Vec<String>,
}

impl ResolvedPath {
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn join(&self) -> String {
        self.segments.join("/")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    NotFound,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Empty => write!(f, "empty request path"),
            PathError::NotFound => write!(f, "path escapes the site root"),
        }
    }
}

impl std::error::Error for PathError {}

fn decode_segment(segment: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(segment.len());
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = hex_val(bytes[i + 1])?;
                let lo = hex_val(bytes[i + 2])?;
                let byte = hi * 16 + lo;
                if byte == 0 {
                    return None;
                }
                out.push(byte);
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn resolve_path(raw_target: &str) -> Result<ResolvedPath, PathError> {
    let path = raw_target.split('?').next().unwrap_or("");
    let path = path.strip_prefix('/').unwrap_or(path);
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    let mut segments = Vec::new();
    for raw in path.split('/') {
        if raw.is_empty() || raw == "." {
            continue;
        }
        // Percent-decoding happens per raw segment so `%2f` can address a
        // nested on-disk path (npm `@scope%2fpkg` lives at `@scope/pkg`).
        // Decoding produces bytes first: multi-byte UTF-8 sequences stay
        // intact, and invalid UTF-8 rejects the request. The decoded text
        // is re-split and any `..` rejects the request, so decoding never
        // escapes the site root.
        let decoded = decode_segment(raw).ok_or(PathError::NotFound)?;
        for part in decoded.split(|b| *b == b'/') {
            if part.is_empty() || part == b"." {
                continue;
            }
            if part == b".." {
                return Err(PathError::NotFound);
            }
            let text = std::str::from_utf8(part).map_err(|_| PathError::NotFound)?;
            segments.push(text.to_string());
        }
    }
    if segments.is_empty() {
        return Err(PathError::Empty);
    }
    Ok(ResolvedPath { segments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_nested_paths() {
        let r = resolve_path("/current/registry/config.json").unwrap();
        assert_eq!(r.segments, ["current", "registry", "config.json"]);
        let r = resolve_path("generations/abc123/simple/index.html").unwrap();
        assert_eq!(r.join(), "generations/abc123/simple/index.html");
    }

    #[test]
    fn scoped_package_encoding_round_trips() {
        let r = resolve_path("/current/@types%2fnode").unwrap();
        assert_eq!(r.segments, ["current", "@types", "node"]);
        assert_eq!(r.join(), "current/@types/node");
    }

    #[test]
    fn rejects_traversal_and_bad_encoding() {
        assert_eq!(resolve_path("/current/../secret").unwrap_err(), PathError::NotFound);
        assert_eq!(resolve_path("/current/%2e%2e/x").unwrap_err(), PathError::NotFound);
        assert_eq!(
            resolve_path("/current/%2e%2e%2fsecret").unwrap_err(),
            PathError::NotFound
        );
        assert_eq!(resolve_path("/current/%zz").unwrap_err(), PathError::NotFound);
        assert_eq!(resolve_path("/current/%2").unwrap_err(), PathError::NotFound);
        assert_eq!(resolve_path("/current/%00").unwrap_err(), PathError::NotFound);
        assert_eq!(resolve_path("/").unwrap_err(), PathError::Empty);
        let r = resolve_path("/current/-/%2f").unwrap();
        assert_eq!(r.segments, ["current", "-"]);
    }

    #[test]
    fn decodes_utf8_bytes_together() {
        let r = resolve_path("/current/%E2%82%AC").unwrap();
        assert_eq!(r.segments, ["current", "\u{20AC}"]);
        assert_eq!(resolve_path("/current/%FF").unwrap_err(), PathError::NotFound);
        assert_eq!(resolve_path("/current/%C3").unwrap_err(), PathError::NotFound);
        assert_eq!(resolve_path("/current/%C3%28").unwrap_err(), PathError::NotFound);
    }

    #[test]
    fn strips_query_and_collapses_slashes() {
        let r = resolve_path("//current//registry//dl?x=1").unwrap();
        assert_eq!(r.segments, ["current", "registry", "dl"]);
    }
}
