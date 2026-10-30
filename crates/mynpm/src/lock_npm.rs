#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    Registry,
    Local,
    Git,
    Unknown,
    ExternalUrl,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Registry => "registry",
            SourceKind::Local => "local",
            SourceKind::Git => "git",
            SourceKind::Unknown => "unknown",
            SourceKind::ExternalUrl => "external-url",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flags {
    pub dev: bool,
    pub optional: bool,
    pub peer: bool,
    pub peer_optional: bool,
    pub link: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Platform {
    pub os: Vec<String>,
    pub cpu: Vec<String>,
    pub libc: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRecord {
    pub key: String,
    pub name: String,
    pub version: Option<String>,
    pub integrity: Option<String>,
    pub resolved: Option<String>,
    pub source_kind: SourceKind,
    pub aliases: Vec<String>,
    pub flags: Flags,
    pub platform: Platform,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootDependency {
    pub importer: String,
    pub requested_name: String,
    pub specifier: String,
    pub package_name: String,
    pub package_version: Option<String>,
    pub group: String,
    pub package_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecord {
    pub id: String,
    pub manager: String,
    pub package_manager_pin: Option<String>,
    pub lockfile_path: String,
    pub lockfile_version: u64,
    pub targets: Vec<String>,
    pub importers: Vec<String>,
    pub roots: Vec<RootDependency>,
    pub packages: Vec<PackageRecord>,
}

fn is_record(v: &deser_value::Value) -> bool {
    v.as_map().is_some()
}

fn require_record<'a>(v: &'a deser_value::Value, label: &str) -> Result<&'a deser_value::Map> {
    v.as_map()
        .ok_or_else(|| Error::Input(format!("{label} must be an object")))
}

fn require_text(v: &deser_value::Value, label: &str) -> Result<String> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s.trim() == s => Ok(s.to_string()),
        _ => Err(Error::Input(format!("{label} must be a non-empty string"))),
    }
}

fn valid_name_char(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
}

fn valid_name_part(part: &str) -> bool {
    let b = part.as_bytes();
    !part.is_empty()
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|&c| valid_name_char(c))
}

fn require_package_name(v: &deser_value::Value, label: &str) -> Result<String> {
    let name = require_text(v, label)?;
    let ok = if let Some(rest) = name.strip_prefix('@') {
        match rest.find('/') {
            Some(i) => {
                let (scope, pkg) = (&rest[..i], &rest[i + 1..]);
                !scope.is_empty()
                    && !pkg.is_empty()
                    && !pkg.contains('/')
                    && valid_name_part(scope)
                    && valid_name_part(pkg)
            }
            None => false,
        }
    } else {
        !name.contains('/') && valid_name_part(&name)
    };
    if !ok {
        return Err(Error::Input(format!("{label} is not a valid package name")));
    }
    Ok(name)
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s.len() == 1 || !s.starts_with('0'))
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

fn require_version(v: &deser_value::Value, label: &str, nullable: bool) -> Result<Option<String>> {
    if nullable {
        if matches!(&**v, deser_value::Kind::Null) {
            return Ok(None);
        }
        if v.as_str().is_none() {
            return Ok(None);
        }
    }
    let version = require_text(v, label)?;
    let mut rest = version.as_str();
    for _ in 0..2 {
        match rest.find('.') {
            Some(i) => {
                if !is_number(&rest[..i]) {
                    return Err(Error::Input(format!("{label} is not an exact package version")));
                }
                rest = &rest[i + 1..];
            }
            None => return Err(Error::Input(format!("{label} is not an exact package version"))),
        }
    }
    let patch_end = rest.find(['-', '+']).unwrap_or(rest.len());
    if !is_number(&rest[..patch_end]) {
        return Err(Error::Input(format!("{label} is not an exact package version")));
    }
    let mut tail = &rest[patch_end..];
    if let Some(stripped) = tail.strip_prefix('-') {
        let end = stripped.find('+').unwrap_or(stripped.len());
        if !valid_ident(&stripped[..end]) {
            return Err(Error::Input(format!("{label} is not an exact package version")));
        }
        tail = &stripped[end..];
    }
    if let Some(stripped) = tail.strip_prefix('+') {
        if !valid_ident(stripped) {
            return Err(Error::Input(format!("{label} is not an exact package version")));
        }
    } else if !tail.is_empty() {
        return Err(Error::Input(format!("{label} is not an exact package version")));
    }
    Ok(Some(version))
}

