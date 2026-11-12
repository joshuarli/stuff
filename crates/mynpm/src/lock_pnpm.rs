#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::inventory::{
    LockfileVersion, Manager, PackageFlags, PackageRecord, PlatformConstraints, ProjectRecord,
    ProjectSnapshot, RootDependency, SourceKind,
};
use crate::value;

fn invalid(message: impl Into<String>) -> Error {
    Error::Input(message.into())
}

fn require_text(v: &deser_value::Value, label: &str) -> Result<String> {
    value::req_str(v, label)
}

fn require_package_name(v: &deser_value::Value, label: &str) -> Result<String> {
    let name = require_text(v, label)?;
    if !value::valid_package_name(&name) {
        return Err(invalid(format!("{label} is not a valid package name")));
    }
    Ok(name)
}

fn parse_integrity(v: Option<&deser_value::Value>, label: &str) -> Result<Option<String>> {
    let Some(v) = v else { return Ok(None) };
    if value::is_null(v) {
        return Err(invalid(format!("{label} must be a non-empty string")));
    }
    let text = require_text(v, label)?;
    value::check_integrity_string(&text, label)?;
    Ok(Some(text))
}

fn package_identity(key: &str, label: &str) -> Result<(String, Option<String>, String)> {
    require_text(&deser_value::Value::from(key.to_string()), label)?;
    let normalized = key.strip_prefix('/').unwrap_or(key);
    let slash = if normalized.starts_with('@') {
        normalized.find('/')
    } else {
        None
    };
    let from = slash.map(|i| i + 1).unwrap_or(0);
    let separator = normalized[from..].find('@').map(|i| i + from);
    let Some(sep) = separator else {
        return Err(invalid(format!("{label} has no package reference")));
    };
    if sep == 0 || sep == normalized.len() - 1 {
        return Err(invalid(format!("{label} has no package reference")));
    }
    let name = require_package_name(&deser_value::Value::from(normalized[..sep].to_string()), &format!("{label} package name"))?;
    let reference = normalized[sep + 1..].to_string();
    let version = reference.split('(').next().unwrap_or("");
    let version = if value::valid_version(version) {
        Some(version.to_string())
    } else {
        None
    };
    Ok((name, version, reference))
}

fn sanitize_url(v: Option<&deser_value::Value>, label: &str) -> Result<(Option<String>, bool)> {
    let Some(v) = v else { return Ok((None, false)) };
    if value::is_null(v) {
        return Err(invalid(format!("{label} must be a non-empty string")));
    }
    let source = require_text(v, label)?;
    let mut url = stuff_url::Url::parse(&source)
        .map_err(|_| invalid(format!("{label} is not a valid absolute URL")))?;
    if url.host_str().is_none() {
        return Err(invalid(format!("{label} is not a valid absolute URL")));
    }
    let had = !url.username().is_empty() || url.password().is_some() || url.query().is_some();
    if had {
        url.clear_credentials();
        url.clear_query();
    }
    url.clear_fragment();
    Ok((Some(url.to_string()), had))
}

