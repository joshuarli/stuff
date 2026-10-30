use crate::config::Route;
use std::path::{Path, PathBuf};
use stuff_http::{
    cache_policy_for, content_type_for, evaluate_preconditions, parse_range, Precondition,
    RangeError, RouteKind,
};

#[derive(Debug, Clone)]
pub struct MappedFile {
    pub path: PathBuf,
    pub kind: RouteKind,
}

#[derive(Debug, Clone)]
pub enum Mapped {
    File(MappedFile),
    NotFound,
}

pub fn map_request(route: &Route, raw_target: &str) -> Mapped {
    let resolved = match stuff_http::resolve_path(raw_target) {
        Ok(r) => r,
        Err(stuff_http::PathError::Empty) => return map_root(route),
        Err(_) => return Mapped::NotFound,
    };
    let Some((first, rest)) = resolved.segments.split_first() else {
        return map_root(route);
    };
    if first == "current" {
        let link = route.site_dir.join("current");
        let Ok(target) = std::fs::read_link(&link) else {
            return Mapped::NotFound;
        };
        if target.components().count() != 2
            || target.components().next().is_none_or(|c| c.as_os_str() != "generations")
        {
            return Mapped::NotFound;
        }
        let mut path = route.site_dir.join(&target);
        for seg in rest {
            path.push(seg);
        }
        return Mapped::File(MappedFile {
            path,
            kind: RouteKind::GenerationPointer,
        });
    }
    let mut path = route.site_dir.clone();
    for seg in &resolved.segments {
        path.push(seg);
    }
    Mapped::File(MappedFile {
        path,
        kind: RouteKind::ImmutableFile,
    })
}

fn map_root(route: &Route) -> Mapped {
    let link = route.site_dir.join("current");
    let Ok(target) = std::fs::read_link(&link) else {
        return Mapped::NotFound;
    };
    if target.components().count() != 2
        || target.components().next().is_none_or(|c| c.as_os_str() != "generations")
    {
        return Mapped::NotFound;
    }
    let path = route.site_dir.join(&target).join("index.html");
    Mapped::File(MappedFile {
        path,
        kind: RouteKind::GenerationPointer,
    })
}

pub fn content_type_for_path(path: &Path) -> &'static str {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    content_type_for(name)
}

pub fn etag_for(size: u64, modified: Option<std::time::SystemTime>) -> Option<String> {
    modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| format!("\"{size}-{}\"", d.as_secs()))
}

