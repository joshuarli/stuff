use crate::map::{MappedAsset, RegistryRoute, WorkerConfig, map_asset, object_key_for, parse_pointer};
use stuff_http::{
    RouteKind as HttpRouteKind, cache_policy_for, content_type_for, evaluate_preconditions,
    parse_range, Precondition,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, deser::Deserialize)]
pub struct RouteEntry {
    #[deser(default)]
    pub host: String,
    #[deser(default)]
    pub url_prefix: String,
    pub bucket_binding: String,
    #[deser(default)]
    pub key_prefix: String,
}

pub fn parse_routes(raw: &str) -> Result<WorkerConfig, String> {
    let routes: Vec<RouteEntry> =
        deser_json::from_str(raw).map_err(|e| format!("STUFF_ROUTES: {e}"))?;
    Ok(WorkerConfig {
        routes: routes
            .into_iter()
            .map(|r| RegistryRoute {
                host: r.host,
                url_prefix: r.url_prefix,
                bucket_binding: r.bucket_binding,
                key_prefix: r.key_prefix,
            })
            .collect(),
    })
}

fn to_json<T: deser::Serialize + ?Sized>(value: &T, fallback_kind: &str) -> String {
    deser_json::to_string(value)
        .unwrap_or_else(|_| format!(r#"{{"kind":"{fallback_kind}"}}"#))
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct MappedJson {
    pub kind: String,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub bucket_binding: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub key_prefix: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub pointer_key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub rel_path: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub message: Option<String>,
}

pub fn map_asset_json(routes_json: &str, host: &str, raw_path: &str) -> String {
    let config = match parse_routes(routes_json) {
        Ok(config) => config,
        Err(message) => {
            return to_json(
                &MappedJson {
                    kind: "error".to_string(),
                    bucket_binding: None,
                    key: None,
                    key_prefix: None,
                    pointer_key: None,
                    rel_path: None,
                    message: Some(message),
                },
                "error",
            );
        }
    };
    let mapped = match map_asset(&config, host, raw_path) {
        MappedAsset::Direct(asset) => MappedJson {
            kind: "direct".to_string(),
            bucket_binding: Some(asset.bucket_binding),
            key: Some(asset.key),
            key_prefix: Some(asset.key_prefix),
            pointer_key: None,
            rel_path: None,
            message: None,
        },
        MappedAsset::ViaPointer {
            bucket_binding,
            pointer_key,
            rel_path,
        } => MappedJson {
            kind: "via_pointer".to_string(),
            bucket_binding: Some(bucket_binding),
            key: None,
            key_prefix: None,
            pointer_key: Some(pointer_key),
            rel_path: Some(rel_path),
            message: None,
        },
        MappedAsset::NotFound => MappedJson {
            kind: "not_found".to_string(),
            bucket_binding: None,
            key: None,
            key_prefix: None,
            pointer_key: None,
            rel_path: None,
            message: None,
        },
    };
    to_json(&mapped, "not_found")
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct PointerJson {
    pub kind: String,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub alias_key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub generation_catalog_key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub npm_packument_key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub npm_version: Option<String>,
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct DirectAliasJson {
    pub kind: String,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub alias_key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub generation_catalog_key: Option<String>,
}

/// Shared-store alias for generation-scoped paths that r2-sync does not
/// upload as separate objects (see the skip list in deploy/r2-sync.sh:
/// `generations/*/tarballs/*`, which also matches `-/tarballs/`, and the
/// symlinked `wheels/` dirs). Keep the two lists in agreement; the
/// `pointer_alias_covers_shared_stores` test below pins this side.
/// - `wheels/...`: mypypi publishes `generations/<id>/wheels` as a symlink
///   to the shared immutable wheel store.
/// - `tarballs/...` and `-/tarballs/...`: mynpm publishes hardlink aliases
///   of the shared immutable tarball store.
///
/// Wheel filenames and content-hash tarball names are immutable, so serving
/// the shared object is byte-identical to serving a generation copy.
fn alias_key_for(key_prefix: &str, rel_path: &str) -> Option<String> {
    let target = if rel_path == "wheels"
        || rel_path.starts_with("wheels/")
        || rel_path.starts_with("tarballs/")
    {
        rel_path
    } else if rel_path.starts_with("-/tarballs/") {
        &rel_path["-/".len()..]
    } else {
        return None;
    };
    let mut parts = Vec::new();
    if !key_prefix.is_empty() {
        parts.push(key_prefix);
    }
    parts.push(target);
    Some(parts.join("/"))
}

pub fn resolve_pointer_json(pointer_key: &str, pointer_text: &str, rel_path: &str) -> String {
    let resolved = match parse_pointer(pointer_text.as_bytes()) {
        Some(generation_id) => {
            let key_prefix = pointer_key.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            let segments: Vec<String> = rel_path.split('/').map(str::to_string).collect();
            let npm = stuff_http::parse_npm_tarball_path(&segments);
            PointerJson {
                kind: "key".to_string(),
                key: Some(object_key_for(key_prefix, &generation_id, rel_path)),
                alias_key: alias_key_for(key_prefix, rel_path),
                generation_catalog_key: Some(object_key_for(key_prefix, &generation_id, "catalog.json")),
                npm_packument_key: npm
                    .as_ref()
                    .map(|n| object_key_for(key_prefix, &generation_id, &n.package)),
                npm_version: npm.map(|n| n.version),
            }
        }
        None => PointerJson {
            kind: "not_found".to_string(),
            key: None,
            alias_key: None,
            generation_catalog_key: None,
            npm_packument_key: None,
            npm_version: None,
        },
    };
    to_json(&resolved, "not_found")
}

pub fn resolve_direct_alias_json(key_prefix: &str, key: &str) -> String {
    let base = if key_prefix.is_empty() {
        String::new()
    } else {
        format!("{key_prefix}/")
    };
    let resolved = key
        .strip_prefix(&base)
        .and_then(|rest| rest.strip_prefix("generations/"))
        .and_then(|rest| rest.split_once('/'))
        .filter(|(generation, _)| {
            !generation.is_empty()
                && generation.len() <= 128
                && generation
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .and_then(|(generation, rel_path)| {
            alias_key_for(key_prefix, rel_path).map(|alias_key| (generation, alias_key))
        });
    let json = match resolved {
        Some((generation, alias_key)) => DirectAliasJson {
            kind: "key".to_string(),
            alias_key: Some(alias_key),
            generation_catalog_key: Some(object_key_for(key_prefix, generation, "catalog.json")),
        },
        None => DirectAliasJson {
            kind: "not_found".to_string(),
            alias_key: None,
            generation_catalog_key: None,
        },
    };
    to_json(&json, "not_found")
}

pub fn resolve_npm_tarball_json(pointer_key: &str, packument_text: &str, version: &str) -> String {
    let resolved = match stuff_http::packument_tarball_digest(packument_text, version) {
        Some(digest) => {
            let key_prefix = pointer_key.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
            PointerJson {
                kind: "key".to_string(),
                key: None,
                alias_key: alias_key_for(key_prefix, &format!("tarballs/{digest}.tgz")),
                generation_catalog_key: None,
                npm_packument_key: None,
                npm_version: None,
            }
        }
        None => PointerJson {
            kind: "not_found".to_string(),
            key: None,
            alias_key: None,
            generation_catalog_key: None,
            npm_packument_key: None,
            npm_version: None,
        },
    };
    to_json(&resolved, "not_found")
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct UnifiedJson {
    pub kind: String,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub body: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub message: Option<String>,
}

pub fn render_unified_index_json(catalogs_json: &str) -> String {
    render_unified_json(catalogs_json, UnifiedBody::Html)
}

pub fn render_unified_llms_txt_json(catalogs_json: &str) -> String {
    render_unified_json(catalogs_json, UnifiedBody::Markdown)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnifiedBody {
    Html,
    Markdown,
}

fn render_unified_json(catalogs_json: &str, body: UnifiedBody) -> String {
    let parsed: deser_value::Value = match deser_json::from_str(catalogs_json) {
        Ok(v) => v,
        Err(e) => {
            return to_json(
                &UnifiedJson {
                    kind: "error".to_string(),
                    body: None,
                    message: Some(format!("catalogs: {e}")),
                },
                "error",
            );
        }
    };
    let Some(seq) = parsed.as_seq() else {
        return to_json(
            &UnifiedJson {
                kind: "error".to_string(),
                body: None,
                message: Some("catalogs must be an array".to_string()),
            },
            "error",
        );
    };
    let mut catalogs = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        let Some(text) = item.as_str() else {
            return to_json(
                &UnifiedJson {
                    kind: "error".to_string(),
                    body: None,
                    message: Some("catalogs must be an array of JSON strings".to_string()),
                },
                "error",
            );
        };
        match stuff_catalog::parse_catalog_json(text) {
            Ok(catalog) => catalogs.push(catalog),
            Err(e) => {
                return to_json(
                    &UnifiedJson {
                        kind: "error".to_string(),
                        body: None,
                        message: Some(format!("catalog: {e}")),
                    },
                    "error",
                );
            }
        }
    }
    catalogs.sort_by(|a, b| a.registry.as_str().cmp(b.registry.as_str()));
    let (kind, body) = match body {
        UnifiedBody::Html => ("html", stuff_catalog::render_unified(&catalogs)),
        UnifiedBody::Markdown => ("markdown", stuff_catalog::render_unified_llms_txt(&catalogs)),
    };
    to_json(
        &UnifiedJson {
            kind: kind.to_string(),
            body: Some(body),
            message: None,
        },
        "error",
    )
}

#[derive(Debug, Clone, Default, deser::Deserialize)]
pub struct RequestHeadersJson {
    #[deser(default)]
    pub if_none_match: Option<String>,
    #[deser(default)]
    pub if_match: Option<String>,
    #[deser(default)]
    pub range: Option<String>,
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct PlanJson {
    pub kind: String,
    #[deser(default)]
    pub status: u16,
    #[deser(default)]
    pub headers: BTreeMap<String, String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub body: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub range_start: Option<u64>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub range_length: Option<u64>,
    #[deser(default)]
    pub head: bool,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub message: Option<String>,
}

fn plan_error(message: String) -> String {
    to_json(
        &PlanJson {
            kind: "error".to_string(),
            status: 0,
            headers: BTreeMap::new(),
            body: None,
            range_start: None,
            range_length: None,
            head: false,
            message: Some(message),
        },
        "error",
    )
}

fn file_headers(
    route_kind: HttpRouteKind,
    file_name: &str,
    etag: &str,
    len: u64,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), content_type_for(file_name).to_string());
    headers.insert("content-length".to_string(), len.to_string());
    headers.insert("accept-ranges".to_string(), "bytes".to_string());
    headers.insert(
        "cache-control".to_string(),
        cache_policy_for(route_kind).header_value(),
    );
    headers.insert("etag".to_string(), etag.to_string());
    headers
}

fn file_name_of(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

pub fn plan_object_response_json(
    route: &str,
    method: &str,
    key: &str,
    headers_json: &str,
    size: u64,
    etag: &str,
) -> String {
    let route_kind = match route {
        "immutable" => HttpRouteKind::ImmutableFile,
        "pointer" => HttpRouteKind::GenerationPointer,
        _ => return plan_error(format!("unknown route: {route}")),
    };
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return plan_error(format!("unknown method: {method}")),
    };
    let request: RequestHeadersJson = match deser_json::from_str(headers_json) {
        Ok(request) => request,
        Err(e) => return plan_error(format!("headers: {e}")),
    };
    let plan = plan_object_response(
        route_kind,
        head,
        file_name_of(key),
        &request,
        size,
        etag,
    );
    to_json(&plan, "error")
}

fn plan_object_response(
    route_kind: HttpRouteKind,
    head: bool,
    file_name: &str,
    request: &RequestHeadersJson,
    size: u64,
    etag: &str,
) -> PlanJson {
    match evaluate_preconditions(
        Some(etag),
        request.if_none_match.as_deref(),
        request.if_match.as_deref(),
    ) {
        Precondition::NotModified => {
            let mut headers = BTreeMap::new();
            headers.insert("etag".to_string(), etag.to_string());
            return PlanJson {
                kind: "plan".to_string(),
                status: 304,
                headers,
                body: None,
                range_start: None,
                range_length: None,
                head,
                message: None,
            };
        }
        Precondition::Failed => {
            return PlanJson {
                kind: "plan".to_string(),
                status: 412,
                headers: BTreeMap::new(),
                body: Some("precondition failed".to_string()),
                range_start: None,
                range_length: None,
                head,
                message: None,
            };
        }
        Precondition::None => {}
    }

    match request.range.as_deref() {
        Some(spec) => match parse_range(spec, size) {
            Ok(ranges) if ranges.len() == 1 => {
                let r = ranges[0];
                let mut headers = file_headers(route_kind, file_name, etag, r.len());
                headers.insert(
                    "content-range".to_string(),
                    format!("bytes {}-{}/{}", r.start, r.end_inclusive, size),
                );
                PlanJson {
                    kind: "plan".to_string(),
                    status: 206,
                    headers,
                    body: None,
                    range_start: Some(r.start),
                    range_length: Some(r.len()),
                    head,
                    message: None,
                }
            }
            Ok(_) => full_response(route_kind, head, file_name, etag, size),
            Err(stuff_http::RangeError::Unsatisfiable) => {
                let mut headers = BTreeMap::new();
                headers.insert("content-range".to_string(), format!("bytes */{size}"));
                headers.insert("etag".to_string(), etag.to_string());
                PlanJson {
                    kind: "plan".to_string(),
                    status: 416,
                    headers,
                    body: None,
                    range_start: None,
                    range_length: None,
                    head,
                    message: None,
                }
            }
            Err(stuff_http::RangeError::Invalid) => {
                full_response(route_kind, head, file_name, etag, size)
            }
        },
        None => full_response(route_kind, head, file_name, etag, size),
    }
}

fn full_response(
    route_kind: HttpRouteKind,
    head: bool,
    file_name: &str,
    etag: &str,
    size: u64,
) -> PlanJson {
    PlanJson {
        kind: "plan".to_string(),
        status: 200,
        headers: file_headers(route_kind, file_name, etag, size),
        body: None,
        range_start: None,
        range_length: None,
        head,
        message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTES: &str = r#"[{"url_prefix":"mycrates","bucket_binding":"MYCRATES","key_prefix":""},{"url_prefix":"mirror","bucket_binding":"MYNPM","key_prefix":""},{"url_prefix":"pypi","bucket_binding":"PYPI","key_prefix":""}]"#;

    fn parse_mapped(s: &str) -> MappedJson {
        deser_json::from_str(s).expect("mapped json parses")
    }

    fn parse_plan(s: &str) -> PlanJson {
        deser_json::from_str(s).expect("plan json parses")
    }

    #[test]
    fn routes_require_bucket_binding_but_default_the_rest() {
        let config = parse_routes(r#"[{"bucket_binding":"B"}]"#).expect("parses");
        assert_eq!(config.routes.len(), 1);
        assert_eq!(config.routes[0].host, "");
        assert_eq!(config.routes[0].url_prefix, "");
        assert_eq!(config.routes[0].bucket_binding, "B");
        assert_eq!(config.routes[0].key_prefix, "");
    }

    #[test]
    fn routes_reject_invalid_json() {
        let err = parse_routes("not json").expect_err("rejects");
        assert!(err.starts_with("STUFF_ROUTES: "), "{err}");
    }

    #[test]
    fn routes_reject_missing_bucket_binding() {
        assert!(parse_routes(r#"[{"url_prefix":"x"}]"#).is_err());
    }

    #[test]
    fn map_json_shapes() {
        let direct = parse_mapped(&map_asset_json(ROUTES, "", "/mycrates/generations/abc/tarballs/x.tgz"));
        assert_eq!(direct.kind, "direct");
        assert_eq!(direct.bucket_binding.as_deref(), Some("MYCRATES"));
        assert_eq!(
            direct.key.as_deref(),
            Some("generations/abc/tarballs/x.tgz")
        );

        let via = parse_mapped(&map_asset_json(ROUTES, "", "/mirror/current/@flatrepo%2fdemo"));
        assert_eq!(via.kind, "via_pointer");
        assert_eq!(via.bucket_binding.as_deref(), Some("MYNPM"));
        assert_eq!(via.pointer_key.as_deref(), Some("current"));
        assert_eq!(via.rel_path.as_deref(), Some("@flatrepo/demo"));

        let missing = parse_mapped(&map_asset_json(ROUTES, "", "/nope/current/x"));
        assert_eq!(missing.kind, "not_found");

        let error = parse_mapped(&map_asset_json("bogus", "", "/mycrates/x"));
        assert_eq!(error.kind, "error");
        assert!(error.message.unwrap().starts_with("STUFF_ROUTES: "));
    }

    #[test]
    fn map_json_routes_by_host() {
        const HOST_ROUTES: &str = r#"[{"host":"crates.17166969.xyz","bucket_binding":"MYCRATES","key_prefix":""},{"host":"npm.17166969.xyz","bucket_binding":"MYNPM","key_prefix":""},{"host":"pypi.17166969.xyz","bucket_binding":"PYPI","key_prefix":""}]"#;
        let via = parse_mapped(&map_asset_json(
            HOST_ROUTES,
            "pypi.17166969.xyz",
            "/current/simple/proj/",
        ));
        assert_eq!(via.kind, "via_pointer");
        assert_eq!(via.bucket_binding.as_deref(), Some("PYPI"));
        assert_eq!(via.pointer_key.as_deref(), Some("current"));
        assert_eq!(via.rel_path.as_deref(), Some("simple/proj/index.html"));

        let cross = parse_mapped(&map_asset_json(
            HOST_ROUTES,
            "npm.17166969.xyz",
            "/current/simple/proj/",
        ));
        assert_eq!(cross.kind, "via_pointer");
        assert_eq!(cross.bucket_binding.as_deref(), Some("MYNPM"));

        let unknown = parse_mapped(&map_asset_json(HOST_ROUTES, "other.example", "/current/x"));
        assert_eq!(unknown.kind, "not_found");
    }

    #[test]
    fn pointer_json_resolves_scoped_keys() {
        let key = parse_plan_pointer(&resolve_pointer_json("current", "abc123", "flatdemo"));
        assert_eq!(key, "generations/abc123/flatdemo");

        let scoped = parse_plan_pointer(&resolve_pointer_json(
            "indexes/main/current",
            "gen9",
            "simple/index.html",
        ));
        assert_eq!(scoped, "indexes/main/generations/gen9/simple/index.html");

        let bad = deser_json::from_str::<PointerJson>(&resolve_pointer_json(
            "current",
            "generations/abc",
            "x",
        ))
        .expect("parses");
        assert_eq!(bad.kind, "not_found");
        assert_eq!(bad.key, None);
    }

    #[test]
    fn npm_standard_tarball_path_resolves_through_the_packument() {
        let flat = deser_json::from_str::<PointerJson>(&resolve_pointer_json(
            "current",
            "gen1",
            "acorn/-/acorn-8.1.0.tgz",
        ))
        .expect("parses");
        assert_eq!(flat.npm_packument_key.as_deref(), Some("generations/gen1/acorn"));
        assert_eq!(flat.npm_version.as_deref(), Some("8.1.0"));

        let scoped = deser_json::from_str::<PointerJson>(&resolve_pointer_json(
            "mirror/current",
            "gen1",
            "@types/node/-/node-22.0.0-beta.1.tgz",
        ))
        .expect("parses");
        assert_eq!(
            scoped.npm_packument_key.as_deref(),
            Some("mirror/generations/gen1/@types/node")
        );
        assert_eq!(scoped.npm_version.as_deref(), Some("22.0.0-beta.1"));

        let digest = "cd".repeat(64);
        let packument = format!(
            r#"{{"versions":{{"22.0.0-beta.1":{{"dist":{{"tarball":"https://h/mirror/current/-/tarballs/{digest}.tgz"}}}}}}}}"#
        );
        let alias = deser_json::from_str::<PointerJson>(&resolve_npm_tarball_json(
            "mirror/current",
            &packument,
            "22.0.0-beta.1",
        ))
        .expect("parses");
        assert_eq!(alias.kind, "key");
        assert_eq!(alias.alias_key, Some(format!("mirror/tarballs/{digest}.tgz")));

        let unknown = deser_json::from_str::<PointerJson>(&resolve_npm_tarball_json(
            "mirror/current",
            &packument,
            "9.9.9",
        ))
        .expect("parses");
        assert_eq!(unknown.kind, "not_found");
        assert_eq!(unknown.alias_key, None);

        let digest_url = deser_json::from_str::<PointerJson>(&resolve_pointer_json(
            "current",
            "gen1",
            "-/tarballs/x.tgz",
        ))
        .expect("parses");
        assert_eq!(digest_url.npm_packument_key, None);
    }

    #[test]
    fn npm_standard_tarball_alias_is_immutable_and_honors_ranges_and_conditionals() {
        let key = "tarballs/x.tgz";
        let full = parse_plan(&plan_object_response_json("immutable", "GET", key, "{}", 1000, "\"e\""));
        assert_eq!(full.status, 200);
        assert_eq!(
            full.headers.get("cache-control").map(String::as_str),
            Some("public, max-age=31536000, immutable")
        );

        let ranged = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            key,
            r#"{"range":"bytes=10-19"}"#,
            1000,
            "\"e\"",
        ));
        assert_eq!(ranged.status, 206);
        assert_eq!((ranged.range_start, ranged.range_length), (Some(10), Some(10)));
        assert_eq!(
            ranged.headers.get("content-range").map(String::as_str),
            Some("bytes 10-19/1000")
        );

        let unsatisfiable = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            key,
            r#"{"range":"bytes=5000-"}"#,
            1000,
            "\"e\"",
        ));
        assert_eq!(unsatisfiable.status, 416);

        let not_modified = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            key,
            r#"{"if_none_match":"\"e\""}"#,
            1000,
            "\"e\"",
        ));
        assert_eq!(not_modified.status, 304);

        let head = parse_plan(&plan_object_response_json("immutable", "HEAD", key, "{}", 1000, "\"e\""));
        assert!(head.head);
        assert_eq!(head.status, 200);
    }

    #[test]
    fn direct_generation_artifacts_resolve_shared_alias() {
        let wheel: DirectAliasJson = deser_json::from_str(&resolve_direct_alias_json(
            "",
            "generations/abc123/wheels/pkg-1.0-py3-none-any.whl",
        ))
        .expect("parses");
        assert_eq!(wheel.kind, "key");
        assert_eq!(
            wheel.alias_key.as_deref(),
            Some("wheels/pkg-1.0-py3-none-any.whl")
        );
        assert_eq!(
            wheel.generation_catalog_key.as_deref(),
            Some("generations/abc123/catalog.json")
        );

        let tarball: DirectAliasJson = deser_json::from_str(&resolve_direct_alias_json(
            "indexes/main",
            "indexes/main/generations/gen9/-/tarballs/abc.tgz",
        ))
        .expect("parses");
        assert_eq!(
            tarball.alias_key.as_deref(),
            Some("indexes/main/tarballs/abc.tgz")
        );
        assert_eq!(
            tarball.generation_catalog_key.as_deref(),
            Some("indexes/main/generations/gen9/catalog.json")
        );

        let page: DirectAliasJson = deser_json::from_str(&resolve_direct_alias_json(
            "",
            "generations/abc123/catalog.json",
        ))
        .expect("parses");
        assert_eq!(page.kind, "not_found");
    }

    #[test]
    fn pointer_alias_covers_shared_stores() {
        let wheels: PointerJson = deser_json::from_str(&resolve_pointer_json(
            "current",
            "abc123",
            "wheels/pkg-1.0-py3-none-any.whl",
        ))
        .expect("parses");
        assert_eq!(wheels.kind, "key");
        assert_eq!(
            wheels.key.as_deref(),
            Some("generations/abc123/wheels/pkg-1.0-py3-none-any.whl")
        );
        assert_eq!(
            wheels.alias_key.as_deref(),
            Some("wheels/pkg-1.0-py3-none-any.whl")
        );

        let nested: PointerJson = deser_json::from_str(&resolve_pointer_json(
            "indexes/main/current",
            "gen9",
            "-/tarballs/abc.tgz",
        ))
        .expect("parses");
        assert_eq!(
            nested.alias_key.as_deref(),
            Some("indexes/main/tarballs/abc.tgz")
        );

        let plain: PointerJson = deser_json::from_str(&resolve_pointer_json(
            "current",
            "abc123",
            "flatdemo",
        ))
        .expect("parses");
        assert_eq!(plain.alias_key, None);

        // Mirrors the r2-sync.sh skip list: generation-scoped tarball
        // hardlinks resolve to the shared store instead of being uploaded.
        let tarballs: PointerJson = deser_json::from_str(&resolve_pointer_json(
            "current",
            "abc123",
            "tarballs/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.tgz",
        ))
        .expect("parses");
        assert_eq!(
            tarballs.alias_key.as_deref(),
            Some("tarballs/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.tgz")
        );

        // The bare wheels dir is the symlinked shared store itself.
        let wheels_dir: PointerJson = deser_json::from_str(&resolve_pointer_json(
            "current",
            "abc123",
            "wheels",
        ))
        .expect("parses");
        assert_eq!(wheels_dir.alias_key.as_deref(), Some("wheels"));
    }

    fn parse_plan_pointer(s: &str) -> String {
        let parsed: PointerJson = deser_json::from_str(s).expect("pointer json parses");
        assert_eq!(parsed.kind, "key");
        parsed.key.expect("key present")
    }

    #[test]
    fn unified_render_combines_catalogs() {
        let cargo = r#"{"schema_version":1,"registry":"cargo","generation":"a","packages":[{"name":"h12tiny","source":"native","latest":"0.6.0","versions":[{"version":"0.6.0","yanked":false}]}]}"#;
        let npm = r#"{"schema_version":1,"registry":"npm","generation":"b","packages":[{"name":"@mine/utils","source":null,"latest":"1.3.0","versions":[{"version":"1.3.0","yanked":false}]}]}"#;
        let input = format!("[{},{}]", deser_json::to_string(&cargo).unwrap(), deser_json::to_string(&npm).unwrap());
        let parsed: UnifiedJson = deser_json::from_str(&render_unified_index_json(&input)).expect("parses");
        assert_eq!(parsed.kind, "html");
        let body = parsed.body.expect("html body");
        assert!(body.contains("h12tiny"));
        assert!(body.contains("@mine/utils"));

        let bad: UnifiedJson =
            deser_json::from_str(&render_unified_index_json("not json")).expect("parses");
        assert_eq!(bad.kind, "error");
    }

    #[test]
    fn unified_llms_txt_render_combines_catalogs() {
        let cargo = r#"{"schema_version":1,"registry":"cargo","generation":"a","packages":[{"name":"h12tiny","source":"native","latest":"0.6.0","versions":[{"version":"0.6.0","yanked":false}]}]}"#;
        let npm = r#"{"schema_version":1,"registry":"npm","generation":"b","packages":[{"name":"@mine/utils","source":null,"latest":"1.3.0","versions":[{"version":"1.3.0","yanked":false}]}]}"#;
        let input = format!("[{},{}]", deser_json::to_string(&cargo).unwrap(), deser_json::to_string(&npm).unwrap());
        let parsed: UnifiedJson =
            deser_json::from_str(&render_unified_llms_txt_json(&input)).expect("parses");
        assert_eq!(parsed.kind, "markdown");
        let body = parsed.body.expect("markdown body");
        assert!(body.starts_with("# stuff registries\n"));
        assert!(body.contains("h12tiny"));
        assert!(body.contains("@mine/utils"));

        let bad: UnifiedJson =
            deser_json::from_str(&render_unified_llms_txt_json("not json")).expect("parses");
        assert_eq!(bad.kind, "error");
    }

    #[test]
    fn headers_json_accepts_nulls_and_missing_keys() {
        let headers: RequestHeadersJson =
            deser_json::from_str(r#"{"if_none_match":null}"#).expect("parses");
        assert_eq!(headers.if_none_match, None);
        assert_eq!(headers.if_match, None);
        assert_eq!(headers.range, None);
    }

    #[test]
    fn plan_full_and_head() {
        let plan = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            "tarballs/x.tgz",
            "{}",
            1000,
            "abc",
        ));
        assert_eq!(plan.kind, "plan");
        assert_eq!(plan.status, 200);
        assert!(!plan.head);
        assert_eq!(plan.headers.get("content-type").map(String::as_str), Some("application/octet-stream"));
        assert_eq!(plan.headers.get("content-length").map(String::as_str), Some("1000"));
        assert_eq!(plan.headers.get("accept-ranges").map(String::as_str), Some("bytes"));
        assert_eq!(
            plan.headers.get("cache-control").map(String::as_str),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(plan.headers.get("etag").map(String::as_str), Some("abc"));

        let head = parse_plan(&plan_object_response_json(
            "pointer",
            "HEAD",
            "simple/index.html",
            "{}",
            10,
            "e",
        ));
        assert_eq!(head.status, 200);
        assert!(head.head);
        assert_eq!(
            head.headers.get("cache-control").map(String::as_str),
            Some("public, max-age=60")
        );
        assert_eq!(
            head.headers.get("content-type").map(String::as_str),
            Some("text/html; charset=utf-8")
        );
    }

    #[test]
    fn plan_conditionals() {
        let fresh = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            "config.json",
            r#"{"if_none_match":"\"abc\""}"#,
            100,
            "\"abc\"",
        ));
        assert_eq!(fresh.status, 304);
        assert_eq!(fresh.headers.get("etag").map(String::as_str), Some("\"abc\""));

        let failed = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            "config.json",
            r#"{"if_match":"\"other\""}"#,
            100,
            "\"abc\"",
        ));
        assert_eq!(failed.status, 412);
        assert_eq!(failed.body.as_deref(), Some("precondition failed"));
    }

    #[test]
    fn plan_single_range() {
        let plan = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            "pkg.tgz",
            r#"{"range":"bytes=0-99"}"#,
            1000,
            "e",
        ));
        assert_eq!(plan.status, 206);
        assert_eq!(plan.range_start, Some(0));
        assert_eq!(plan.range_length, Some(100));
        assert_eq!(
            plan.headers.get("content-range").map(String::as_str),
            Some("bytes 0-99/1000")
        );
        assert_eq!(plan.headers.get("content-length").map(String::as_str), Some("100"));
    }

    #[test]
    fn plan_range_fallbacks() {
        let multi = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            "pkg.tgz",
            r#"{"range":"bytes=0-0,-1"}"#,
            1000,
            "e",
        ));
        assert_eq!(multi.status, 200);
        assert_eq!(multi.range_start, None);

        let invalid = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            "pkg.tgz",
            r#"{"range":"items=0-1"}"#,
            1000,
            "e",
        ));
        assert_eq!(invalid.status, 200);

        let unsatisfiable = parse_plan(&plan_object_response_json(
            "immutable",
            "GET",
            "pkg.tgz",
            r#"{"range":"bytes=1000-"}"#,
            1000,
            "e",
        ));
        assert_eq!(unsatisfiable.status, 416);
        assert_eq!(
            unsatisfiable.headers.get("content-range").map(String::as_str),
            Some("bytes */1000")
        );
    }

    #[test]
    fn plan_rejects_bad_input() {
        for (route, method, headers) in [
            ("bogus", "GET", "{}"),
            ("immutable", "POST", "{}"),
            ("immutable", "GET", "{bad"),
        ] {
            let plan = parse_plan(&plan_object_response_json(route, method, "x", headers, 1, "e"));
            assert_eq!(plan.kind, "error", "{route} {method} {headers}");
        }
    }

    fn glob_matches(pattern: &str, value: &str) -> bool {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 1 {
            return pattern == value;
        }
        if !value.starts_with(parts[0]) {
            return false;
        }
        let mut rest = &value[parts[0].len()..];
        for (i, part) in parts[1..].iter().enumerate() {
            let is_last = i + 2 == parts.len();
            if part.is_empty() {
                return is_last || rest.is_empty();
            }
            if is_last {
                return rest.ends_with(part);
            }
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
        rest.is_empty()
    }

    #[test]
    fn shared_store_alias_list_matches_r2_sync_patterns() {
        let patterns: Vec<&str> = include_str!("../../../deploy/shared-store-paths.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        assert!(!patterns.is_empty());

        for rel in ["wheels/pkg-1.0-py3-none-any.whl", "tarballs/abc.tgz", "-/tarballs/abc.tgz"] {
            let generation_key = format!("generations/g1/{rel}");
            assert!(alias_key_for("", rel).is_some(), "{rel} must resolve to the shared store");
            assert!(
                patterns.iter().any(|p| glob_matches(p, &generation_key)),
                "{generation_key} must be skipped by deploy/r2-sync.sh"
            );
        }
        for rel in ["catalog.json", "registry/flatdemo", "simple/index.html", "-/other"] {
            let generation_key = format!("generations/g1/{rel}");
            assert!(alias_key_for("", rel).is_none(), "{rel} must not alias");
            assert!(
                !patterns.iter().any(|p| glob_matches(p, &generation_key)),
                "{generation_key} must be uploaded by deploy/r2-sync.sh"
            );
        }
    }
}