fn source_kind(resolution: &deser_value::Map, reference: &str, url: Option<&str>, had_credentials: bool) -> SourceKind {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let res_type = resolution.get("type").unwrap_or(&null).as_str().unwrap_or("");
    let has_directory = resolution.get("directory").is_some_and(|v| !value::is_null(v));
    let has_commit = resolution.get("commit").is_some_and(|v| !value::is_null(v));
    let lower_ref = reference.to_lowercase();
    if res_type == "directory"
        || has_directory
        || lower_ref.starts_with("file:")
        || lower_ref.starts_with("link:")
        || lower_ref.starts_with("workspace:")
    {
        return SourceKind::Local;
    }
    if had_credentials {
        return SourceKind::ExternalUrl;
    }
    if res_type == "git"
        || has_commit
        || lower_ref.starts_with("git+")
        || lower_ref.starts_with("git:")
        || lower_ref.starts_with("github:")
        || lower_ref.starts_with("ssh+git:")
    {
        return SourceKind::Git;
    }
    let Some(url) = url else {
        return SourceKind::Registry;
    };
    if url.to_lowercase().starts_with("file:") {
        return SourceKind::Local;
    }
    match stuff_url::Url::parse(url) {
        Ok(parsed) => {
            if parsed.scheme() == "https" && parsed.host_str() == Some("registry.npmjs.org") {
                SourceKind::Registry
            } else {
                SourceKind::ExternalUrl
            }
        }
        Err(_) => SourceKind::ExternalUrl,
    }
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
                return Err(invalid(format!("{label}.{field} must be a string or non-empty string array")));
            }
            for item in seq.iter() {
                match item.as_str() {
                    Some(s) if !s.is_empty() => values.push(s.to_string()),
                    _ => return Err(invalid(format!("{label}.{field} must be a string or non-empty string array"))),
                }
            }
        } else if let Some(s) = v.as_str() {
            if s.is_empty() {
                return Err(invalid(format!("{label}.{field} must be a string or non-empty string array")));
            }
            values.push(s.to_string());
        } else {
            return Err(invalid(format!("{label}.{field} must be a string or non-empty string array")));
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

fn parse_flags(entry: &deser_value::Map, resolution: &deser_value::Map, label: &str) -> Result<PackageFlags> {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let mut flags = PackageFlags { dev: false, optional: false, peer: false, peer_optional: false, link: false };
    for field in ["dev", "optional", "peer", "peerOptional"] {
        let v = entry.get(field).unwrap_or(&null);
        if value::is_null(v) {
            continue;
        }
        match v.as_bool() {
            Some(b) => match field {
                "dev" => flags.dev = b,
                "optional" => flags.optional = b,
                "peer" => flags.peer = b,
                _ => flags.peer_optional = b,
            },
            None => return Err(invalid(format!("{label}.{field} must be a boolean"))),
        }
    }
    flags.link = resolution.get("type").and_then(|v| v.as_str()) == Some("directory");
    Ok(flags)
}

fn normalize_targets(target_ids: Vec<String>) -> Result<Vec<String>> {
    if target_ids.is_empty() {
        return Err(invalid("at least one target ID is required for pnpm lockfile import"));
    }
    for (i, id) in target_ids.iter().enumerate() {
        if id.is_empty() || id.trim() != id {
            return Err(invalid(format!("targetIds[{i}] must be a non-empty string")));
        }
    }
    let mut sorted = target_ids.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != target_ids.len() {
        return Err(invalid("target IDs must be unique"));
    }
    sorted.sort();
    Ok(sorted)
}

fn normalize_importer_path(v: &deser_value::Value, label: &str) -> Result<String> {
    let path = require_text(v, label)?;
    if path != "."
        && (path.starts_with('/') || path.contains('\\') || path.split('/').any(|p| p.is_empty() || p == "." || p == ".."))
    {
        return Err(invalid(format!("{label} must be a safe project-relative path")));
    }
    Ok(path)
}

fn snapshot_reference(key: &str) -> String {
    key.strip_prefix('/').unwrap_or(key).split('(').next().unwrap_or("").to_string()
}

fn snapshot_dependency_reference(name: &str, reference: &str) -> Option<String> {
    let normalized = reference.strip_prefix("npm:").unwrap_or(reference);
    if normalized.to_lowercase().starts_with("link:")
        || normalized.to_lowercase().starts_with("file:")
        || normalized.to_lowercase().starts_with("workspace:")
    {
        return None;
    }
    let package_separator = normalized.find('@');
    let peer_suffix = normalized.find('(');
    if package_separator.is_some_and(|p| peer_suffix.is_none_or(|s| p < s)) {
        Some(normalized.strip_prefix('/').unwrap_or(normalized).to_string())
    } else {
        Some(format!("{name}@{normalized}"))
    }
}

fn package_reference_matches(reference: &str, requested_name: &str, record: &PackageRecord) -> bool {
    let package_key = record.key.strip_prefix('/').unwrap_or(&record.key);
    let candidate = reference.strip_prefix("npm:").unwrap_or(reference);
    if candidate == package_key || candidate == format!("/{package_key}") {
        return true;
    }
    let Ok((identity_name, identity_version, identity_reference)) = package_identity(&record.key, "pnpm package key") else {
        return false;
    };
    if candidate.contains('@')
        && let Ok((ref_name, _, ref_reference)) = package_identity(candidate, "pnpm importer package reference")
        && ref_name == identity_name
        && ref_reference.split('(').next().unwrap_or("")
            == identity_reference.split('(').next().unwrap_or("")
    {
        return true;
    }
    let candidate_base = candidate.split('(').next().unwrap_or("");
    let identity_base = identity_reference.split('(').next().unwrap_or("");
    if identity_name == requested_name && candidate_base == identity_base {
        return true;
    }
    identity_version.is_some_and(|v| candidate_base == v) && identity_name == requested_name
}

fn record_canonical(record: &PackageRecord) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{}",
        record.key,
        record.name,
        record.version.clone().unwrap_or_default(),
        record.integrity.clone().unwrap_or_default(),
        record.resolved.clone().unwrap_or_default(),
        record.source_kind.as_str(),
        record.aliases,
        (record.flags.dev, record.flags.optional, record.flags.peer, record.flags.peer_optional, record.flags.link),
        (record.platform.os.clone().unwrap_or_default(), record.platform.cpu.clone().unwrap_or_default(), record.platform.libc.clone().unwrap_or_default()),
        record.targets.join(","),
    )
}

