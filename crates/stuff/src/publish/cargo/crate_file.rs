use std::collections::HashSet;
use std::io::Read;

use deser_value::Value;
use sha2::{Digest, Sha256};

use super::model::{DependencyKind, Features};
use super::requirements::canonical_requirement;
use super::{integrity, limit, Rejection};

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_compressed: u64,
    pub max_expanded: u64,
    pub max_entries: usize,
    pub max_ratio: u64,
    pub max_manifest_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_compressed: 1024 * 1024 * 1024,
            max_expanded: 4 * 1024 * 1024 * 1024,
            max_entries: 200_000,
            max_ratio: 1000,
            max_manifest_bytes: MAX_MANIFEST_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanResult {
    pub checksum: String,
    pub cargo_toml: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackagedDep {
    pub alias: String,
    pub package: Option<String>,
    pub version_req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: Option<DependencyKind>,
    pub registry: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackagedManifest {
    pub name: String,
    pub version: String,
    pub links: Option<String>,
    pub rust_version: Option<String>,
    pub features: Features,
    pub dependencies: Vec<PackagedDep>,
}

pub fn scan(
    archive: &[u8],
    name: &str,
    version: &str,
    limits: &Limits,
) -> Result<ScanResult, Rejection> {
    if archive.len() as u128 > u128::from(limits.max_compressed) {
        return Err(limit("compressed size exceeded"));
    }
    let checksum = sha256_hex(archive);
    let prefix = format!("{name}-{version}/");
    let manifest_path = format!("{prefix}Cargo.toml");
    let decoder = flate2::read::MultiGzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let entries = tar
        .entries()
        .map_err(|error| integrity(error.to_string()))?;
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut entries_seen: usize = 0;
    let mut expanded: u128 = 0;
    let mut cargo_toml: Option<String> = None;
    for entry in entries {
        let mut entry = entry.map_err(|error| integrity(error.to_string()))?;
        entries_seen += 1;
        if entries_seen > limits.max_entries {
            return Err(limit("too many entries"));
        }
        let raw_type = entry.header().entry_type().as_byte();
        if !(raw_type == b'0' || raw_type == 0 || raw_type == b'5') {
            return Err(integrity(format!(
                "unsupported entry type: {}",
                python_bytes_repr(raw_type)
            )));
        }
        let mut path = entry.path_bytes().to_vec();
        for byte in &mut path {
            if *byte == b'\\' {
                *byte = b'/';
            }
        }
        if raw_type == b'5' {
            path.push(b'/');
        }
        validate_archive_path(&path)?;
        if !seen.insert(path.clone()) {
            return Err(integrity("duplicate archive path"));
        }
        if !path.starts_with(prefix.as_bytes()) {
            return Err(integrity("wrong prefix"));
        }
        expanded += u128::from(entry.size());
        if expanded > u128::from(limits.max_expanded) {
            return Err(limit("expanded size exceeded"));
        }
        if !archive.is_empty()
            && expanded / u128::from(archive.len() as u64) > u128::from(limits.max_ratio)
        {
            return Err(limit("expansion ratio exceeded"));
        }
        if path.as_slice() == manifest_path.as_bytes() {
            if cargo_toml.is_some() {
                return Err(integrity("duplicate Cargo.toml"));
            }
            let mut data = Vec::new();
            (&mut entry)
                .take(limits.max_manifest_bytes as u64 + 1)
                .read_to_end(&mut data)
                .map_err(|error| integrity(error.to_string()))?;
            if data.len() > limits.max_manifest_bytes {
                return Err(limit("manifest too large"));
            }
            cargo_toml =
                Some(String::from_utf8(data).map_err(|error| integrity(error.to_string()))?);
        }
    }
    let Some(cargo_toml) = cargo_toml else {
        return Err(integrity("missing Cargo.toml"));
    };
    parse_manifest_identity(&cargo_toml, name, version)?;
    Ok(ScanResult {
        checksum,
        cargo_toml,
    })
}

pub fn parse_packaged_manifest(text: &str) -> Result<PackagedManifest, Rejection> {
    let parsed = parse_toml(text)?;
    let package = table_get(&parsed, "package")
        .and_then(|value| value.as_map())
        .ok_or_else(|| integrity("missing package in Cargo.toml"))?;
    let name = package
        .get("name")
        .and_then(|value| value.as_str())
        .ok_or_else(|| integrity("missing package name"))?
        .to_string();
    let version = package
        .get("version")
        .and_then(|value| value.as_str())
        .ok_or_else(|| integrity("missing package version"))?
        .to_string();
    let links = package
        .get("links")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let rust_version = package
        .get("rust-version")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let mut features = Features::empty();
    if let Some(raw_features) = table_get(&parsed, "features").and_then(|value| value.as_map()) {
        let mut entries = Vec::new();
        for (key, values) in raw_features.iter() {
            let key = key.as_str().unwrap_or_default();
            let seq = values
                .as_seq()
                .ok_or_else(|| integrity(format!("feature '{key}' is not an array")))?;
            let mut items = Vec::new();
            for value in seq.iter() {
                let item = value
                    .as_str()
                    .ok_or_else(|| integrity(format!("feature '{key}' has non-string entry")))?;
                items.push(item.to_string());
            }
            entries.push((key.to_string(), items));
        }
        features = Features::from_entries(entries);
    }

    let mut dependencies = Vec::new();
    for (section, kind) in [
        ("dependencies", None),
        ("dev-dependencies", Some(DependencyKind::Dev)),
        ("build-dependencies", Some(DependencyKind::Build)),
    ] {
        collect_dependencies(
            &parsed,
            section,
            kind,
            None,
            &mut dependencies,
            false,
        )?;
    }
    if let Some(targets) = table_get(&parsed, "target").and_then(|value| value.as_map()) {
        for (target_spec, table_value) in targets.iter() {
            let target_spec = target_spec.as_str().unwrap_or_default();
            let table = table_value
                .as_map()
                .ok_or_else(|| integrity("target entry is not a table"))?;
            for (section, kind) in [
                ("dependencies", None),
                ("dev-dependencies", Some(DependencyKind::Dev)),
                ("build-dependencies", Some(DependencyKind::Build)),
            ] {
                collect_dependencies(
                    table_value,
                    section,
                    kind,
                    Some(target_spec),
                    &mut dependencies,
                    true,
                )?;
            }
            for (section, _) in table.iter() {
                let section = section.as_str().unwrap_or_default();
                if !is_dependency_section(section) {
                    return Err(integrity(format!("unknown target section '{section}'")));
                }
            }
        }
    }

    Ok(PackagedManifest {
        name,
        version,
        links,
        rust_version,
        features,
        dependencies,
    })
}

fn parse_manifest_identity(manifest: &str, name: &str, version: &str) -> Result<(), Rejection> {
    let parsed = parse_toml(manifest)?;
    let package = table_get(&parsed, "package")
        .and_then(|value| value.as_map())
        .ok_or_else(|| integrity("missing package in Cargo.toml"))?;
    if package.get("name").and_then(|value| value.as_str()) != Some(name)
        || package.get("version").and_then(|value| value.as_str()) != Some(version)
    {
        return Err(integrity("name/version mismatch"));
    }
    Ok(())
}

fn collect_dependencies(
    container: &Value,
    section: &str,
    forced_kind: Option<DependencyKind>,
    target: Option<&str>,
    out: &mut Vec<PackagedDep>,
    scoped: bool,
) -> Result<(), Rejection> {
    let Some(value) = table_get(container, section) else {
        return Ok(());
    };
    let table = value.as_map().ok_or_else(|| {
        let label = if scoped {
            format!("target.{section}")
        } else {
            section.to_string()
        };
        integrity(format!("'{label}' is not a table"))
    })?;
    for (alias, spec) in table.iter() {
        let alias = alias.as_str().unwrap_or_default();
        out.push(parse_dep_spec(alias, spec, forced_kind.clone(), target)?);
    }
    Ok(())
}

fn parse_dep_spec(
    alias: &str,
    spec: &Value,
    forced_kind: Option<DependencyKind>,
    target: Option<&str>,
) -> Result<PackagedDep, Rejection> {
    let (version_req, table) = if let Some(text) = spec.as_str() {
        (text.to_string(), None)
    } else if spec.as_map().is_some() {
        let version_req = table_get(spec, "version")
            .and_then(|value| value.as_str())
            .unwrap_or("*")
            .to_string();
        (version_req, Some(spec))
    } else {
        return Err(integrity(format!("dependency '{alias}' has invalid spec")));
    };
    let mut package = None;
    let mut features = Vec::new();
    let mut optional = false;
    let mut default_features = true;
    let mut kind = forced_kind.clone();
    let mut registry = None;
    if let Some(table) = table {
        if let Some(value) = table_get(table, "package").and_then(|value| value.as_str()) {
            package = Some(value.to_string());
        }
        if let Some(values) = table_get(table, "features").and_then(|value| value.as_seq()) {
            for value in values.iter() {
                let item = value.as_str().ok_or_else(|| {
                    integrity(format!("dependency '{alias}' has non-string feature"))
                })?;
                features.push(item.to_string());
            }
        }
        if let Some(value) = table_get(table, "optional").and_then(|value| value.as_bool()) {
            optional = value;
        }
        if let Some(value) = table_get(table, "default-features").and_then(|value| value.as_bool()) {
            default_features = value;
        }
        if let Some(value) = table_get(table, "kind").and_then(|value| value.as_str()) {
            if forced_kind.is_some() {
                return Err(integrity(format!(
                    "dependency '{alias}' sets kind inside target section"
                )));
            }
            kind = manifest_kind(value)?;
        }
        if let Some(value) = table_get(table, "registry").and_then(|value| value.as_str()) {
            registry = Some(value.to_string());
        }
        if let Some(value) = table_get(table, "registry-index").and_then(|value| value.as_str()) {
            if registry.is_some() {
                return Err(integrity(format!(
                    "dependency '{alias}' sets both registry and registry-index"
                )));
            }
            registry = Some(value.to_string());
        }
        if table_get(table, "git").is_some() || table_get(table, "path").is_some() {
            return Err(integrity(format!(
                "dependency '{alias}' uses git/path source"
            )));
        }
    }
    if version_req != "*" {
        canonical_requirement(&version_req).map_err(|_| {
            integrity(format!(
                "dependency '{alias}' has invalid version_req '{version_req}'"
            ))
        })?;
    }
    Ok(PackagedDep {
        alias: alias.to_string(),
        package,
        version_req,
        features,
        optional,
        default_features,
        target: target.map(str::to_string),
        kind,
        registry,
    })
}

fn manifest_kind(text: &str) -> Result<Option<DependencyKind>, Rejection> {
    match text {
        "normal" => Ok(None),
        "dev" => Ok(Some(DependencyKind::Dev)),
        "build" => Ok(Some(DependencyKind::Build)),
        other => Err(integrity(format!("unknown dependency kind '{other}'"))),
    }
}

fn is_dependency_section(section: &str) -> bool {
    matches!(
        section,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    )
}

fn validate_archive_path(raw: &[u8]) -> Result<(), Rejection> {
    if raw.is_empty() || raw[0] == b'/' {
        return Err(integrity("path traversal"));
    }
    let mut parts: Vec<&[u8]> = raw.split(|byte| *byte == b'/').collect();
    if parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == b"." || *part == b"..")
    {
        return Err(integrity("path traversal"));
    }
    Ok(())
}

fn parse_toml(text: &str) -> Result<Value, Rejection> {
    deser_toml::from_str(text).map_err(|error| integrity(error.to_string()))
}

fn table_get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_map().and_then(|map| map.get(key))
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn python_bytes_repr(byte: u8) -> String {
    let mut out = String::from("b'");
    match byte {
        b'\t' => out.push_str("\\t"),
        b'\n' => out.push_str("\\n"),
        b'\r' => out.push_str("\\r"),
        b'\\' => out.push_str("\\\\"),
        b'\'' => out.push_str("\\'"),
        0x20..=0x7e => out.push(byte as char),
        _ => out.push_str(&format!("\\x{byte:02x}")),
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::{Limits, validate_archive_path};

    #[test]
    fn archive_paths_reject_traversal_and_empty_components() {
        assert!(validate_archive_path(b"crate-1.0.0/src/lib.rs").is_ok());
        assert!(validate_archive_path(b"crate-1.0.0/dir/").is_ok());
        assert!(validate_archive_path(b"/absolute").is_err());
        assert!(validate_archive_path(b"a//b").is_err());
        assert!(validate_archive_path(b"a/./b").is_err());
        assert!(validate_archive_path(b"a/../b").is_err());
        assert!(validate_archive_path(b"").is_err());
    }

    #[test]
    fn default_limits_match_the_python_scan_defaults() {
        let limits = Limits::default();
        assert_eq!(limits.max_compressed, 1024 * 1024 * 1024);
        assert_eq!(limits.max_expanded, 4 * 1024 * 1024 * 1024);
        assert_eq!(limits.max_entries, 200_000);
        assert_eq!(limits.max_ratio, 1000);
        assert_eq!(limits.max_manifest_bytes, 1024 * 1024);
    }
}