fn expected_len(algo: &str) -> Option<usize> {
    match algo {
        "sha1" => Some(20),
        "sha256" => Some(32),
        "sha384" => Some(48),
        "sha512" => Some(64),
        _ => None,
    }
}

fn require_integrity(v: &deser_value::Value, label: &str) -> Result<Option<String>> {
    if matches!(&**v, deser_value::Kind::Null) {
        return Ok(None);
    }
    let text = match v.as_str() {
        Some(s) => s.to_string(),
        None => {
            if v.as_map().is_none() && v.as_seq().is_none() && v.as_bool().is_none() {
                return Ok(None);
            }
            return Err(Error::Input(format!("{label} is malformed")));
        }
    };
    if text.trim().is_empty() {
        return Err(Error::Input(format!("{label} is malformed")));
    }
    let mut claims: BTreeMap<String, String> = BTreeMap::new();
    for token in text.split_whitespace() {
        let claim = token.split('?').next().unwrap_or("");
        let sep = claim.find('-').ok_or_else(|| {
            Error::Input(format!("{label} contains an unsupported integrity token"))
        })?;
        let (algo, digest) = (&claim[..sep], &claim[sep + 1..]);
        let len = expected_len(algo).ok_or_else(|| {
            Error::Input(format!("{label} contains an unsupported integrity token"))
        })?;
        if digest.is_empty()
            || !digest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        {
            return Err(Error::Input(format!(
                "{label} contains an unsupported integrity token"
            )));
        }
        let bytes = B64.decode(digest).map_err(|_| {
            Error::Input(format!("{label} contains a malformed {algo} digest"))
        })?;
        if bytes.len() != len {
            return Err(Error::Input(format!(
                "{label} contains a malformed {algo} digest"
            )));
        }
        if B64.encode(&bytes) != digest {
            return Err(Error::Input(format!(
                "{label} contains a malformed {algo} digest"
            )));
        }
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        if let Some(prev) = claims.get(algo)
            && prev != &hex {
                return Err(Error::Input(format!(
                    "{label} contains conflicting {algo} claims"
                )));
            }
        claims.insert(algo.to_string(), hex);
    }
    Ok(Some(text))
}