fn parse_package_entry(key: &str, entry_value: &deser_value::Value, targets: &[String]) -> Result<PackageRecord> {
    let label = format!("pnpm lockfile packages[{key:?}]");
    let (name_ident, version_ident, reference) = package_identity(key, &label)?;
    let entry = value::req_map(entry_value, &label)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let resolution_val = entry.get("resolution").unwrap_or(&null);
    let resolution = value::req_map(resolution_val, &format!("{label}.resolution"))?;
    if let Some(rev) = resolution.get("revision") {
        if value::is_null(rev) {
            // fall through: null revision treated as absent? TS: revision !== undefined check; null is an object → typeof null !== 'number' → invalid
            return Err(invalid(format!("{label}.resolution.revision must be a non-negative integer")));
        }
        let n: Option<i64> = rev.as_u64().map(|n| n as i64).or_else(|| rev.as_i64());
        match n {
            Some(n) if n >= 0 => {
                if n != 0 {
                    return Err(invalid(format!("{label} uses unsupported resolution revision {n}")));
                }
            }
            _ => return Err(invalid(format!("{label}.resolution.revision must be a non-negative integer"))),
        }
    }
    let integrity = parse_integrity(resolution.get("integrity"), &format!("{label}.resolution.integrity"))?;
    let (url, had_credentials) = sanitize_url(resolution.get("tarball"), &format!("{label}.resolution.tarball"))?;
    let name = match entry.get("name") {
        Some(nv) if !value::is_null(nv) => require_package_name(nv, &format!("{label}.name"))?,
        _ => name_ident.clone(),
    };
    let version = match entry.get("version") {
        Some(vv) if !value::is_null(vv) => {
            let text = require_text(vv, &format!("{label}.version"))?;
            if !value::valid_version(&text) {
                return Err(invalid(format!("{label}.version is not an exact package version")));
            }
            Some(text)
        }
        Some(_) => None,
        None => version_ident,
    };
    if !value::valid_package_name(&name) {
        return Err(invalid(format!("{label}.name is not a valid package name")));
    }
    let source = source_kind(resolution, &reference, url.as_deref(), had_credentials);
    let aliases = if name == name_ident { vec![] } else { vec![name_ident] };
    Ok(PackageRecord {
        key: key.to_string(),
        name,
        version,
        integrity,
        resolved: url,
        source_kind: source,
        aliases,
        flags: parse_flags(entry, resolution, &label)?,
        platform: parse_platform(entry, &label)?,
        targets: targets.to_vec(),
    })
}

fn root_canonical(root: &RootDependency) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        root.importer,
        root.requested_name,
        root.specifier,
        root.package_name,
        root.package_version.clone().unwrap_or_default(),
        root.group,
        root.package_key.clone().unwrap_or_default(),
    )
}