pub fn cache_header_for(kind: RouteKind) -> String {
    cache_policy_for(kind).header_value()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseBody {
    Empty,
    Inline(Vec<u8>),
    File { path: PathBuf, offset: u64, len: u64 },
}

#[derive(Debug, Clone)]
pub struct FileResponse {
    pub status: u16,
    pub reason: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: ResponseBody,
}

impl FileResponse {
    fn text(status: u16, reason: &'static str, message: &str) -> Self {
        Self {
            status,
            reason,
            headers: vec![
                ("content-type".to_string(), "text/plain".to_string()),
                ("content-length".to_string(), message.len().to_string()),
            ],
            body: ResponseBody::Inline(message.as_bytes().to_vec()),
        }
    }
}

fn has_symlink_component(route: &Route, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(&route.site_dir) else {
        return true;
    };
    let mut cur = route.site_dir.clone();
    for comp in rel.components() {
        cur.push(comp);
        if std::fs::symlink_metadata(&cur)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

pub fn respond(
    route: &Route,
    raw_target: &str,
    method: &str,
    headers: &[(String, String)],
) -> FileResponse {
    if method != "GET" && method != "HEAD" {
        return FileResponse::text(405, "Method Not Allowed", "method not allowed");
    }
    let is_head = method == "HEAD";

    let Mapped::File(mapped) = map_request(route, raw_target) else {
        return FileResponse::text(404, "Not Found", "not found");
    };
    if has_symlink_component(route, &mapped.path) {
        return FileResponse::text(404, "Not Found", "not found");
    }
    let Ok(mut canon) = std::fs::canonicalize(&mapped.path) else {
        return FileResponse::text(404, "Not Found", "not found");
    };
    if !canon.starts_with(&route.canonical_site_dir) {
        return FileResponse::text(404, "Not Found", "not found");
    }
    if canon.is_dir() {
        canon.push("index.html");
        let Ok(index) = std::fs::canonicalize(&canon) else {
            return FileResponse::text(404, "Not Found", "not found");
        };
        if !index.starts_with(&route.canonical_site_dir) {
            return FileResponse::text(404, "Not Found", "not found");
        }
        canon = index;
    }
    let meta = match std::fs::metadata(&canon) {
        Ok(m) if m.is_file() => m,
        _ => return FileResponse::text(404, "Not Found", "not found"),
    };
    let size = meta.len();
    let etag = etag_for(size, meta.modified().ok());

    match evaluate_preconditions(
        etag.as_deref(),
        header(headers, "if-none-match"),
        header(headers, "if-match"),
    ) {
        Precondition::NotModified => {
            let mut resp = FileResponse {
                status: 304,
                reason: "Not Modified",
                headers: Vec::new(),
                body: ResponseBody::Empty,
            };
            if let Some(e) = etag {
                resp.headers.push(("etag".to_string(), e));
            }
            return resp;
        }
        Precondition::Failed => {
            return FileResponse::text(412, "Precondition Failed", "precondition failed");
        }
        Precondition::None => {}
    }

    let (status, reason, offset, length, content_range) = match header(headers, "range") {
        Some(spec) => match parse_range(spec, size) {
            Ok(ranges) if ranges.len() == 1 => {
                let r = ranges[0];
                (
                    206,
                    "Partial Content",
                    r.start,
                    r.len(),
                    Some(format!("bytes {}-{}/{}", r.start, r.end_inclusive, size)),
                )
            }
            Ok(_) => (200, "OK", 0, size, None),
            Err(RangeError::Unsatisfiable) => {
                let mut resp = FileResponse {
                    status: 416,
                    reason: "Range Not Satisfiable",
                    headers: vec![(
                        "content-range".to_string(),
                        format!("bytes */{size}"),
                    )],
                    body: ResponseBody::Empty,
                };
                if let Some(e) = etag {
                    resp.headers.push(("etag".to_string(), e));
                }
                return resp;
            }
            Err(RangeError::Invalid) => (200, "OK", 0, size, None),
        },
        None => (200, "OK", 0, size, None),
    };

    let mime = content_type_for_path(&canon).to_string();
    let body = if is_head || length == 0 {
        ResponseBody::Empty
    } else {
        ResponseBody::File {
            path: canon,
            offset,
            len: length,
        }
    };
    let mut resp_headers = vec![
        ("content-type".to_string(), mime),
        ("content-length".to_string(), length.to_string()),
        ("accept-ranges".to_string(), "bytes".to_string()),
        ("cache-control".to_string(), cache_header_for(mapped.kind)),
    ];
    if let Some(e) = etag {
        resp_headers.push(("etag".to_string(), e));
    }
    if let Some(cr) = content_range {
        resp_headers.push(("content-range".to_string(), cr));
    }
    FileResponse {
        status,
        reason,
        headers: resp_headers,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use stuff_store::Site;
    use tempfile::tempdir;

    fn test_route() -> (tempfile::TempDir, Route) {
        let dir = tempdir().unwrap();
        let site_dir = dir.path().join("site");
        let site = Site::new(site_dir.clone());
        site.ensure_layout().unwrap();
        let staged = site.stage().unwrap();
        let staged_path = staged.path().to_path_buf();
        fs::create_dir_all(staged_path.join("registry")).unwrap();
        stuff_store::write_file_durable(
            &staged_path.join("registry/config.json"),
            b"{}",
        )
        .unwrap();
        site.activate(staged_path, "gen1", &|_: &Path| Ok(true)).unwrap();
        let route = Route {
            prefix: String::new(),
            canonical_site_dir: std::fs::canonicalize(&site_dir).unwrap(),
            site_dir,
        };
        (dir, route)
    }

    #[test]
    fn current_resolves_through_symlink() {
        let (_tmp, route) = test_route();
        match map_request(&route, "/current/registry/config.json") {
            Mapped::File(f) => {
                assert_eq!(f.kind, RouteKind::GenerationPointer);
                assert_eq!(fs::read(&f.path).unwrap(), b"{}");
            }
            Mapped::NotFound => panic!("should resolve"),
        }
    }

    #[test]
    fn generations_resolve_directly() {
        let (_tmp, route) = test_route();
        match map_request(&route, "/generations/gen1/registry/config.json") {
            Mapped::File(f) => {
                assert_eq!(f.kind, RouteKind::ImmutableFile);
                assert_eq!(fs::read(&f.path).unwrap(), b"{}");
            }
            Mapped::NotFound => panic!("should resolve"),
        }
    }

    #[test]
    fn rejects_link_escape_and_traversal() {
        let (_tmp, route) = test_route();
        assert!(matches!(map_request(&route, "/current/../../etc"), Mapped::NotFound));
        assert!(matches!(
            map_request(&route, "/current/%2e%2e/x"),
            Mapped::NotFound
        ));
    }

    #[test]
    fn missing_current_link_is_not_found() {
        let dir = tempdir().unwrap();
        let site_dir = dir.path().join("empty");
        fs::create_dir_all(&site_dir).unwrap();
        let route = Route {
            prefix: String::new(),
            canonical_site_dir: std::fs::canonicalize(&site_dir).unwrap(),
            site_dir,
        };
        assert!(matches!(map_request(&route, "/current/x"), Mapped::NotFound));
    }

    #[test]
    fn root_serves_current_index() {
        let (_tmp, route) = test_route();
        let no_headers: Vec<(String, String)> = Vec::new();
        let r = respond(&route, "/", "GET", &no_headers);
        assert_eq!((r.status, r.reason), (404, "Not Found"));

        std::fs::write(route.site_dir.join("generations/gen1/index.html"), b"<html></html>").unwrap();
        let r = respond(&route, "/", "GET", &no_headers);
        assert_eq!((r.status, r.reason), (200, "OK"));

        match map_request(&route, "/current/%2e%2e/x") {
            Mapped::NotFound => {}
            Mapped::File(_) => panic!("traversal must not resolve"),
        }
    }

    #[test]
    fn respond_covers_methods_conditions_and_range() {
        let (_tmp, route) = test_route();
        let no_headers: Vec<(String, String)> = Vec::new();

        let r = respond(&route, "/current/registry/config.json", "POST", &no_headers);
        assert_eq!((r.status, r.reason), (405, "Method Not Allowed"));
        assert!(matches!(r.body, ResponseBody::Inline(_)));

        let r = respond(&route, "/current/registry/config.json", "GET", &no_headers);
        assert_eq!((r.status, r.reason), (200, "OK"));
        assert!(matches!(r.body, ResponseBody::File { len: 2, .. }));
        let etag = r.headers.iter().find(|(k, _)| k == "etag").unwrap().1.clone();

        let r = respond(
            &route,
            "/current/registry/config.json",
            "GET",
            &[("If-None-Match".to_string(), etag.clone())],
        );
        assert_eq!(r.status, 304);

        let r = respond(
            &route,
            "/current/registry/config.json",
            "GET",
            &[("If-Match".to_string(), "\"nope\"".to_string())],
        );
        assert_eq!(r.status, 412);

        let r = respond(
            &route,
            "/current/registry/config.json",
            "GET",
            &[("Range".to_string(), "bytes=0-0".to_string())],
        );
        assert_eq!((r.status, r.reason), (206, "Partial Content"));
        assert!(matches!(r.body, ResponseBody::File { offset: 0, len: 1, .. }));

        let r = respond(
            &route,
            "/current/registry/config.json",
            "GET",
            &[("Range".to_string(), "bytes=99-".to_string())],
        );
        assert_eq!(r.status, 416);

        let r = respond(&route, "/missing", "GET", &no_headers);
        assert_eq!(r.status, 404);
        let r = respond(&route, "/current/%2e%2e/x", "GET", &no_headers);
        assert_eq!(r.status, 404);
    }

    #[test]
    fn directory_serves_index_html() {
        let (tmp, route) = test_route();
        let no_headers: Vec<(String, String)> = Vec::new();
        let dir = tmp.path().join("site/generations/gen1/simple/proj");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("index.html"), b"<html></html>").unwrap();

        let r = respond(&route, "/generations/gen1/simple/proj/", "GET", &no_headers);
        assert_eq!((r.status, r.reason), (200, "OK"));
        assert!(matches!(r.body, ResponseBody::File { len: 13, .. }));

        let r = respond(&route, "/generations/gen1/simple/empty/", "GET", &no_headers);
        assert_eq!(r.status, 404);
    }
}