fn package_name_from_key(key: &str) -> Option<String> {
    let marker = "node_modules/";
    let offset = key.rfind(marker)?;
    let rest = &key[offset + marker.len()..];
    let mut segments = rest.split('/');
    let first = segments.next()?;
    if let Some(scope) = first.strip_prefix('@') {
        if scope.is_empty() {
            return None;
        }
        let second = segments.next()?;
        if second.is_empty() {
            return None;
        }
        Some(format!("@{scope}/{second}"))
    } else if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

fn sanitize_resolved(value: Option<String>, label: &str) -> Result<(Option<String>, bool)> {
    let Some(text) = value else {
        return Ok((None, false));
    };
    let has_scheme = text
        .find("://")
        .is_some_and(|i| text[..i].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.'));
    if !has_scheme {
        return Ok((Some(text), false));
    }
    let mut url = url::Url::parse(&text)
        .map_err(|e| Error::Input(format!("{label} is not a valid URL: {e}")))?;
    let had = !url.username().is_empty() || url.password().is_some() || url.query().is_some();
    if had {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
    }
    Ok((Some(url.to_string()), had))
}

fn classify_source(resolved: Option<&str>, is_link: bool, had_credentials: bool) -> SourceKind {
    let r = resolved.unwrap_or("");
    let lower = r.to_lowercase();
    if is_link || lower.starts_with("file:") || lower.starts_with("link:") || lower.starts_with("workspace:") {
        return SourceKind::Local;
    }
    if had_credentials {
        return SourceKind::ExternalUrl;
    }
    if lower.starts_with("git+") || lower.starts_with("git:") || lower.starts_with("github:") || lower.starts_with("ssh+git:") {
        return SourceKind::Git;
    }
    if resolved.is_none() {
        return SourceKind::Unknown;
    }
    if lower.starts_with("http:") || lower.starts_with("https:") {
        match url::Url::parse(r) {
            Ok(u) => {
                if u.scheme() == "https" && u.host_str() == Some("registry.npmjs.org") {
                    return SourceKind::Registry;
                }
                return SourceKind::ExternalUrl;
            }
            Err(_) => return SourceKind::ExternalUrl,
        }
    }
    if r.contains("://") {
        return SourceKind::ExternalUrl;
    }
    SourceKind::Local
}

fn parse_platform(entry: &deser_value::Map, label: &str) -> Result<Platform> {
    let mut out = Platform::default();
    for field in ["os", "cpu", "libc"] {
        let Some(v) = entry.get(field) else { continue };
        if matches!(&**v, deser_value::Kind::Null) {
            continue;
        }
        let mut values: Vec<String> = Vec::new();
        if let Some(seq) = v.as_seq() {
            if seq.is_empty() {
                return Err(Error::Input(format!(
                    "{label}.{field} must be a string or non-empty string array"
                )));
            }
            for item in seq.iter() {
                match item.as_str() {
                    Some(s) if !s.is_empty() => values.push(s.to_string()),
                    _ => {
                        return Err(Error::Input(format!(
                            "{label}.{field} must be a string or non-empty string array"
                        )));
                    }
                }
            }
        } else if let Some(s) = v.as_str() {
            if s.is_empty() {
                return Err(Error::Input(format!(
                    "{label}.{field} must be a string or non-empty string array"
                )));
            }
            values.push(s.to_string());
        } else {
            return Err(Error::Input(format!(
                "{label}.{field} must be a string or non-empty string array"
            )));
        }
        values.sort();
        values.dedup();
        match field {
            "os" => out.os = values,
            "cpu" => out.cpu = values,
            _ => out.libc = values,
        }
    }
    Ok(out)
}

fn parse_flags(entry: &deser_value::Map, label: &str) -> Result<Flags> {
    let mut flags = Flags {
        dev: false,
        optional: false,
        peer: false,
        peer_optional: false,
        link: false,
    };
    for field in ["dev", "optional", "peer", "peerOptional", "link"] {
        let Some(v) = entry.get(field) else { continue };
        match v.as_bool() {
            Some(b) => match field {
                "dev" => flags.dev = b,
                "optional" => flags.optional = b,
                "peer" => flags.peer = b,
                "peerOptional" => flags.peer_optional = b,
                _ => flags.link = b,
            },
            None => return Err(Error::Input(format!("{label}.{field} must be a boolean"))),
        }
    }
    Ok(flags)
}

fn normalize_specifier(specifier: &str, requested: &str) -> String {
    if !specifier.starts_with("npm:") {
        return requested.to_string();
    }
    let target = &specifier[4..];
    let split = if let Some(rest) = target.strip_prefix('@') {
        rest.rfind('@').map(|i| i + 1)
    } else {
        target.find('@')
    };
    match split {
        Some(i) if i > 0 => target[..i].to_string(),
        _ => target.to_string(),
    }
}

pub struct NpmOptions {
    pub project_id: String,
    pub package_manager_pin: Option<String>,
    pub lockfile_path: String,
    pub target_ids: Vec<String>,
}

fn exact_target_ids(ids: Vec<String>) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Err(Error::Input(
            "at least one target ID is required for npm lockfile import".to_string(),
        ));
    }
    for id in &ids {
        if id.is_empty() || id.trim() != id {
            return Err(Error::Input("target ID must be a non-empty string".to_string()));
        }
    }
    let set: BTreeSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    if set.len() != ids.len() {
        return Err(Error::Input("target IDs must be unique".to_string()));
    }
    let mut sorted = ids;
    sorted.sort();
    Ok(sorted)
}