fn importer_roots(
    document: &deser_value::Map,
    packages: &[PackageRecord],
    importer_label: &str,
) -> Result<Vec<RootDependency>> {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let importers_val = document.get("importers").unwrap_or(&null);
    let importers = if value::is_null(importers_val) {
        return Ok(Vec::new());
    } else {
        value::req_map(importers_val, &format!("{importer_label}.importers"))?
    };
    let mut roots = Vec::new();
    for i in 0..importers.len() {
        let Some((path_val, importer_val)) = importers.get_index(i) else { continue };
        let path_str = path_val.as_str().unwrap_or("").to_string();
        let owned_path = deser_value::Value::from(path_str.clone());
        normalize_importer_path(&owned_path, &format!("{importer_label}.importers path"))?;
        let importer = value::req_map(importer_val, &format!("{importer_label}.importers[{path_str:?}]"))?;
        for group in ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"] {
            let Some(deps_val) = importer.get(group) else { continue };
            if value::is_null(deps_val) {
                continue;
            }
            let deps = value::req_map(deps_val, &format!("{importer_label}.importers[{path_str:?}].{group}"))?;
            for j in 0..deps.len() {
                let Some((req_val, entry_val)) = deps.get_index(j) else { continue };
                let requested = req_val.as_str().unwrap_or("").to_string();
                require_package_name(&deser_value::Value::from(requested.clone()), &format!("{importer_label}.importers dependency name"))?;
                let (specifier, reference) = if let Some(s) = entry_val.as_str() {
                    (s.to_string(), s.to_string())
                } else {
                    let dep = value::req_map(entry_val, &format!("{importer_label}.importers dependency"))?;
                    let spec = require_text(dep.get("specifier").unwrap_or(&null), &format!("{importer_label}.importers dependency specifier"))?;
                    let refr = require_text(dep.get("version").unwrap_or(&null), &format!("{importer_label}.importers dependency version"))?;
                    (spec, refr)
                };
                let matched = packages.iter().find(|pkg| package_reference_matches(&reference, &requested, pkg));
                let (package_name, package_version, package_key) = match matched {
                    Some(m) => (m.name.clone(), m.version.clone(), Some(m.key.clone())),
                    None => {
                        let lower = reference.to_lowercase();
                        if lower.starts_with("link:") || lower.starts_with("file:") || lower.starts_with("workspace:") {
                            (requested.clone(), None, None)
                        } else {
                            let target = reference.strip_prefix("npm:").unwrap_or(&reference);
                            let target_base = target.split('(').next().unwrap_or("");
                            let (at_pos, search_from) = if let Some(rest) = target_base.strip_prefix('@') {
                                match rest.find('@') {
                                    Some(i) => (Some(i + 1), true),
                                    None => (None, false),
                                }
                            } else {
                                (target_base.find('@'), false)
                            };
                            let _ = search_from;
                            match at_pos {
                                Some(sep) if sep > 0 => {
                                    let nm = require_package_name(
                                        &deser_value::Value::from(target_base[..sep].to_string()),
                                        &format!("{importer_label} dependency target"),
                                    )?;
                                    let tv = &target_base[sep + 1..];
                                    (nm, if value::valid_version(tv) { Some(tv.to_string()) } else { None }, None)
                                }
                                _ => {
                                    if value::valid_version(target_base) {
                                        (requested.clone(), Some(target_base.to_string()), None)
                                    } else {
                                        (requested.clone(), None, None)
                                    }
                                }
                            }
                        }
                    }
                };
                roots.push(RootDependency {
                    importer: path_str.clone(),
                    requested_name: requested,
                    specifier,
                    package_name,
                    package_version,
                    group: group.to_string(),
                    package_key,
                });
            }
        }
    }
    Ok(roots)
}

pub struct PnpmOptions {
    pub project_id: String,
    pub package_manager_pin: Option<String>,
    pub lockfile_path: String,
    pub target_ids: Vec<String>,
    pub snapshot: Option<ProjectSnapshot>,
}

fn lockfile_documents(values: &[deser_value::Value]) -> Result<Vec<&deser_value::Map>> {
    if values.is_empty() {
        return Err(invalid("pnpm lockfile must contain at least one YAML document"));
    }
    let mut docs = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        docs.push(value::req_map(value, &format!("pnpm lockfile document {}", index + 1)).map_err(|_| {
            invalid(format!("pnpm lockfile document {} must be an object", index + 1))
        })?);
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let version = docs[0].get("lockfileVersion").unwrap_or(&null).as_str().unwrap_or("").to_string();
    if version != "9.0" {
        return Err(invalid(format!("pnpm lockfileVersion must be '9.0'; received {version}")));
    }
    for (index, document) in docs.iter().enumerate() {
        let n = index + 1;
        if document.get("lockfileVersion").unwrap_or(&null).as_str().unwrap_or("") != version {
            return Err(invalid(format!("pnpm lockfile document {n} has a different lockfileVersion")));
        }
        for field in ["packages", "snapshots", "importers"] {
            let fv = document.get(field).unwrap_or(&null);
            if !value::is_null(fv) {
                value::req_map(fv, &format!("pnpm lockfile document {n}.{field}")).map_err(|_| {
                    invalid(format!("pnpm lockfile document {n}.{field} must be an object"))
                })?;
            }
        }
    }
    Ok(docs)
}

