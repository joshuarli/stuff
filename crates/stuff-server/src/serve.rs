use crate::config::Route;
use std::path::{Path, PathBuf};
use stuff_http::{
    cache_policy_for, content_type_for, evaluate_preconditions, packument_tarball_digest,
    parse_npm_tarball_path, parse_range, Precondition, RangeError, RouteKind,
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
    let plan = match stuff_http::plan_route(raw_target) {
        Ok(plan) => plan,
        Err(_) => return Mapped::NotFound,
    };
    match &plan.scope {
        stuff_http::RouteScope::Current => {
            let Some(generation_dir) = current_generation_dir(route) else {
                return Mapped::NotFound;
            };
            map_generation_target(route, &generation_dir, &plan, true)
        }
        stuff_http::RouteScope::Generation(generation) => {
            let generation_dir = route.site_dir.join("generations").join(generation);
            map_generation_target(route, &generation_dir, &plan, false)
        }
        stuff_http::RouteScope::Site => Mapped::File(MappedFile {
            path: candidate_path(
                &route.site_dir,
                &plan.rel_path,
                plan.index_fallback.as_deref(),
            ),
            kind: plan.kind(),
        }),
    }
}

fn current_generation_dir(route: &Route) -> Option<PathBuf> {
    let link = route.site_dir.join("current");
    let target = std::fs::read_link(&link).ok()?;
    let mut components = target.components();
    let generations = components.next()?;
    let generation = components.next()?;
    if generations.as_os_str() != "generations" || components.next().is_some() {
        return None;
    }
    stuff_store::parse_generation_id(generation.as_os_str().to_str()?)?;
    Some(route.site_dir.join(generations).join(generation))
}

fn candidate_path(base: &Path, rel_path: &str, index_fallback: Option<&str>) -> PathBuf {
    let path = base.join(rel_path);
    let is_directory = std::fs::metadata(&path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false);
    match (is_directory, index_fallback) {
        (true, Some(fallback)) => base.join(fallback),
        _ => path,
    }
}

fn is_generation_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
}

fn map_generation_target(
    route: &Route,
    generation_dir: &Path,
    plan: &stuff_http::RoutePlan,
    allow_npm_tarball: bool,
) -> Mapped {
    if let Some(shared) = &plan.shared_store_path {
        if !generation_dir.join(stuff_http::GENERATION_CATALOG).is_file() {
            return Mapped::NotFound;
        }
        let generation_path = generation_dir.join(&plan.rel_path);
        return Mapped::File(MappedFile {
            path: if is_generation_file(&generation_path)
                && !has_symlink_component(route, &generation_path)
            {
                generation_path
            } else {
                route.site_dir.join(shared)
            },
            kind: plan.kind(),
        });
    }
    if allow_npm_tarball
        && let Some(aliased) = map_npm_tarball(route, generation_dir, &segments_of(&plan.rel_path))
    {
        return Mapped::File(aliased);
    }
    Mapped::File(MappedFile {
        path: candidate_path(generation_dir, &plan.rel_path, plan.index_fallback.as_deref()),
        kind: plan.kind(),
    })
}

