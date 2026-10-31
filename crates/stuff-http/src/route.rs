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

pub const GENERATION_CATALOG: &str = "catalog.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteScope {
    Current,
    Generation(String),
    Site,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePlan {
    pub scope: RouteScope,
    pub rel_path: String,
    pub index_fallback: Option<String>,
    pub shared_store_path: Option<String>,
}

impl RoutePlan {
    pub fn kind(&self) -> crate::cache::RouteKind {
        match self.scope {
            RouteScope::Current => {
                if is_immutable_asset(&self.rel_path) {
                    crate::cache::RouteKind::ImmutableFile
                } else {
                    crate::cache::RouteKind::GenerationPointer
                }
            }
            RouteScope::Generation(_) | RouteScope::Site => crate::cache::RouteKind::ImmutableFile,
        }
    }
}

fn segments_of(rel_path: &str) -> Vec<String> {
    rel_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn is_immutable_asset(rel_path: &str) -> bool {
    let segments = segments_of(rel_path);
    match segments.as_slice() {
        [first, ..] if first == "wheels" || first == "tarballs" => true,
        [dash, tarballs, ..] if dash == "-" && tarballs == "tarballs" => true,
        _ => crate::npm::parse_npm_tarball_path(&segments).is_some(),
    }
}

pub fn shared_store_path(rel_path: &str) -> Option<String> {
    if rel_path == "wheels" || rel_path.starts_with("wheels/") {
        return Some(rel_path.to_string());
    }
    if rel_path.starts_with("tarballs/") {
        return Some(rel_path.to_string());
    }
    rel_path
        .strip_prefix("-/tarballs/")
        .map(|rest| format!("tarballs/{rest}"))
}

pub fn plan_route(raw_target: &str) -> Result<RoutePlan, PathError> {
    let resolved = match resolve_path(raw_target) {
        Ok(resolved) => resolved,
        Err(PathError::Empty) => {
            return Ok(RoutePlan {
                scope: RouteScope::Current,
                rel_path: "index.html".to_string(),
                index_fallback: None,
                shared_store_path: None,
            });
        }
        Err(error) => return Err(error),
    };
    let path = raw_target.split('?').next().unwrap_or("");
    let trailing_slash = path.ends_with('/');
    let segments = resolved.segments;

    if let [name] = segments.as_slice()
        && matches!(name.as_str(), "index.html" | "catalog.json" | "llms.txt")
    {
        return Ok(RoutePlan {
            scope: RouteScope::Current,
            rel_path: name.clone(),
            index_fallback: None,
            shared_store_path: None,
        });
    }

    let (scope, rel) = match segments.as_slice() {
        [current, rest @ ..] if current == "current" => (RouteScope::Current, rest.join("/")),
        [simple, rest @ ..] if simple == "simple" => (RouteScope::Current, segments.join("/")),
        [generations, generation, rest @ ..] if generations == "generations" => {
            (RouteScope::Generation(generation.clone()), rest.join("/"))
        }
        _ => (RouteScope::Site, segments.join("/")),
    };
    let shared_store_path = shared_store_path(&rel);
    let (rel_path, index_fallback) = if rel.is_empty() {
        ("index.html".to_string(), None)
    } else if trailing_slash {
        (format!("{rel}/index.html"), None)
    } else {
        (rel.clone(), Some(format!("{rel}/index.html")))
    };
    Ok(RoutePlan {
        scope,
        rel_path,
        index_fallback,
        shared_store_path,
    })
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

    #[test]
    fn plans_current_aliases_and_directories() {
        for (path, rel) in [
            ("/", "index.html"),
            ("/index.html", "index.html"),
            ("/catalog.json", "catalog.json"),
            ("/llms.txt", "llms.txt"),
            ("/simple", "simple"),
            ("/simple/", "simple/index.html"),
            ("/simple/proj", "simple/proj"),
            ("/simple/proj/", "simple/proj/index.html"),
        ] {
            let plan = plan_route(path).unwrap();
            assert_eq!(plan.scope, RouteScope::Current, "{path}");
            assert_eq!(plan.rel_path, rel, "{path}");
            assert_eq!(plan.kind(), crate::cache::RouteKind::GenerationPointer, "{path}");
        }
        assert_eq!(
            plan_route("/simple/proj").unwrap().index_fallback.as_deref(),
            Some("simple/proj/index.html")
        );
        assert_eq!(
            plan_route("/current/simple/proj/").unwrap().rel_path,
            "simple/proj/index.html"
        );
        assert_eq!(
            plan_route("/current/registry/config.json").unwrap().index_fallback.as_deref(),
            Some("registry/config.json/index.html")
        );
    }

    #[test]
    fn plans_generation_and_site_paths_as_immutable() {
        let plan = plan_route("/generations/gen1/simple/proj").unwrap();
        assert_eq!(plan.scope, RouteScope::Generation("gen1".to_string()));
        assert_eq!(plan.rel_path, "simple/proj");
        assert_eq!(
            plan.index_fallback.as_deref(),
            Some("simple/proj/index.html")
        );
        assert_eq!(plan.kind(), crate::cache::RouteKind::ImmutableFile);

        let plan = plan_route("/tarballs/x.tgz").unwrap();
        assert_eq!(plan.scope, RouteScope::Site);
        assert_eq!(plan.kind(), crate::cache::RouteKind::ImmutableFile);
    }

    #[test]
    fn classifies_shared_digest_paths_as_immutable() {
        for path in [
            "/current/tarballs/x.tgz",
            "/current/-/tarballs/x.tgz",
            "/current/wheels/x.whl",
        ] {
            let plan = plan_route(path).unwrap();
            assert_eq!(plan.kind(), crate::cache::RouteKind::ImmutableFile, "{path}");
        }
        assert_eq!(
            shared_store_path("tarballs/x.tgz").as_deref(),
            Some("tarballs/x.tgz")
        );
        assert_eq!(
            shared_store_path("-/tarballs/x.tgz").as_deref(),
            Some("tarballs/x.tgz")
        );
        assert_eq!(
            shared_store_path("wheels/x.whl").as_deref(),
            Some("wheels/x.whl")
        );
        assert_eq!(
            plan_route("/current/-/tarballs/x.tgz").unwrap().shared_store_path.as_deref(),
            Some("tarballs/x.tgz")
        );
        assert_eq!(shared_store_path("catalog.json"), None);
    }
}