fn parse_pnpm_documents(
    values: Vec<deser_value::Value>,
    options: PnpmOptions,
) -> Result<ProjectRecord> {
    let documents = lockfile_documents(&values)?;
    let targets = normalize_targets(options.target_ids)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let mut packages_by_key: Vec<PackageRecord> = Vec::new();
    let mut snapshot_keys: rustc_hash::FxHashSet<String> = Default::default();
    let mut snapshot_entries: rustc_hash::FxHashSet<String> = Default::default();
    let mut snapshot_references: rustc_hash::FxHashSet<String> = Default::default();
    let mut importer_paths: rustc_hash::FxHashSet<String> = Default::default();
    let mut roots: Vec<RootDependency> = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let n = index + 1;
        let doc_packages_val = document.get("packages").unwrap_or(&null);
        let doc_packages = if value::is_null(doc_packages_val) {
            None
        } else {
            Some(value::req_map(doc_packages_val, &format!("pnpm lockfile document {n}.packages")).map_err(|_| {
                invalid(format!("pnpm lockfile document {n}.packages must be an object"))
            })?)
        };
        if let Some(pkgs) = doc_packages {
            for i in 0..pkgs.len() {
                let Some((k, entry)) = pkgs.get_index(i) else { continue };
                let key = k.as_str().unwrap_or("").to_string();
                let parsed = parse_package_entry(&key, entry, &targets)?;
                if let Some(prior) = packages_by_key.iter().find(|p| p.key == key) {
                    if record_canonical(prior) != record_canonical(&parsed) {
                        return Err(Error::Conflict(format!("pnpm lockfile documents disagree about package {key}")));
                    }
                } else {
                    packages_by_key.push(parsed);
                }
            }
        }
        let snapshots_val = document.get("snapshots").unwrap_or(&null);
        let snapshots = if value::is_null(snapshots_val) {
            None
        } else {
            Some(value::req_map(snapshots_val, &format!("pnpm lockfile document {n}.snapshots")).map_err(|_| {
                invalid(format!("pnpm lockfile document {n}.snapshots must be an object"))
            })?)
        };
        let importers_val = document.get("importers").unwrap_or(&null);
        let importers = if value::is_null(importers_val) {
            None
        } else {
            Some(value::req_map(importers_val, &format!("pnpm lockfile document {n}.importers")).map_err(|_| {
                invalid(format!("pnpm lockfile document {n}.importers must be an object"))
            })?)
        };
        if let Some(imps) = importers {
            for i in 0..imps.len() {
                if let Some((path_val, _)) = imps.get_index(i) {
                    let owned = path_val.clone();
                    importer_paths.insert(normalize_importer_path(&owned, &format!("pnpm lockfile document {n}.importers path"))?);
                }
            }
        }
        if let Some(snaps) = snapshots {
            for i in 0..snaps.len() {
                let Some((k, snapshot_value)) = snaps.get_index(i) else { continue };
                let key = k.as_str().unwrap_or("").to_string();
                let snapshot_entry = value::req_map(snapshot_value, &format!("pnpm lockfile snapshots[{key:?}]"))?;
                for group in ["dependencies", "optionalDependencies"] {
                    let Some(deps_val) = snapshot_entry.get(group) else { continue };
                    if value::is_null(deps_val) {
                        continue;
                    }
                    let dependencies = value::req_map(deps_val, &format!("pnpm lockfile snapshots[{key:?}].{group}"))?;
                    for j in 0..dependencies.len() {
                        let Some((name_val, reference_val)) = dependencies.get_index(j) else { continue };
                        let dep_name = name_val.as_str().unwrap_or("").to_string();
                        require_package_name(
                            &deser_value::Value::from(dep_name.clone()),
                            &format!("pnpm lockfile snapshots[{key:?}].{group} dependency name"),
                        )?;
                        let reference = require_text(reference_val, &format!("pnpm lockfile snapshots[{key:?}].{group}[{dep_name:?}]"))?;
                        if let Some(normalized) = snapshot_dependency_reference(&dep_name, &reference) {
                            snapshot_references.insert(normalized);
                        }
                    }
                }
                snapshot_keys.insert(snapshot_reference(&key));
                snapshot_entries.insert(key.strip_prefix('/').unwrap_or(&key).to_string());
            }
        }
        roots.extend(importer_roots(document, &packages_by_key, &format!("pnpm lockfile document {n}"))?);
    }
    if let Some(snapshot) = snapshot_keys
        .iter()
        .filter(|snapshot| !packages_by_key.iter().any(|pkg| snapshot_reference(&pkg.key) == **snapshot))
        .min()
    {
        return Err(invalid(format!("pnpm snapshot has no matching package entry: {snapshot}")));
    }
    if let Some(missing) = snapshot_references.iter().filter(|r| !snapshot_entries.contains(*r)).min() {
        return Err(invalid(format!("pnpm snapshot dependency has no matching snapshot: {missing}")));
    }
    let mut seen_roots: rustc_hash::FxHashSet<String> = Default::default();
    let mut unique_roots = Vec::new();
    for root in roots {
        if seen_roots.insert(root_canonical(&root)) {
            unique_roots.push(root);
        }
    }
    unique_roots.sort_by_key(|r| (r.importer.clone(), r.group.clone(), r.requested_name.clone()));
    packages_by_key.sort_by_key(|p| p.key.clone());
    let mut importer_paths: Vec<String> = importer_paths.into_iter().collect();
    importer_paths.sort();
    if options.project_id.is_empty() || options.project_id.trim() != options.project_id {
        return Err(invalid("project ID must be a non-empty string"));
    }
    if options.lockfile_path.is_empty() {
        return Err(invalid("lockfile path must be a non-empty string"));
    }
    Ok(ProjectRecord {
        id: options.project_id,
        manager: Manager::Pnpm,
        package_manager_pin: options.package_manager_pin,
        lockfile_path: options.lockfile_path,
        lockfile_version: LockfileVersion::Pnpm("9.0".to_string()),
        targets,
        importers: importer_paths,
        roots: unique_roots,
        packages: packages_by_key,
        snapshot: options.snapshot,
    })
}