fn segments_of(rel_path: &str) -> Vec<String> {
    rel_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

fn map_npm_tarball(route: &Route, generation_dir: &Path, rest: &[String]) -> Option<MappedFile> {
    let request = parse_npm_tarball_path(rest)?;
    let mut packument_path = generation_dir.to_path_buf();
    for seg in request.packument_segments() {
        packument_path.push(seg);
    }
    if has_symlink_component(route, &packument_path) {
        return None;
    }
    let packument = std::fs::read_to_string(&packument_path).ok()?;
    let digest = packument_tarball_digest(&packument, &request.version)?;
    Some(MappedFile {
        path: generation_dir.join("-/tarballs").join(format!("{digest}.tgz")),
        kind: RouteKind::ImmutableFile,
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
        let mut headers = vec![
            ("content-type".to_string(), "text/plain".to_string()),
            ("content-length".to_string(), message.len().to_string()),
        ];
        if status == 404 {
            headers.push(("cache-control".to_string(), "public, max-age=60".to_string()));
        }
        Self {
            status,
            reason,
            headers,
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
    let Ok(canon) = std::fs::canonicalize(&mapped.path) else {
        return FileResponse::text(404, "Not Found", "not found");
    };
    if !canon.starts_with(&route.canonical_site_dir) {
        return FileResponse::text(404, "Not Found", "not found");
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
        stuff_store::write_file_durable(&staged_path.join("catalog.json"), b"{}").unwrap();
        site.activate(staged_path, "gen1", &|_: &Path| Ok(true)).unwrap();
        let route = Route {
            prefix: String::new(),
            canonical_site_dir: std::fs::canonicalize(&site_dir).unwrap(),
            site_dir,
        };
        (dir, route)
    }

    #[test]
    fn root_llms_txt_resolves_through_current() {
        let (_tmp, route) = test_route();
        match map_request(&route, "/llms.txt") {
            Mapped::File(f) => {
                assert_eq!(f.kind, RouteKind::GenerationPointer);
                assert_eq!(
                    f.path,
                    route
                        .site_dir
                        .join("generations/gen1")
                        .join("llms.txt")
                );
            }
            Mapped::NotFound => panic!("should resolve"),
        }
        let no_headers: Vec<(String, String)> = Vec::new();
        let r = respond(&route, "/llms.txt", "GET", &no_headers);
        assert_eq!((r.status, r.reason), (404, "Not Found"));

        std::fs::write(
            route.site_dir.join("generations/gen1/llms.txt"),
            b"# Cargo registry\n",
        )
        .unwrap();
        let r = respond(&route, "/llms.txt", "GET", &no_headers);
        assert_eq!((r.status, r.reason), (200, "OK"));
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| k == "content-type" && v == "text/markdown; charset=utf-8"),
            "{:?}",
            r.headers
        );
    }

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TARBALL: &[u8] = b"0123456789tarball";

    fn seed_npm_generation(route: &Route) {
        let generation = route.site_dir.join("generations/gen1");
        let packument = format!(
            r#"{{"name":"NAME","versions":{{"1.2.3":{{"dist":{{"tarball":"http://h/current/-/tarballs/{DIGEST}.tgz"}}}}}}}}"#
        );
        fs::create_dir_all(generation.join("@scope")).unwrap();
        fs::write(generation.join("flat"), packument.replace("NAME", "flat")).unwrap();
        fs::write(
            generation.join("@scope/pkg"),
            packument.replace("NAME", "@scope/pkg"),
        )
        .unwrap();
        fs::create_dir_all(generation.join("-/tarballs")).unwrap();
        fs::write(generation.join(format!("-/tarballs/{DIGEST}.tgz")), TARBALL).unwrap();
    }

    #[test]
    fn npm_standard_tarball_path_serves_digest_object() {
        let (_tmp, route) = test_route();
        seed_npm_generation(&route);
        let no_headers: Vec<(String, String)> = Vec::new();
        let digest_path = format!("/current/-/tarballs/{DIGEST}.tgz");
        let digest = respond(&route, &digest_path, "GET", &no_headers);
        assert_eq!(digest.status, 200);
        let digest_cache = digest.headers.iter().find(|(k, _)| k == "cache-control").unwrap();
        assert_eq!(digest_cache.1, "public, max-age=31536000, immutable");

        for path in ["/current/flat/-/flat-1.2.3.tgz", "/current/@scope%2fpkg/-/pkg-1.2.3.tgz", "/current/@scope/pkg/-/pkg-1.2.3.tgz"] {
            let r = respond(&route, path, "GET", &no_headers);
            assert_eq!(r.status, 200, "{path}");
            assert!(matches!(&r.body, ResponseBody::File { len, .. } if *len == TARBALL.len() as u64));
            let cache = r.headers.iter().find(|(k, _)| k == "cache-control").unwrap();
            assert_eq!(cache.1, "public, max-age=31536000, immutable");
            assert_eq!(r.headers.iter().find(|(k, _)| k == "content-type"), digest.headers.iter().find(|(k, _)| k == "content-type"));
            let head = respond(&route, path, "HEAD", &no_headers);
            assert_eq!(head.status, 200);
            assert!(matches!(head.body, ResponseBody::Empty));
        }
    }

    #[test]
    fn npm_standard_tarball_path_honors_ranges_and_conditionals() {
        let (_tmp, route) = test_route();
        seed_npm_generation(&route);
        let path = "/current/@scope%2fpkg/-/pkg-1.2.3.tgz";
        let ok = respond(&route, path, "GET", &[]);
        let etag = ok.headers.iter().find(|(k, _)| k == "etag").unwrap().1.clone();

        let r = respond(&route, path, "GET", &[("Range".to_string(), "bytes=2-5".to_string())]);
        assert_eq!((r.status, r.reason), (206, "Partial Content"));
        assert!(matches!(r.body, ResponseBody::File { offset: 2, len: 4, .. }));
        assert!(r.headers.iter().any(|(k, v)| k == "content-range" && v == &format!("bytes 2-5/{}", TARBALL.len())));

        let r = respond(&route, path, "GET", &[("Range".to_string(), "bytes=99-".to_string())]);
        assert_eq!(r.status, 416);

        let r = respond(&route, path, "GET", &[("If-None-Match".to_string(), etag)]);
        assert_eq!(r.status, 304);

        let r = respond(&route, path, "GET", &[("If-Match".to_string(), "\"nope\"".to_string())]);
        assert_eq!(r.status, 412);
    }

    #[test]
    fn npm_standard_tarball_path_is_not_found_for_unknown_versions_and_packages() {
        let (_tmp, route) = test_route();
        seed_npm_generation(&route);
        for path in [
            "/current/flat/-/flat-9.9.9.tgz",
            "/current/flat/-/other-1.2.3.tgz",
            "/current/missing/-/missing-1.2.3.tgz",
            "/current/@scope/pkg/-/pkg-9.9.9.tgz",
            "/current/@scope/pkg/-/flat-1.2.3.tgz",
        ] {
            assert_eq!(respond(&route, path, "GET", &[]).status, 404, "{path}");
        }
    }

    #[test]
    fn a_site_directory_retargeted_after_startup_is_refused() {
        let (tmp, real) = test_route();
        let link = tmp.path().join("site-link");
        std::os::unix::fs::symlink(&real.site_dir, &link).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let through_link = |canonical_site_dir: std::path::PathBuf| Route {
            prefix: String::new(),
            site_dir: link.clone(),
            canonical_site_dir,
        };
        let started_here = through_link(fs::canonicalize(&real.site_dir).unwrap());
        assert_eq!(respond(&started_here, "/current/registry/config.json", "GET", &[]).status, 200);
        let started_elsewhere = through_link(fs::canonicalize(&elsewhere).unwrap());
        assert_eq!(respond(&started_elsewhere, "/current/registry/config.json", "GET", &[]).status, 404);
    }

    #[test]
    fn wheels_resolve_to_the_shared_store_through_current_and_generations() {
        let (_tmp, route) = test_route();
        fs::create_dir_all(route.site_dir.join("wheels")).unwrap();
        fs::write(route.site_dir.join("wheels/pkg-1.0-py3-none-any.whl"), b"wheel").unwrap();
        std::os::unix::fs::symlink("../../wheels", route.site_dir.join("generations/gen1/wheels")).unwrap();
        for path in [
            "/current/wheels/pkg-1.0-py3-none-any.whl",
            "/generations/gen1/wheels/pkg-1.0-py3-none-any.whl",
            "/wheels/pkg-1.0-py3-none-any.whl",
        ] {
            let r = respond(&route, path, "GET", &[]);
            assert_eq!(r.status, 200, "{path}");
            let cache = r.headers.iter().find(|(k, _)| k == "cache-control").unwrap();
            assert_eq!(cache.1, "public, max-age=31536000, immutable", "{path}");
        }
        for path in ["/current/wheels/missing.whl", "/current/wheels/../x", "/current/wheels/"] {
            assert_eq!(respond(&route, path, "GET", &[]).status, 404, "{path}");
        }
    }

    #[test]
    fn root_aliases_and_slashless_directories_resolve_through_current() {
        let (_tmp, route) = test_route();
        let generation = route.site_dir.join("generations/gen1");
        assert_eq!(respond(&route, "/", "GET", &[]).status, 404);
        fs::write(generation.join("index.html"), b"<html>root</html>").unwrap();
        fs::create_dir_all(generation.join("simple/proj")).unwrap();
        fs::write(generation.join("simple/proj/index.html"), b"<html>proj</html>").unwrap();
        let no_headers: Vec<(String, String)> = Vec::new();
        for (path, body) in [
            ("/", "<html>root</html>"),
            ("/index.html", "<html>root</html>"),
            ("/catalog.json", "{}"),
            ("/simple/proj", "<html>proj</html>"),
            ("/simple/proj/", "<html>proj</html>"),
            ("/current/simple/proj", "<html>proj</html>"),
            ("/current/simple/proj/", "<html>proj</html>"),
        ] {
            let r = respond(&route, path, "GET", &no_headers);
            assert_eq!(r.status, 200, "{path}");
            let cache = r.headers.iter().find(|(k, _)| k == "cache-control").unwrap();
            assert_eq!(cache.1, "public, max-age=60, stale-if-error=0", "{path}");
            if body == "{}" {
                assert!(matches!(&r.body, ResponseBody::File { path, .. } if path.ends_with("catalog.json")));
            } else {
                assert!(matches!(&r.body, ResponseBody::File { path, .. } if path.ends_with("index.html")), "{path}");
            }
        }
        assert_eq!(respond(&route, "/simple", "GET", &no_headers).status, 404);
    }

    #[test]
    fn missing_generations_and_unpublished_sites_do_not_expose_files() {
        let (_tmp, route) = test_route();
        fs::create_dir_all(route.site_dir.join("wheels")).unwrap();
        fs::write(route.site_dir.join("wheels/pkg-1.0-py3-none-any.whl"), b"wheel").unwrap();
        std::os::unix::fs::symlink("../../wheels", route.site_dir.join("generations/gen1/wheels")).unwrap();
        let no_headers: Vec<(String, String)> = Vec::new();
        let ok = respond(&route, "/generations/gen1/wheels/pkg-1.0-py3-none-any.whl", "GET", &no_headers);
        assert_eq!(ok.status, 200);
        let missing = respond(&route, "/generations/gen2/wheels/pkg-1.0-py3-none-any.whl", "GET", &no_headers);
        assert_eq!(missing.status, 404);
        let missing_current = respond(&route, "/current/-/tarballs/missing.tgz", "GET", &no_headers);
        assert_eq!(missing_current.status, 404);

        let empty = tempdir().unwrap();
        let unpublished = Route {
            prefix: String::new(),
            canonical_site_dir: std::fs::canonicalize(empty.path()).unwrap(),
            site_dir: empty.path().to_path_buf(),
        };
        assert_eq!(respond(&unpublished, "/current/x", "GET", &no_headers).status, 404);
        assert_eq!(respond(&unpublished, "/", "GET", &no_headers).status, 404);
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

        let r = respond(&route, "/generations/gen1/simple/proj", "GET", &no_headers);
        assert_eq!((r.status, r.reason), (200, "OK"));
        assert!(matches!(r.body, ResponseBody::File { len: 13, .. }));

        let r = respond(&route, "/generations/gen1/simple/empty/", "GET", &no_headers);
        assert_eq!(r.status, 404);
    }
}
