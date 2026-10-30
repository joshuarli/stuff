use crate::error::{Error, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path};
use tar::Archive;

#[derive(Debug, Clone)]
pub struct ScanLimits {
    pub max_compressed: u64,
    pub max_expanded: u64,
    pub max_entries: usize,
    pub max_ratio: u64,
    pub max_manifest_bytes: u64,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_compressed: 1024 * 1024 * 1024,
            max_expanded: 4 * 1024 * 1024 * 1024,
            max_entries: 200_000,
            max_ratio: 1000,
            max_manifest_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub struct ScanResult {
    pub checksum: String,
    pub cargo_toml: String,
}

fn validate_archive_path(raw: &str) -> Result<()> {
    if raw.is_empty() || raw.starts_with('/') {
        return Err(Error::Integrity("path traversal".to_string()));
    }
    let p = Path::new(raw);
    for comp in p.components() {
        match comp {
            Component::Normal(_) => {}
            _ => return Err(Error::Integrity("path traversal".to_string())),
        }
    }
    Ok(())
}

pub fn scan_crate_staged<R: Read + Seek>(
    mut reader: R,
    expected_len: u64,
    max_compressed: u64,
    expected_sha256: Option<&str>,
    name: &str,
    version: &str,
    limits: &ScanLimits,
) -> Result<ScanResult> {
    if expected_len > max_compressed {
        return Err(Error::Limit("compressed size exceeded".to_string()));
    }
    if expected_len > limits.max_compressed {
        return Err(Error::Limit("compressed size exceeded".to_string()));
    }
    let mut hasher = Sha256::new();
    let mut counted: u64 = 0;
    let mut buf = [0u8; 65536];
    reader.seek(SeekFrom::Start(0))?;
    loop {
        let n = reader.read(&mut buf).map_err(|e| Error::Operation(e.to_string()))?;
        if n == 0 {
            break;
        }
        counted += n as u64;
        if counted > max_compressed || counted > limits.max_compressed {
            return Err(Error::Limit("compressed size exceeded".to_string()));
        }
        hasher.update(&buf[..n]);
    }
    if counted != expected_len {
        return Err(Error::Integrity("staged upload length mismatch".to_string()));
    }
    let checksum = format!("{:x}", hasher.finalize_reset());
    if let Some(exp) = expected_sha256
        && exp != checksum
    {
        return Err(Error::Integrity("staged upload checksum mismatch".to_string()));
    }

    reader.seek(SeekFrom::Start(0))?;
    scan_stream(reader, name, version, limits, &checksum, counted)
}

fn scan_stream<R: Read>(
    reader: R,
    name: &str,
    version: &str,
    limits: &ScanLimits,
    checksum: &str,
    compressed_len: u64,
) -> Result<ScanResult> {
    let gz = GzDecoder::new(reader);
    let mut archive = Archive::new(gz);
    let mut entries_seen: usize = 0;
    let mut expanded: u64 = 0;
    let mut found_cargo_toml: Option<String> = None;
    let mut seen_paths: HashSet<String> = HashSet::new();
    let prefix = format!("{}-{}/", name, version);

    for entry in archive.entries().map_err(|e| Error::Integrity(e.to_string()))? {
        let entry = entry.map_err(|e| Error::Integrity(e.to_string()))?;
        entries_seen += 1;
        if entries_seen > limits.max_entries {
            return Err(Error::Limit("too many entries".to_string()));
        }
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            return Err(Error::Integrity(format!(
                "unsupported entry type: {:?}",
                entry_type
            )));
        }
        let path = entry.path().map_err(|e| Error::Integrity(e.to_string()))?;
        let path_str = path.to_string_lossy().replace('\\', "/");
        validate_archive_path(&path_str)?;
        if !seen_paths.insert(path_str.clone()) {
            return Err(Error::Integrity("duplicate archive path".to_string()));
        }
        if !path_str.starts_with(&prefix) {
            return Err(Error::Integrity("wrong prefix".to_string()));
        }
        let size = entry.size();
        expanded = expanded.saturating_add(size);
        if expanded > limits.max_expanded {
            return Err(Error::Limit("expanded size exceeded".to_string()));
        }
        if compressed_len > 0 && expanded.saturating_div(compressed_len.max(1)) > limits.max_ratio {
            return Err(Error::Limit("expansion ratio exceeded".to_string()));
        }
        if path_str == format!("{}Cargo.toml", prefix) {
            if found_cargo_toml.is_some() {
                return Err(Error::Integrity("duplicate Cargo.toml".to_string()));
            }
            let mut manifest = Vec::new();
            let mut limited = entry.take(limits.max_manifest_bytes.saturating_add(1));
            limited
                .read_to_end(&mut manifest)
                .map_err(|e| Error::Integrity(e.to_string()))?;
            if manifest.len() as u64 > limits.max_manifest_bytes {
                return Err(Error::Limit("manifest too large".to_string()));
            }
            let s = String::from_utf8(manifest).map_err(|e| Error::Integrity(e.to_string()))?;
            found_cargo_toml = Some(s);
        }
    }