pub fn decode_pnpm_documents(text: &str) -> Result<Vec<deser_value::Value>> {
    let mut de = deser_yaml::Deserializer::from_str(text);
    let mut docs = Vec::new();
    loop {
        if de.is_end() {
            break;
        }
        match de.deserialize::<deser_value::Value>() {
            Ok(doc) => {
                if value::is_null(&doc) {
                    return Err(invalid(format!("pnpm lockfile document {} is empty", docs.len() + 1)));
                }
                docs.push(doc);
            }
            Err(e) => {
                return Err(invalid(format!("pnpm lockfile is not valid YAML: {e}")));
            }
        }
    }
    if docs.is_empty() {
        return Err(invalid("pnpm lockfile must contain at least one YAML document"));
    }
    Ok(docs)
}

pub fn parse_pnpm_lock(text: &str, options: PnpmOptions) -> Result<ProjectRecord> {
    let documents = decode_pnpm_documents(text)?;
    parse_pnpm_documents(documents, options)
}

pub fn pnpm_build_package_keys(text: &str) -> Result<Vec<String>> {
    let documents = decode_pnpm_documents(text)?;
    let maps = lockfile_documents(&documents)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let mut keys: rustc_hash::FxHashSet<String> = Default::default();
    for (index, document) in maps.iter().enumerate() {
        let n = index + 1;
        let packages_val = document.get("packages").unwrap_or(&null);
        if value::is_null(packages_val) {
            continue;
        }
        let packages = value::req_map(packages_val, &format!("pnpm lockfile document {n}.packages")).map_err(|_| {
            invalid(format!("pnpm lockfile document {n}.packages must be an object"))
        })?;
        for i in 0..packages.len() {
            let Some((k, entry_value)) = packages.get_index(i) else { continue };
            let key = k.as_str().unwrap_or("").to_string();
            let entry = value::req_map(entry_value, &format!("pnpm lockfile document {n}.packages[{key:?}]"))?;
            if entry.get("requiresBuild").and_then(|v| v.as_bool()) == Some(true) {
                keys.insert(key);
            }
        }
    }
    let mut keys: Vec<String> = keys.into_iter().collect();
    keys.sort_unstable();
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> PnpmOptions {
        PnpmOptions {
            project_id: "demo".to_string(),
            package_manager_pin: None,
            lockfile_path: "pnpm-lock.yaml".to_string(),
            target_ids: vec!["linux-x64-gnu".to_string()],
            snapshot: None,
        }
    }

    fn read_lock(name: &str) -> String {
        std::fs::read_to_string(format!("../../tests/fixtures/mynpm/locks/{name}")).unwrap()
    }

    #[test]
    fn parses_pnpm_fixtures() {
        for name in ["pnpm-v9-local.yaml", "pnpm-v12-peer.yaml", "pnpm-v12-two-documents.yaml"] {
            let project = parse_pnpm_lock(&read_lock(name), opts()).unwrap();
            assert_eq!(project.manager, Manager::Pnpm);
            assert!(!project.packages.is_empty(), "{name}");
            assert!(!project.roots.is_empty(), "{name}");
        }
    }

    #[test]
    fn rejects_bad_lockfile_version_and_duplicates() {
        assert!(parse_pnpm_lock("lockfileVersion: '8.0'\n", opts()).is_err());
        let dup = "lockfileVersion: '9.0'\na: 1\na: 2\n";
        let err = parse_pnpm_lock(dup, opts()).unwrap_err();
        assert!(format!("{err}").contains("YAML") || format!("{err}").contains("uplicate"), "{err}");
        let bad = PnpmOptions { target_ids: vec!["a".to_string(), "a".to_string()], ..opts() };
        assert!(parse_pnpm_lock(&read_lock("pnpm-v9-local.yaml"), bad).is_err());
    }

    #[test]
    fn build_keys_fixture() {
        let keys = pnpm_build_package_keys(&read_lock("pnpm-v12-peer.yaml")).unwrap();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]));
    }
    const MIRROR_LOCK: &str = "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n      '@vitest/ui':\n        specifier: 5.0.5\n        version: 5.0.5\n      other:\n        specifier: 1.0.0\n        version: 1.0.0\n      foreign:\n        specifier: 1.0.0\n        version: 1.0.0\n\npackages:\n\n  '@vitest/ui@5.0.5':\n    resolution: {integrity: sha512-dCoGUOdgmCaSB2g+WQ/spPCBkytAWdNUkFSXY4z+eYp2d8JEOY8v0uDRaJTG9qf/kTlTVZ4CuYMYMz/f1HS/qQ==, tarball: https://npm.example/current/-/tarballs/742a0650e76098269207683e590feca4f081932b4059d354905497638cfe798a7677c244398f2fd2e0d16894c6f6a7ff913953559e02b98318333fdfd474bfa9.tgz}\n\n  other@1.0.0:\n    resolution: {integrity: sha512-dCoGUOdgmCaSB2g+WQ/spPCBkytAWdNUkFSXY4z+eYp2d8JEOY8v0uDRaJTG9qf/kTlTVZ4CuYMYMz/f1HS/qQ==, tarball: https://npm.example/current/-/tarballs/00000650e76098269207683e590feca4f081932b4059d354905497638cfe798a7677c244398f2fd2e0d16894c6f6a7ff913953559e02b98318333fdfd474bfa9.tgz}\n\n  foreign@1.0.0:\n    resolution: {integrity: sha512-dCoGUOdgmCaSB2g+WQ/spPCBkytAWdNUkFSXY4z+eYp2d8JEOY8v0uDRaJTG9qf/kTlTVZ4CuYMYMz/f1HS/qQ==, tarball: https://npm.example.evil/current/-/tarballs/742a0650e76098269207683e590feca4f081932b4059d354905497638cfe798a7677c244398f2fd2e0d16894c6f6a7ff913953559e02b98318333fdfd474bfa9.tgz}\n";

    #[test]
    fn mirror_tarballs_matching_their_integrity_are_registry_sources() {
        let mut project = parse_pnpm_lock(MIRROR_LOCK, opts()).unwrap();
        crate::inventory::adopt_mirror_tarballs(&mut project, "https://npm.example");
        let kind = |name: &str| {
            let record = project.packages.iter().find(|p| p.name == name).unwrap();
            (record.source_kind.clone(), record.resolved.is_some())
        };
        assert_eq!(kind("@vitest/ui"), (SourceKind::Registry, false));
        assert_eq!(kind("other"), (SourceKind::ExternalUrl, true));
        assert_eq!(kind("foreign"), (SourceKind::ExternalUrl, true));
    }
}
