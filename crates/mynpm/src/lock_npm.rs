#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::inventory::{
    LockfileVersion, Manager, PackageFlags, PackageRecord, PlatformConstraints, ProjectRecord,
    RootDependency, SourceKind,
};
use crate::value;

fn require_package_name(v: &deser_value::Value, label: &str) -> Result<String> {
    let name = value::req_str(v, label)?;
    if !value::valid_package_name(&name) {
        return Err(Error::Input(format!("{label} is not a valid package name")));
    }
    Ok(name)
}

fn require_version(v: Option<&deser_value::Value>, label: &str, nullable: bool) -> Result<Option<String>> {
    let Some(v) = v else {
        if nullable {
            return Ok(None);
        }
        return Err(Error::Input(format!("{label} must be a non-empty string")));
    };
    if value::is_null(v) {
        return Err(Error::Input(format!("{label} must be a non-empty string")));
    }
    let version = value::req_str(v, label)?;
    if !value::valid_version(&version) {
        return Err(Error::Input(format!("{label} is not an exact package version")));
    }
    Ok(Some(version))
}

fn require_integrity(v: Option<&deser_value::Value>, label: &str) -> Result<Option<String>> {
    let Some(v) = v else { return Ok(None) };
    if value::is_null(v) {
        return Err(Error::Input(format!("{label} is malformed")));
    }
    let text = value::req_str(v, label).map_err(|_| Error::Input(format!("{label} is malformed")))?;
    value::check_integrity_string(&text, label)?;
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

fn parse_platform(entry: &deser_value::Map, label: &str) -> Result<PlatformConstraints> {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let mut out = PlatformConstraints { os: None, cpu: None, libc: None };
    for field in ["os", "cpu", "libc"] {
        let v = entry.get(field).unwrap_or(&null);
        if value::is_null(v) {
            continue;
        }
        let mut values: Vec<String> = Vec::new();
        if let Some(seq) = v.as_seq() {
            if seq.is_empty() {
                return Err(Error::Input(format!("{label}.{field} must be a string or non-empty string array")));
            }
            for item in seq.iter() {
                match item.as_str() {
                    Some(s) if !s.is_empty() => values.push(s.to_string()),
                    _ => return Err(Error::Input(format!("{label}.{field} must be a string or non-empty string array"))),
                }
            }
        } else if let Some(s) = v.as_str() {
            if s.is_empty() {
                return Err(Error::Input(format!("{label}.{field} must be a string or non-empty string array")));
            }
            values.push(s.to_string());
        } else {
            return Err(Error::Input(format!("{label}.{field} must be a string or non-empty string array")));
        }
        values.sort();
        values.dedup();
        match field {
            "os" => out.os = Some(values),
            "cpu" => out.cpu = Some(values),
            _ => out.libc = Some(values),
        }
    }
    Ok(out)
}

fn parse_flags(entry: &deser_value::Map, label: &str) -> Result<PackageFlags> {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let mut flags = PackageFlags { dev: false, optional: false, peer: false, peer_optional: false, link: false };
    for field in ["dev", "optional", "peer", "peerOptional", "link"] {
        let v = entry.get(field).unwrap_or(&null);
        if value::is_null(v) {
            continue;
        }
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
    pub snapshot: Option<crate::inventory::ProjectSnapshot>,
}

fn exact_target_ids(ids: Vec<String>) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Err(Error::Input("at least one target ID is required for npm lockfile import".to_string()));
    }
    for id in &ids {
        if id.is_empty() || id.trim() != id {
            return Err(Error::Input("target ID must be a non-empty string".to_string()));
        }
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != ids.len() {
        return Err(Error::Input("target IDs must be unique".to_string()));
    }
    sorted.sort();
    Ok(sorted)
}

pub fn parse_npm_lock(text: &str, options: NpmOptions) -> Result<ProjectRecord> {
    let value: deser_value::Value = deser_json::from_str(text)
        .map_err(|e| Error::Input(format!("npm lockfile is not valid JSON: {e}")))?;
    if value.as_map().is_none() {
        return Err(Error::Input("npm lockfile must be an object".to_string()));
    }
    let root = value::req_map(&value, "npm lockfile")?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let version_num = root
        .get("lockfileVersion")
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok())))
        .ok_or_else(|| Error::Input("npm lockfileVersion must be 2 or 3".to_string()))?;
    if version_num != 2 && version_num != 3 {
        return Err(Error::Input(format!("npm lockfileVersion must be 2 or 3; received {version_num}")));
    }
    if options.project_id.is_empty() || options.project_id.trim() != options.project_id {
        return Err(Error::Input("project ID must be a non-empty string".to_string()));
    }
    if options.lockfile_path.is_empty() {
        return Err(Error::Input("lockfile path must be a non-empty string".to_string()));
    }
    let targets = exact_target_ids(options.target_ids)?;
    let packages_val = root.get("packages").unwrap_or(&null);
    let packages_map = value::req_map(packages_val, "npm lockfile packages")?;
    let root_entry_val = packages_map.get("").ok_or_else(|| Error::Input("npm lockfile root package entry must be an object".to_string()))?;
    let root_entry = value::req_map(root_entry_val, "npm lockfile root package entry")?;

    let mut keys: Vec<String> = Vec::with_capacity(packages_map.len());
    for i in 0..packages_map.len() {
        if let Some((k, _)) = packages_map.get_index(i)
            && let Some(s) = k.as_str() {
                keys.push(s.to_string());
            }
    }
    let workspace_lookup: std::collections::BTreeSet<String> = keys
        .iter()
        .filter(|k| package_name_from_key(k).is_none())
        .cloned()
        .collect();

    let mut records: Vec<PackageRecord> = Vec::new();
    let mut by_key: std::collections::BTreeMap<String, (String, Option<String>)> = std::collections::BTreeMap::new();
    for key in &keys {
        let requested = match package_name_from_key(key) {
            Some(n) => n,
            None => continue,
        };
        let label = format!("npm lockfile packages[{key:?}]");
        let entry_val = packages_map.get(key.as_str()).ok_or_else(|| Error::Input(format!("{label} must be an object")))?;
        let entry = value::req_map(entry_val, &label)?;
        let link_target = if entry.get("link").and_then(|v| v.as_bool()) == Some(true) {
            match entry.get("resolved").and_then(|v| v.as_str()) {
                Some(resolved) => {
                    let ws_key = resolved.strip_prefix("file:").unwrap_or(resolved);
                    if workspace_lookup.contains(ws_key) {
                        packages_map.get(ws_key).and_then(|v| v.as_map())
                    } else {
                        None
                    }
                }
                None => None,
            }
        } else {
            None
        };
        let name_val = entry.get("name").or_else(|| link_target.as_ref().and_then(|m| m.get("name")));
        let name = match name_val {
            Some(v) => require_package_name(v, &format!("{label}.name"))?,
            None => require_package_name(&deser_value::Value::from(requested.clone()), &format!("{label}.name"))?,
        };
        let version_val = entry.get("version").or_else(|| link_target.as_ref().and_then(|m| m.get("version")));
        let is_link = entry.get("link").and_then(|v| v.as_bool()) == Some(true);
        let version = require_version(version_val, &format!("{label}.version"), is_link)?;
        let resolved_raw = match entry.get("resolved") {
            None => None,
            Some(v) if value::is_null(v) => None,
            Some(v) => Some(value::req_str(v, &format!("{label}.resolved"))?),
        };
        let (resolved, had_credentials) = sanitize_resolved(resolved_raw, &format!("{label}.resolved"))?;
        let integrity = require_integrity(entry.get("integrity"), &format!("{label}.integrity"))?;
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
        if value::is_null(deps_val) {
            continue;
        }
        let deps = value::req_map(deps_val, &format!("npm lockfile root {group}"))?;
        for i in 0..deps.len() {
            let Some((k, spec_val)) = deps.get_index(i) else { continue };
            let requested = k.as_str().unwrap_or("").to_string();
            require_package_name(&deser_value::Value::from(requested.clone()), &format!("npm lockfile root {group} package name"))?;
            let specifier = value::req_str(spec_val, &format!("npm lockfile root {group}.{requested}"))?;
            let package_key = format!("node_modules/{requested}");
            let installed = by_key.get(&package_key);
            let target_name = normalize_specifier(&specifier, &requested);
            require_package_name(&deser_value::Value::from(target_name.clone()), &format!("npm lockfile root {group}.{requested} target"))?;
            roots.push(RootDependency {
                importer: ".".to_string(),
                requested_name: requested.clone(),
                specifier,
                package_name: installed.map(|(n, _)| n.clone()).unwrap_or(target_name),
                package_version: installed.and_then(|(_, v)| v.clone()),
                group: group.to_string(),
                package_key: installed.map(|_| package_key),
            });
        }
    }

    records.sort_by_key(|r| r.key.clone());
    roots.sort_by_key(|r| (r.group.clone(), r.requested_name.clone()));
    Ok(ProjectRecord {
        id: options.project_id,
        manager: Manager::Npm,
        package_manager_pin: options.package_manager_pin,
        lockfile_path: options.lockfile_path,
        lockfile_version: LockfileVersion::Npm(version_num),
        targets,
        importers: vec![".".to_string()],
        roots,
        packages: records,
        snapshot: options.snapshot,
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
            snapshot: None,
        }
    }

    fn read_lock(name: &str) -> String {
        std::fs::read_to_string(format!("../../tests/fixtures/mynpm/locks/{name}")).unwrap()
    }

    #[test]
    fn parses_v2_and_v3_fixtures() {
        for name in ["package-lock-v2.json", "package-lock-v3.json", "package-lock-add.json"] {
            let project = parse_npm_lock(&read_lock(name), opts()).unwrap();
            assert_eq!(project.manager, Manager::Npm);
            assert!(!project.packages.is_empty());
            assert_eq!(project.importers, vec![".".to_string()]);
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