pub fn parse_npm_lock(text: &str, options: NpmOptions) -> Result<ProjectRecord> {
    let value: deser_value::Value = deser_json::from_str(text)
        .map_err(|e| Error::Input(format!("npm lockfile is not valid JSON: {e}")))?;
    if !is_record(&value) {
        return Err(Error::Input("npm lockfile must be an object".to_string()));
    }
    let root = require_record(&value, "npm lockfile")?;
    let version_num = root
        .get("lockfileVersion")
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok())))
        .ok_or_else(|| Error::Input("npm lockfileVersion must be 2 or 3".to_string()))?;
    if version_num != 2 && version_num != 3 {
        return Err(Error::Input(format!(
            "npm lockfileVersion must be 2 or 3; received {version_num}"
        )));
    }
    if options.project_id.is_empty() || options.project_id.trim() != options.project_id {
        return Err(Error::Input("project ID must be a non-empty string".to_string()));
    }
    if options.lockfile_path.is_empty() {
        return Err(Error::Input("lockfile path must be a non-empty string".to_string()));
    }
    let targets = exact_target_ids(options.target_ids)?;
    let packages_val = root
        .get("packages")
        .ok_or_else(|| Error::Input("npm lockfile packages must be an object".to_string()))?;
    let packages_map = require_record(packages_val, "npm lockfile packages")?;
    let root_entry_val = packages_map.get("").ok_or_else(|| {
        Error::Input("npm lockfile root package entry must be an object".to_string())
    })?;
    let root_entry = require_record(root_entry_val, "npm lockfile root package entry")?;

    let mut keys: Vec<String> = Vec::with_capacity(packages_map.len());
    for i in 0..packages_map.len() {
        if let Some((k, _)) = packages_map.get_index(i)
            && let Some(s) = k.as_str() {
                keys.push(s.to_string());
            }
    }
    let workspace_lookup: BTreeMap<String, String> = keys
        .iter()
        .filter(|k| package_name_from_key(k).is_none())
        .map(|k| (k.clone(), k.clone()))
        .collect();

    let mut records: Vec<PackageRecord> = Vec::new();
    let mut by_key: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    for key in &keys {
        let requested = match package_name_from_key(key) {
            Some(n) => n,
            None => continue,
        };
        let label = format!("npm lockfile packages[{key:?}]");
        let entry_val = packages_map.get(key.as_str()).ok_or_else(|| {
            Error::Input(format!("{label} must be an object"))
        })?;
        let entry = require_record(entry_val, &label)?;
        let link_target = if entry.get("link").and_then(|v| v.as_bool()) == Some(true) {
            match entry.get("resolved").and_then(|v| v.as_str()) {
                Some(resolved) => {
                    let ws_key = resolved.strip_prefix("file:").unwrap_or(resolved);
                    workspace_lookup.get(ws_key).and_then(|k| {
                        packages_map
                            .get(k.as_str())
                            .and_then(|v| v.as_map())
                    })
                }
                None => None,
            }
        } else {
            None
        };
        let name_val = entry
            .get("name")
            .or_else(|| link_target.as_ref().and_then(|m| m.get("name")));
        let name = match name_val {
            Some(v) => require_package_name(v, &format!("{label}.name"))?,
            None => require_package_name(
                &deser_value::Value::from(requested.clone()),
                &format!("{label}.name"),
            )?,
        };
        let version_val = entry
            .get("version")
            .or_else(|| link_target.as_ref().and_then(|m| m.get("version")));
        let is_link = entry.get("link").and_then(|v| v.as_bool()) == Some(true);
        let version = match version_val {
            Some(v) => require_version(v, &format!("{label}.version"), is_link)?,
            None => require_version(
                &deser_value::Value::from(deser_value::Kind::Null),
                &format!("{label}.version"),
                true,
            )?,
        };
        let resolved_raw = match entry.get("resolved") {
            Some(v) if matches!(&**v, deser_value::Kind::Null) => None,
            Some(v) => Some(require_text(v, &format!("{label}.resolved"))?),
            None => None,
        };
        let (resolved, had_credentials) = sanitize_resolved(resolved_raw, &format!("{label}.resolved"))?;
        let integrity = match entry.get("integrity") {
            Some(v) => require_integrity(v, &format!("{label}.integrity"))?,
            None => None,
        };
        if let Some(link) = entry.get("link")
            && link.as_bool().is_none() {
                return Err(Error::Input(format!("{label}.link must be a boolean")));
            }
        let aliases = if name == requested { vec![] } else { vec![requested.clone()] };
        records.push(PackageRecord {
            key: key.clone(),
            name: name.clone(),
            version: version.clone(),
            integrity,
            resolved: resolved.clone(),
            source_kind: classify_source(resolved.as_deref(), is_link, had_credentials),
            aliases,
            flags: parse_flags(entry, &label)?,
            platform: parse_platform(entry, &label)?,
            targets: targets.clone(),
        });
        by_key.insert(key.clone(), (name, version));
    }

    let groups = ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"];
    let mut roots: Vec<RootDependency> = Vec::new();
    for group in groups {
        let Some(deps_val) = root_entry.get(group) else { continue };
        let deps = require_record(deps_val, &format!("npm lockfile root {group}"))?;
        for i in 0..deps.len() {
            let Some((k, spec_val)) = deps.get_index(i) else { continue };
            let requested = k.as_str().unwrap_or("").to_string();
            require_package_name(
                &deser_value::Value::from(requested.clone()),
                &format!("npm lockfile root {group} package name"),
            )?;
            let specifier = require_text(
                spec_val,
                &format!("npm lockfile root {group}.{requested}"),
            )?;
            let package_key = format!("node_modules/{requested}");
            let installed = by_key.get(&package_key);
            let target_name = normalize_specifier(&specifier, &requested);
            require_package_name(
                &deser_value::Value::from(target_name.clone()),
                &format!("npm lockfile root {group}.{requested} target"),
            )?;
            roots.push(RootDependency {
                importer: ".".to_string(),
                requested_name: requested.clone(),
                specifier,
                package_name: installed
                    .map(|(n, _)| n.clone())
                    .unwrap_or(target_name),
                package_version: installed.and_then(|(_, v)| v.clone()),
                group: group.to_string(),
                package_key: installed.map(|_| package_key),
            });
        }
    }

    records.sort_by(|a, b| a.key.cmp(&b.key));
    roots.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.requested_name.cmp(&b.requested_name))
    });
    Ok(ProjectRecord {
        id: options.project_id,
        manager: "npm".to_string(),
        package_manager_pin: options.package_manager_pin,
        lockfile_path: options.lockfile_path,
        lockfile_version: version_num,
        targets,
        importers: vec![".".to_string()],
        roots,
        packages: records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> NpmOptions {
        NpmOptions {
            project_id: "demo".to_string(),
            package_manager_pin: None,
            lockfile_path: "package-lock.json".to_string(),
            target_ids: vec!["linux-x64-gnu".to_string()],
        }
    }

    fn read_lock(name: &str) -> String {
        std::fs::read_to_string(format!("../../tools/mynpm/test/fixtures/locks/{name}")).unwrap()
    }

    #[test]
    fn parses_v2_and_v3_fixtures() {
        for name in ["package-lock-v2.json", "package-lock-v3.json", "package-lock-add.json"] {
            let project = parse_npm_lock(&read_lock(name), opts()).unwrap();
            assert_eq!(project.manager, "npm");
            assert!(!project.packages.is_empty());
            assert_eq!(project.importers, vec![".".to_string()]);
            for pkg in &project.packages {
                assert!(!pkg.key.is_empty());
                assert!(!pkg.name.is_empty());
            }
        }
    }

    #[test]
    fn rejects_bad_version_and_duplicate_targets() {
        assert!(parse_npm_lock("{}", opts()).is_err());
        let bad = NpmOptions {
            target_ids: vec!["a".to_string(), "a".to_string()],
            ..opts()
        };
        assert!(parse_npm_lock(&read_lock("package-lock-v2.json"), bad).is_err());
    }
}