    let cargo_toml = found_cargo_toml.ok_or_else(|| Error::Integrity("missing Cargo.toml".to_string()))?;
    assert_manifest_parity(&cargo_toml, name, version)?;

    Ok(ScanResult {
        checksum: checksum.to_string(),
        cargo_toml,
    })
}

#[derive(Debug, Clone)]
pub struct PackagedManifest {
    pub name: String,
    pub version: String,
    pub links: Option<String>,
    pub rust_version: Option<String>,
    pub features: std::collections::BTreeMap<String, Vec<String>>,
    pub dependencies: Vec<PackagedDep>,
}

#[derive(Debug, Clone)]
pub struct PackagedDep {
    pub alias: String,
    pub package: Option<String>,
    pub version_req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: Option<String>,
    pub registry: Option<String>,
}

fn dep_table_entry_kind(kind: Option<&str>) -> Result<Option<String>> {
    match kind {
        None | Some("normal") => Ok(None),
        Some("dev") => Ok(Some("dev".to_string())),
        Some("build") => Ok(Some("build".to_string())),
        Some(other) => Err(Error::Integrity(format!("unknown dependency kind '{other}'"))),
    }
}

pub fn parse_packaged_manifest(manifest: &str) -> Result<PackagedManifest> {
    let parsed: deser_value::Value =
        deser_toml::from_str(manifest).map_err(|e| Error::Integrity(e.to_string()))?;
    let pkg = parsed
        .get("package")
        .ok_or_else(|| Error::Integrity("missing package in Cargo.toml".to_string()))?;
    let name = pkg
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Integrity("missing package name".to_string()))?
        .to_string();
    let version = pkg
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Integrity("missing package version".to_string()))?
        .to_string();
    let links = pkg.get("links").and_then(|v| v.as_str()).map(|s| s.to_string());
    let rust_version = pkg
        .get("rust-version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut features = std::collections::BTreeMap::new();
    if let Some(feats) = parsed.get("features").and_then(|v| v.as_map()) {
        for (k, v) in feats.iter() {
            let k = k.as_str().ok_or_else(|| Error::Integrity("feature key is not a string".to_string()))?;
            let arr = v
                .as_seq()
                .ok_or_else(|| Error::Integrity(format!("feature '{k}' is not an array")))?;
            let mut vals = Vec::new();
            for item in arr.iter() {
                let s = item
                    .as_str()
                    .ok_or_else(|| Error::Integrity(format!("feature '{k}' has non-string entry")))?;
                vals.push(s.to_string());
            }
            features.insert(k.to_string(), vals);
        }
    }

    let mut dependencies = Vec::new();
    collect_dep_table(&parsed, "dependencies", None, &mut dependencies)?;
    collect_dep_table(&parsed, "dev-dependencies", Some("dev"), &mut dependencies)?;
    collect_dep_table(&parsed, "build-dependencies", Some("build"), &mut dependencies)?;
    if let Some(target) = parsed.get("target").and_then(|v| v.as_map()) {
        for (target_key, target_table) in target.iter() {
            let target_spec = target_key.as_str().ok_or_else(|| Error::Integrity("target key is not a string".to_string()))?.to_string();
            let t = Some(target_spec);
            collect_dep_table_scoped(target_table, "dependencies", None, &t, &mut dependencies)?;
            collect_dep_table_scoped(target_table, "dev-dependencies", Some("dev"), &t, &mut dependencies)?;
            collect_dep_table_scoped(target_table, "build-dependencies", Some("build"), &t, &mut dependencies)?;
            let target_obj = target_table
                .as_map()
                .ok_or_else(|| Error::Integrity("target entry is not a table".to_string()))?;
            for (section_key, _) in target_obj.iter() {
                let section = section_key.as_str().ok_or_else(|| Error::Integrity("target section key is not a string".to_string()))?;
                if section != "dependencies" && section != "dev-dependencies" && section != "build-dependencies" {
                    return Err(Error::Integrity(format!("unknown target section '{section}'")));
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

fn collect_dep_table(
    manifest: &deser_value::Value,
    section: &str,
    forced_kind: Option<&str>,
    out: &mut Vec<PackagedDep>,
) -> Result<()> {
    let Some(table) = manifest.get(section) else {
        return Ok(());
    };
    let table = table
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("'{section}' is not a table")))?;
    for (alias_key, spec) in table.iter() {
        let alias = alias_key.as_str().ok_or_else(|| Error::Integrity("dependency alias is not a string".to_string()))?;
        out.push(parse_dep_spec(alias, spec, forced_kind, None)?);
    }
    Ok(())
}

fn collect_dep_table_scoped(
    target_table: &deser_value::Value,
    section: &str,
    forced_kind: Option<&str>,
    target: &Option<String>,
    out: &mut Vec<PackagedDep>,
) -> Result<()> {
    let Some(table) = target_table.get(section) else {
        return Ok(());
    };
    let table = table
        .as_map()
        .ok_or_else(|| Error::Integrity(format!("'target.{section}' is not a table")))?;
    for (alias_key, spec) in table.iter() {
        let alias = alias_key.as_str().ok_or_else(|| Error::Integrity("dependency alias is not a string".to_string()))?;
        out.push(parse_dep_spec(alias, spec, forced_kind, target.clone())?);
    }
    Ok(())
}

fn parse_dep_spec(
    alias: &str,
    spec: &deser_value::Value,
    forced_kind: Option<&str>,
    target: Option<String>,
) -> Result<PackagedDep> {
    let (version_req, table) = if let Some(v) = spec.as_str() {
        (v.to_string(), None)
    } else if let Some(t) = spec.as_map() {
        let v = t.get("version").and_then(|x| x.as_str()).unwrap_or("*").to_string();
        (v, Some(t))
    } else {
        return Err(Error::Integrity(format!("dependency '{alias}' has invalid spec")));
    };
    let mut package = None;
    let mut features = Vec::new();
    let mut optional = false;
    let mut default_features = true;
    let mut kind = dep_table_entry_kind(forced_kind)?;
    let mut registry = None;
    if let Some(t) = table {
        if let Some(p) = t.get("package").and_then(|x| x.as_str()) {
            package = Some(p.to_string());
        }
        if let Some(f) = t.get("features").and_then(|x| x.as_seq()) {
            for item in f.iter() {
                features.push(
                    item.as_str()
                        .ok_or_else(|| Error::Integrity(format!("dependency '{alias}' has non-string feature")))?
                        .to_string(),
                );
            }
        }
        if let Some(o) = t.get("optional").and_then(|x| x.as_bool()) {
            optional = o;
        }
        if let Some(d) = t.get("default-features").and_then(|x| x.as_bool()) {
            default_features = d;
        }
        if let Some(k) = t.get("kind").and_then(|x| x.as_str()) {
            if forced_kind.is_some() {
                return Err(Error::Integrity(format!("dependency '{alias}' sets kind inside target section")));
            }
            kind = dep_table_entry_kind(Some(k))?;
        }
        if let Some(r) = t.get("registry").and_then(|x| x.as_str()) {
            registry = Some(r.to_string());
        }
        if let Some(r) = t.get("registry-index").and_then(|x| x.as_str()) {
            if registry.is_some() {
                return Err(Error::Integrity(format!(
                    "dependency '{alias}' sets both registry and registry-index"
                )));
            }
            registry = Some(r.to_string());
        }
        if t.get("git").is_some() || t.get("path").is_some() {
            return Err(Error::Integrity(format!("dependency '{alias}' uses git/path source")));
        }
    }
    if version_req != "*" && crate::version::VersionReq::parse(&version_req).is_err() {
        return Err(Error::Integrity(format!(
            "dependency '{alias}' has invalid version_req '{version_req}'"
        )));
    }
    Ok(PackagedDep {
        alias: alias.to_string(),
        package,
        version_req,
        features,
        optional,
        default_features,
        target,
        kind,
        registry,
    })
}

pub fn assert_manifest_parity(manifest: &str, name: &str, version: &str) -> Result<deser_value::Value> {
    let parsed: deser_value::Value =
        deser_toml::from_str(manifest).map_err(|e| Error::Integrity(e.to_string()))?;
    match parsed.get("package") {
        Some(pkg) => {
            let pkg_name = pkg.get("name").and_then(|v| v.as_str());
            let pkg_ver = pkg.get("version").and_then(|v| v.as_str());
            if pkg_name != Some(name) || pkg_ver != Some(version) {
                return Err(Error::Integrity("name/version mismatch".to_string()));
            }
        }
        None => {
            return Err(Error::Integrity("missing package in Cargo.toml".to_string()));
        }
    }
    Ok(parsed)
}

pub fn scan_crate<R: Read>(mut reader: R, name: &str, version: &str, limits: &ScanLimits) -> Result<ScanResult> {
    let mut spill = tempfile::NamedTempFile::new().map_err(|e| Error::Operation(e.to_string()))?;
    let mut counted: u64 = 0;
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf).map_err(|e| Error::Operation(e.to_string()))?;
        if n == 0 {
            break;
        }
        counted += n as u64;
        if counted > limits.max_compressed {
            return Err(Error::Limit("compressed size exceeded".to_string()));
        }
        std::io::Write::write_all(&mut spill, &buf[..n])
            .map_err(|e| Error::Operation(e.to_string()))?;
    }
    let mut f: File = spill.into_file();
    f.seek(SeekFrom::Start(0))?;
    scan_crate_staged(f, counted, limits.max_compressed, None, name, version, limits)
}
