#[derive(Debug, Clone)]
pub struct RegistryRoute {
    pub url_prefix: String,
    pub bucket_binding: String,
    pub key_prefix: String,
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub routes: Vec<RegistryRoute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    ImmutableObject,
    ViaPointer,
}

#[derive(Debug, Clone)]
pub struct AssetKey {
    pub bucket_binding: String,
    pub key: String,
    pub kind: RouteKind,
}

#[derive(Debug, Clone)]
pub enum MappedAsset {
    Direct(AssetKey),
    ViaPointer {
        bucket_binding: String,
        pointer_key: String,
        rel_path: String,
    },
    NotFound,
}

fn join_key(prefix: &str, parts: &[&str]) -> String {
    let mut out = String::new();
    if !prefix.is_empty() {
        out.push_str(prefix);
    }
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    out
}

pub fn pointer_key(key_prefix: &str) -> String {
    join_key(key_prefix, &["current"])
}

pub fn object_key_for(key_prefix: &str, generation_id: &str, rel_path: &str) -> String {
    let mut parts = vec!["generations", generation_id];
    parts.extend(rel_path.split('/').filter(|s| !s.is_empty()));
    join_key(key_prefix, &parts)
}

pub fn parse_pointer(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() || text.len() > 128 {
        return None;
    }
    if !text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return None;
    }
    Some(text.to_string())
}

pub fn map_asset(config: &WorkerConfig, raw_path: &str) -> MappedAsset {
    let mut best: Option<(&RegistryRoute, String)> = None;
    for route in &config.routes {
        let rest = if route.url_prefix.is_empty() {
            raw_path.to_string()
        } else {
            let with_slash = format!("/{}/", route.url_prefix.trim_matches('/'));
            if raw_path == with_slash.trim_end_matches('/') {
                "/".to_string()
            } else if let Some(rest) = raw_path.strip_prefix(&with_slash) {
                format!("/{rest}")
            } else {
                continue;
            }
        };
        let replace = match &best {
            None => true,
            Some((r, _)) => {
                r.url_prefix.is_empty() || r.url_prefix.len() < route.url_prefix.len()
            }
        };
        if replace {
            best = Some((route, rest));
        }
    }
    let Some((route, rest)) = best else {
        return MappedAsset::NotFound;
    };

    let resolved = match stuff_http::resolve_path(&rest) {
        Ok(r) => r,
        Err(_) => return MappedAsset::NotFound,
    };
    let Some((first, remaining)) = resolved.segments.split_first() else {
        return MappedAsset::NotFound;
    };
    if first == "current" {
        let mut rel = remaining.join("/");
        if raw_path.ends_with('/') && !rel.is_empty() {
            rel.push_str("/index.html");
        }
        if rel.is_empty() {
            return MappedAsset::NotFound;
        }
        return MappedAsset::ViaPointer {
            bucket_binding: route.bucket_binding.clone(),
            pointer_key: pointer_key(&route.key_prefix),
            rel_path: rel,
        };
    }
    let mut parts: Vec<&str> = vec![route.key_prefix.as_str(), first];
    parts.extend(remaining.iter().map(|s| s.as_str()));
    let mut key = join_key("", &parts);
    if raw_path.ends_with('/') {
        if !key.is_empty() {
            key.push('/');
        }
        key.push_str("index.html");
    }
    MappedAsset::Direct(AssetKey {
        bucket_binding: route.bucket_binding.clone(),
        key,
        kind: RouteKind::ImmutableObject,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> WorkerConfig {
        WorkerConfig {
            routes: vec![
                RegistryRoute {
                    url_prefix: "mirror".to_string(),
                    bucket_binding: "MYNPM".to_string(),
                    key_prefix: String::new(),
                },
                RegistryRoute {
                    url_prefix: String::new(),
                    bucket_binding: "MYCRATES".to_string(),
                    key_prefix: String::new(),
                },
            ],
        }
    }

    #[test]
    fn longest_prefix_selects_bucket() {
        let c = config();
        match map_asset(&c, "/mirror/current/flatdemo") {
            MappedAsset::ViaPointer { bucket_binding, pointer_key, rel_path } => {
                assert_eq!(bucket_binding, "MYNPM");
                assert_eq!(pointer_key, "current");
                assert_eq!(rel_path, "flatdemo");
            }
            other => panic!("unexpected {other:?}"),
        }
        match map_asset(&c, "/current/registry/config.json") {
            MappedAsset::ViaPointer { bucket_binding, .. } => {
                assert_eq!(bucket_binding, "MYCRATES");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scoped_route_maps_to_nested_key() {
        let c = config();
        match map_asset(&c, "/mirror/current/@flatrepo%2fdemo") {
            MappedAsset::ViaPointer { rel_path, .. } => {
                assert_eq!(rel_path, "@flatrepo/demo");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn generation_and_direct_keys() {
        let c = config();
        match map_asset(&c, "/mirror/generations/abc/tarballs/x.tgz") {
            MappedAsset::Direct(a) => {
                assert_eq!(a.key, "generations/abc/tarballs/x.tgz");
                assert_eq!(a.bucket_binding, "MYNPM");
            }
            other => panic!("unexpected {other:?}"),
        }
        match map_asset(&c, "/mirror/tarballs/x.tgz") {
            MappedAsset::Direct(a) => assert_eq!(a.key, "tarballs/x.tgz"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn trailing_slash_appends_index() {
        let c = config();
        match map_asset(&c, "/current/simple/proj/") {
            MappedAsset::ViaPointer { rel_path, .. } => {
                assert_eq!(rel_path, "simple/proj/index.html");
            }
            other => panic!("unexpected {other:?}"),
        }
        match map_asset(&c, "/generations/g/simple/") {
            MappedAsset::Direct(a) => assert_eq!(a.key, "generations/g/simple/index.html"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn rejects_escape_and_bare_current() {
        let c = config();
        assert!(matches!(map_asset(&c, "/current"), MappedAsset::NotFound));
        assert!(matches!(map_asset(&c, "/current/%2e%2e/x"), MappedAsset::NotFound));
        assert!(matches!(map_asset(&c, "/current/../../etc/passwd"), MappedAsset::NotFound));
    }

    #[test]
    fn pointer_parsing_is_strict() {
        assert_eq!(parse_pointer(b"d1922c4e2357943e82db9f37"), Some("d1922c4e2357943e82db9f37".to_string()));
        assert_eq!(parse_pointer(b"  abc123  \n"), Some("abc123".to_string()));
        assert_eq!(parse_pointer(b"generations/abc"), None);
        assert_eq!(parse_pointer(b""), None);
        assert_eq!(parse_pointer(b"a/b"), None);
        assert_eq!(parse_pointer(b"\xff\xfe"), None);
        assert_eq!(parse_pointer(&[b'a'; 129]), None);
    }

    #[test]
    fn key_prefix_scopes_objects() {
        let config = WorkerConfig {
            routes: vec![RegistryRoute {
                url_prefix: "pypi".to_string(),
                bucket_binding: "PYPI".to_string(),
                key_prefix: "indexes/main".to_string(),
            }],
        };
        match map_asset(&config, "/pypi/current/simple/") {
            MappedAsset::ViaPointer { bucket_binding, pointer_key, rel_path } => {
                assert_eq!(bucket_binding, "PYPI");
                assert_eq!(pointer_key, "indexes/main/current");
                assert_eq!(rel_path, "simple/index.html");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            object_key_for("indexes/main", "gen9", "simple/index.html"),
            "indexes/main/generations/gen9/simple/index.html"
        );
    }
}
