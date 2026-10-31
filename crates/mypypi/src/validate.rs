#![forbid(unsafe_code)]

use crate::error::{Error, Result, Stage};
use crate::models::{PackageVersion, Target, WheelIdentity, canonicalize_name};
use crate::platforms::{current_platform, native_architectures_from_bytes, target_platform_tags};
use crate::process_policy::{redacted_diagnostic, run_capture, uv_environment};
use sha2::{Digest, Sha384, Sha512, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionKind {
    Extras,
    MissingDependency,
    SkipImport,
}

impl ExceptionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExceptionKind::Extras => "extras",
            ExceptionKind::MissingDependency => "missing-dependency",
            ExceptionKind::SkipImport => "skip-import",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ValidationException {
    pub kind: ExceptionKind,
    pub value: String,
    pub reason: String,
}

impl ValidationException {
    pub fn new(kind: ExceptionKind, value: &str, reason: &str) -> Result<Self> {
        if value.trim().is_empty() || value.trim() != value {
            return Err(Error::validation("validation exception value must be non-empty", Stage::Validation));
        }
        if reason.trim().is_empty() || reason.trim() != reason {
            return Err(Error::validation("validation exception requires an explanation", Stage::Validation));
        }
        match kind {
            ExceptionKind::Extras => {
                let parts: Vec<&str> = value.split(',').collect();
                if parts.iter().any(|p| !is_extra_name(p)) {
                    return Err(Error::validation("validation extras must be comma-separated extra names", Stage::Validation));
                }
                let mut seen = BTreeSet::new();
                for p in &parts {
                    if !seen.insert(*p) {
                        return Err(Error::validation("validation extras must be unique", Stage::Validation));
                    }
                }
            }
            ExceptionKind::SkipImport => {
                if !value.split('.').all(is_identifier) {
                    return Err(Error::validation("skipped import must be a dotted Python name", Stage::Validation));
                }
            }
            ExceptionKind::MissingDependency => {
                parse_requirement(value).ok_or_else(|| {
                    Error::validation("missing dependency must be a valid requirement", Stage::Validation)
                })?;
                if value.contains('@') && value.split('@').count() == 2 && looks_like_url(value.split('@').nth(1).unwrap_or("")) {
                    return Err(Error::validation("missing dependency cannot use a direct URL", Stage::Validation));
                }
            }
        }
        Ok(Self { kind, value: value.to_string(), reason: reason.to_string() })
    }
}

fn is_extra_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn looks_like_url(s: &str) -> bool {
    let t = s.trim();
    t.contains("://") || t.starts_with("file:") || t.starts_with('/')
}

#[derive(Debug, Clone)]
pub struct RequirementName {
    pub name: String,
}

fn parse_requirement(value: &str) -> Option<RequirementName> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    // name [extras] (constraints) ; markers
    let before_marker = v.split(';').next().unwrap_or("").trim();
    if before_marker.is_empty() {
        return None;
    }
    if before_marker.contains('@') {
        // direct URL reference — caller rejects for missing-dependency
        let name = before_marker.split('@').next().unwrap_or("").trim();
        if name.is_empty() {
            return None;
        }
        return Some(RequirementName { name: name.to_string() });
    }
    // name ends at first of " \t[(<>=!~,"
    let end = before_marker
        .find([' ', '\t', '[', '(', '<', '>', '=', '!', '~', ','])
        .unwrap_or(before_marker.len());
    let name = before_marker[..end].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return None;
    }
    Some(RequirementName { name: name.to_string() })
}

#[derive(Debug, Clone, Default)]
pub struct ValidationPolicy {
    pub exceptions: Vec<ValidationException>,
}

impl ValidationPolicy {
    pub fn new(exceptions: Vec<ValidationException>) -> Result<Self> {
        let mut seen = BTreeSet::new();
        for e in &exceptions {
            if !seen.insert((e.kind.as_str(), e.value.clone())) {
                return Err(Error::validation("validation exceptions must be unique", Stage::Validation));
            }
        }
        if exceptions.iter().filter(|e| e.kind == ExceptionKind::Extras).count() > 1 {
            return Err(Error::validation("only one validation extras exception is allowed", Stage::Validation));
        }
        Ok(Self { exceptions })
    }

    pub fn extras(&self) -> Option<&str> {
        self.exceptions.iter().find(|e| e.kind == ExceptionKind::Extras).map(|e| e.value.as_str())
    }

    pub fn missing_dependencies(&self) -> Vec<&str> {
        self.exceptions.iter().filter(|e| e.kind == ExceptionKind::MissingDependency).map(|e| e.value.as_str()).collect()
    }

    pub fn skipped_imports(&self) -> BTreeSet<&str> {
        self.exceptions.iter().filter(|e| e.kind == ExceptionKind::SkipImport).map(|e| e.value.as_str()).collect()
    }
}

#[derive(Debug, Clone)]
pub struct WheelMetadata {
    pub identity: WheelIdentity,
    pub requires_python: String,
    pub tags: BTreeSet<String>,
    pub imports: Vec<String>,
    pub native_architectures: BTreeSet<String>,
}

fn failure(package: Option<&PackageVersion>, target: &Target, detail: impl Into<String>) -> Error {
    let mut e = Error::validation(detail, Stage::Validation);
    if let Some(p) = package {
        e = e.with_package(p);
    }
    e = e.with_target(&target.python_version.to_string(), &target.platform);
    e
}

pub fn platform_architecture(platform_name: &str) -> Result<String> {
    let suffix = platform_name.rsplit('-').next().unwrap_or("");
    match suffix {
        "x86_64" => Ok("x86_64".to_string()),
        "aarch64" => Ok("aarch64".to_string()),
        "arm64" => Ok("arm64".to_string()),
        _ => Err(Error::validation(format!("unsupported validation platform: {platform_name}"), Stage::Validation)),
    }
}

pub fn supported_tags(target: &Target, linux_repair_policy: &str) -> Result<BTreeSet<String>> {
    let release = target.python_version.release();
    if release.len() < 2 {
        return Err(Error::validation("target Python version has no minor release", Stage::Validation));
    }
    let platforms = target_platform_tags(target, linux_repair_policy)
        .map_err(|e| Error::validation(e.detail, Stage::Validation))?;
    let mut out = BTreeSet::new();
    let triples = crate::platforms::ordered_target_tags(target, linux_repair_policy)
        .map_err(|e| Error::validation(e.detail, Stage::Validation))?;
    for t in triples {
        out.insert(t.as_string());
    }
    let _ = platforms;
    Ok(out)
}

pub fn local_platform() -> Option<String> {
    current_platform()
}

pub fn inspect_wheel(path: &Path, target: &Target, linux_repair_policy: &str) -> Result<WheelMetadata> {
    metadata_from_wheel(path, target, linux_repair_policy)
}

fn metadata_from_wheel(path: &Path, target: &Target, linux_repair_policy: &str) -> Result<WheelMetadata> {
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let identity = WheelIdentity::parse(&filename).map_err(|e| failure(None, target, format!("invalid wheel {filename}: {}", e.detail)))?;
    let package = identity.package.clone();
    let fail = |d: String| failure(Some(&package), target, format!("invalid wheel {filename}: {d}"));
    let file = std::fs::File::open(path).map_err(|e| fail(e.to_string()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| fail(e.to_string()))?;
    let mut names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| fail(e.to_string()))?;
        names.push(f.name().to_string());
    }
    let res = {
        let mut seen = BTreeSet::new();
        names.iter().any(|n| !seen.insert(n.clone()))
    }; if res {
        return Err(fail("wheel contains duplicate archive paths".to_string()));
    }
    for name in &names {
        let parts: Vec<&str> = name.strip_suffix('/').unwrap_or(name).split('/').collect();
        let bad = name.starts_with('/')
            || name.contains('\\')
            || name.contains('\0')
            || parts.is_empty()
            || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..")
            || parts[0].contains(':');
        if bad {
            return Err(fail("wheel contains an unsafe archive path".to_string()));
        }
        let f = archive.by_index(names.iter().position(|n| n == name).unwrap()).map_err(|e| fail(e.to_string()))?;
        if let Some(mode) = f.unix_mode() {
            // symlink bit 0o120000
            if mode & 0o170000 == 0o120000 {
                return Err(fail("wheel contains a symbolic link".to_string()));
            }
        }
    }
    let dist_infos: BTreeSet<String> = names
        .iter()
        .map(|n| n.split('/').next().unwrap_or("").to_string())
        .filter(|d| d.ends_with(".dist-info"))
        .collect();
    if dist_infos.len() != 1 {
        return Err(fail("wheel must contain exactly one .dist-info directory".to_string()));
    }
    let dist_info = dist_infos.iter().next().unwrap().clone();
    if !crate::wheel_metadata::matches_dist_info(&identity.package.name, &identity.package.version, &dist_info) {
        return Err(fail("wheel .dist-info directory does not match its filename".to_string()));
    }
    let metadata_path = format!("{dist_info}/METADATA");
    let wheel_path = format!("{dist_info}/WHEEL");
    let record_path = format!("{dist_info}/RECORD");
    let name_set: BTreeSet<&str> = names.iter().map(|s| s.as_str()).collect();
    if !(name_set.contains(metadata_path.as_str()) && name_set.contains(wheel_path.as_str()) && name_set.contains(record_path.as_str())) {
        return Err(fail("wheel requires METADATA, WHEEL, and RECORD".to_string()));
    }
    let mut read_member = |name: &str| -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        archive.by_name(name).map_err(|e| fail(e.to_string()))?.read_to_end(&mut buf).map_err(|e| fail(e.to_string()))?;
        Ok(buf)
    };
    let metadata_bytes = read_member(&metadata_path)?;
    let wheel_bytes = read_member(&wheel_path)?;
    let metadata = parse_headers(&metadata_bytes);
    let wheel_headers = parse_headers(&wheel_bytes);
    let meta_versions = metadata.get("metadata-version").cloned().unwrap_or_default();
    let meta_names = metadata.get("name").cloned().unwrap_or_default();
    let meta_pkg_versions = metadata.get("version").cloned().unwrap_or_default();
    let rp_values = metadata.get("requires-python").cloned().unwrap_or_default();
    let wheel_versions = wheel_headers.get("wheel-version").cloned().unwrap_or_default();
    let purelib = wheel_headers.get("root-is-purelib").cloned().unwrap_or_default();
    let generators = wheel_headers.get("generator").cloned().unwrap_or_default();
    if meta_versions.len() != 1 || meta_names.len() != 1 || meta_pkg_versions.len() != 1 || rp_values.len() > 1 || wheel_versions.len() != 1 || purelib.len() != 1 || generators.is_empty() {
        return Err(fail("wheel metadata contains missing or repeated singleton fields".to_string()));
    }
    {
        let parts: Vec<&str> = wheel_versions[0].split('.').collect();
        if parts.len() != 2 || parts.iter().any(|p| !p.chars().all(|c| c.is_ascii_digit())) || parts[0] != "1" {
            return Err(fail("wheel uses an unsupported WHEEL format version".to_string()));
        }
    }
    if purelib[0] != "true" && purelib[0] != "false" {
        return Err(fail("wheel Root-Is-Purelib must be true or false".to_string()));
    }
    let parsed_pkg =
        crate::wheel_metadata::metadata_identity(&meta_names[0], &meta_pkg_versions[0]).map_err(fail)?;
    if parsed_pkg != package {
        return Err(fail("wheel filename and METADATA identity differ".to_string()));
    }
    let requires_python = rp_values.first().cloned().unwrap_or_default();
    if !requires_python.is_empty() && requires_python.parse::<pep440_rs::VersionSpecifiers>().is_err() {
        return Err(fail("wheel has invalid package metadata".to_string()));
    }
    for dep in metadata.get("requires-dist").cloned().unwrap_or_default() {
        if parse_requirement(&dep).is_none() {
            return Err(fail("wheel has an invalid Requires-Dist entry".to_string()));
        }
        if dep.contains('@') {
            return Err(fail("wheel dependencies cannot use direct URLs".to_string()));
        }
    }
    let wheel_tags: BTreeSet<String> = wheel_headers
        .get("tag")
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|v| v.split_whitespace().map(|s| s.to_string()))
        .collect();
    let filename_tags: BTreeSet<String> = crate::platforms::wheel_tag_triples(&filename)
        .map_err(|e| fail(e.detail))?
        .iter()
        .map(|t| t.as_string())
        .collect();
    if wheel_tags.is_empty() || wheel_tags != filename_tags {
        return Err(fail("wheel WHEEL tags do not match its filename".to_string()));
    }
    let supported = supported_tags(target, linux_repair_policy)?;
    if filename_tags.intersection(&supported).next().is_none() {
        return Err(fail("wheel filename tags are incompatible with target".to_string()));
    }
    let execution = crate::platforms::target_python_version(target);
    let spec_ok = requires_python.trim().is_empty()
        || requires_python.parse::<pep440_rs::VersionSpecifiers>().is_ok_and(|s| s.contains(&execution));
    if !spec_ok {
        return Err(fail(format!("wheel Requires-Python excludes CPython {execution}")));
    }
    // RECORD verification
    let record_bytes = read_member(&record_path)?;
    let record_text = String::from_utf8(record_bytes).map_err(|_| fail("wheel RECORD is not valid UTF-8".to_string()))?;
    let mut recorded: BTreeMap<String, (String, String)> = BTreeMap::new();
    for row in parse_csv(&record_text).map_err(fail)? {
        if row.len() != 3 || row[0].is_empty() {
            return Err(fail("wheel RECORD contains an invalid row".to_string()));
        }
        if recorded.contains_key(&row[0]) {
            return Err(fail(format!("wheel RECORD duplicates {}", row[0])));
        }
        recorded.insert(row[0].clone(), (row[1].clone(), row[2].clone()));
    }
    let file_names: BTreeSet<String> = names.iter().filter(|n| !n.ends_with('/')).cloned().collect();
    if recorded.keys().cloned().collect::<BTreeSet<_>>() != file_names {
        return Err(fail("wheel RECORD does not match its files".to_string()));
    }
    for name in &file_names {
        let (digest_value, size_value) = recorded.get(name.as_str()).expect("recorded");
        if *name == record_path {
            if !digest_value.is_empty() || !size_value.is_empty() {
                return Err(fail("wheel RECORD row must leave RECORD hash and size empty".to_string()));
            }
            continue;
        }
        if size_value.chars().any(|c| !c.is_ascii_digit()) || size_value.is_empty() {
            return Err(fail(format!("wheel RECORD has an invalid size for {name}")));
        }
        let (algorithm, sep, expected_digest) = match digest_value.find('=') {
            Some(i) => (&digest_value[..i], true, &digest_value[i + 1..]),
            None => ("", false, ""),
        };
        if !sep || !matches!(algorithm, "sha256" | "sha384" | "sha512") {
            return Err(fail(format!("wheel RECORD has an invalid hash for {name}")));
        }
        let mut hasher_bytes = Vec::new();
        archive.by_name(name).map_err(|e| fail(format!("wheel RECORD has an invalid hash for {name}: {e}")))?.read_to_end(&mut hasher_bytes).map_err(|e| fail(format!("wheel RECORD has an invalid hash for {name}: {e}")))?;
        let computed = match algorithm {
            "sha256" => {
                let mut h = Sha256::new();
                h.update(&hasher_bytes);
                base64_url_nopad(&h.finalize())
            }
            "sha384" => {
                let mut h = Sha384::new();
                h.update(&hasher_bytes);
                base64_url_nopad(&h.finalize())
            }
            "sha512" => {
                let mut h = Sha512::new();
                h.update(&hasher_bytes);
                base64_url_nopad(&h.finalize())
            }
            _ => unreachable!(),
        };
        if computed != *expected_digest {
            return Err(fail(format!("wheel RECORD hash does not match {name}")));
        }
        let info = archive.by_name(name).map_err(|e| fail(e.to_string()))?;
        if info.size().to_string() != *size_value {
            return Err(fail(format!("wheel RECORD size does not match {name}")));
        }
    }
    // imports + native architectures
    let mut imports: BTreeSet<String> = BTreeSet::new();
    let mut native_architectures: BTreeSet<String> = BTreeSet::new();
    let expected_arch = platform_architecture(&target.platform)?;
    // map arm64<->aarch64
    let arch_ok = |have: &str| {
        have == expected_arch
            || (expected_arch == "arm64" && have == "aarch64")
            || (expected_arch == "aarch64" && have == "arm64")
    };
    for name in &names {
        if name.ends_with('/') || *name == format!("{dist_info}/RECORD") {
            continue;
        }
        let mut head = [0u8; 4096];
        let n = {
            let mut m = archive.by_name(name).map_err(|e| fail(e.to_string()))?;
            let mut total = 0;
            while total < head.len() {
                let r = m.read(&mut head[total..]).map_err(|e| fail(e.to_string()))?;
                if r == 0 {
                    break;
                }
                total += r;
            }
            total
        };
        let arches = native_architectures_from_bytes(&head[..n]).map_err(|e| fail(format!("native artifact: {e}")))?;
        if !arches.is_empty() {
            for a in &arches {
                native_architectures.insert(a.clone());
            }
            if !arches.iter().any(|a| arch_ok(a)) {
                let mut have: Vec<&String> = arches.iter().collect();
                have.sort();
                return Err(fail(format!(
                    "native artifact {name} does not contain target architecture {expected_arch}"
                )));
            }
        }
        let parts: Vec<&str> = name.split('/').collect();
        if parts[0].ends_with(".dist-info") || parts[0].ends_with(".data") {
            continue;
        }
        if parts.len() > 1
            && (parts.last().is_some_and(|f| f.ends_with(".py")) || is_extension_module(parts.last().unwrap_or(&"")))
        {
            if is_identifier(parts[0]) {
                imports.insert(parts[0].to_string());
            }
        } else if parts.len() == 1 {
            let filename_only = parts[0];
            if let Some(module) = top_level_module(filename_only)
                && is_identifier(&module) {
                    imports.insert(module);
                }
        }
    }
    Ok(WheelMetadata {
        identity,
        requires_python,
        tags: wheel_tags,
        imports: imports.into_iter().collect(),
        native_architectures,
    })
}

fn is_extension_module(filename: &str) -> bool {
    filename.ends_with(".so") || filename.ends_with(".pyd")
}

fn top_level_module(filename: &str) -> Option<String> {
    if let Some(stem) = filename.strip_suffix(".py") {
        if !stem.contains('.') && !stem.is_empty() {
            return Some(stem.to_string());
        }
        return None;
    }
    for suffix in [".so", ".pyd"] {
        if let Some(stem) = filename.strip_suffix(suffix) {
            // strip ABI tags like .cpython-315-darwin.so
            let base = stem.split('.').next().unwrap_or("");
            if !base.is_empty() && !base.contains('/') {
                return Some(base.to_string());
            }
        }
    }
    None
}

struct TempDirGuard {
    path: std::path::PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn base64_url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut n: u32 = 0;
        for b in chunk {
            n = (n << 8) | (*b as u32);
        }
        let total_bits = chunk.len() * 8;
        let chars = total_bits.div_ceil(6);
        n <<= 6 * chars - total_bits;
        for i in 0..chars {
            let shift = 6 * (chars - 1 - i);
            let idx = ((n >> shift) & 0x3F) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    out
}

fn parse_headers(bytes: &[u8]) -> BTreeMap<String, Vec<String>> {
    let text = String::from_utf8_lossy(bytes);
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if (line.starts_with(' ') || line.starts_with('\t')) && current.is_some() {
            let key = current.clone().unwrap();
            if let Some(vals) = map.get_mut(&key)
                && let Some(last) = vals.last_mut() {
                    last.push(' ');
                    last.push_str(line.trim());
                }
            continue;
        }
        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim().to_lowercase();
            let val = line[colon + 1..].trim().to_string();
            map.entry(key.clone()).or_default().push(val);
            current = Some(key);
        }
    }
    map
}

fn parse_csv(text: &str) -> std::result::Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let row = parse_csv_row(line).ok_or_else(|| format!("line {}", lineno + 1))?;
        rows.push(row);
    }
    Ok(rows)
}

fn parse_csv_row(line: &str) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    let mut field_quoted = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' && cur.is_empty() && !field_quoted {
            in_quotes = true;
            field_quoted = true;
        } else if c == ',' {
            fields.push(cur.clone());
            cur.clear();
            field_quoted = false;
        } else {
            cur.push(c);
        }
    }
    if in_quotes {
        return None;
    }
    fields.push(cur);
    Some(fields)
}

pub fn install_command(python: &Path, wheel: &Path, wheel_set: &Path, policy: &ValidationPolicy) -> Vec<String> {
    let mut target = wheel.display().to_string();
    if let Some(extras) = policy.extras() {
        target = format!("{target}[{extras}]");
    }
    let mut cmd = vec![
        "uv".to_string(),
        "pip".to_string(),
        "install".to_string(),
        "--no-config".to_string(),
        "--no-cache".to_string(),
        format!("--python={}", python.display()),
        "--no-index".to_string(),
        "--only-binary=:all:".to_string(),
        format!("--find-links={}", wheel_set.display()),
        target,
    ];
    for dep in policy.missing_dependencies() {
        cmd.push(dep.to_string());
    }
    cmd
}

pub fn uv_environment_map() -> BTreeMap<String, String> {
    uv_environment(None)
}

fn run_validation(
    metadata: &WheelMetadata,
    wheel: &Path,
    wheel_set: &Path,
    target: &Target,
    policy: &ValidationPolicy,
) -> Result<()> {
    // interpreter gate: this port runs under the host CPython; enforce version match
    let probed = probe_running_python();
    let release = target.python_version.release();
    if release.len() >= 2 {
        match probed {
            Some((impl_name, major, minor)) => {
                if impl_name != "cpython" || major != release[0] || minor != release[1] {
                    return Err(failure(
                        Some(&metadata.identity.package),
                        target,
                        format!(
                            "validation requires CPython {}.{}; current interpreter is {impl_name} {major}.{minor}",
                            release[0], release[1]
                        ),
                    ));
                }
            }
            None => {
                return Err(failure(
                    Some(&metadata.identity.package),
                    target,
                    "validation requires a local CPython interpreter on PATH",
                ));
            }
        }
    }
    let local = local_platform();
    if local.as_deref() != Some(target.platform.as_str()) {
        return Err(failure(
            Some(&metadata.identity.package),
            target,
            format!(
                "validation requires a native {} runner; current runner is {}",
                target.platform,
                local.as_deref().unwrap_or("unsupported")
            ),
        ));
    }
    let env = uv_environment_map();
    let python_exe = crate::platforms::python_binary();
    let scratch_path = crate::process_policy::make_temp_dir("mypypi-validate-").map_err(|e| failure(Some(&metadata.identity.package), target, format!("validation process failed: {e}")))?;
    let scratch = TempDirGuard { path: scratch_path };
    let neutral = scratch.path.canonicalize().unwrap_or_else(|_| scratch.path.clone());
    let run = |command: Vec<String>, stage: &str| -> Result<()> {
        match run_capture(&command, Some(&neutral), &env, None, 300) {
            Ok(out) if out.success() => Ok(()),
            Ok(out) => {
                let mut detail = format!("{stage} failed for {}", wheel.file_name().and_then(|s| s.to_str()).unwrap_or("?"));
                if !out.stderr.trim().is_empty() {
                    detail = format!("{detail}: {}", redacted_diagnostic(out.stderr.trim(), 2000));
                }
                Err(failure(Some(&metadata.identity.package), target, detail))
            }
            Err(crate::process_policy::RunError::Timeout { .. }) => {
                Err(failure(Some(&metadata.identity.package), target, format!("{stage} timed out after 300 seconds")))
            }
            Err(e) => Err(failure(Some(&metadata.identity.package), target, format!("validation process failed: {e}"))),
        }
    };
    let venv_dir = neutral.join("venv");
    run(
        vec![
            "uv".to_string(),
            "venv".to_string(),
            "--no-config".to_string(),
            "--quiet".to_string(),
            "--python-preference=only-system".to_string(),
            format!("--python={python_exe}"),
            venv_dir.display().to_string(),
        ],
        "environment creation",
    )?;
    let python = if cfg!(windows) {
        venv_dir.join("Scripts/python.exe")
    } else {
        venv_dir.join("bin/python")
    };
    run(install_command(&python, wheel, wheel_set, policy), "dependency installation")?;
    let skipped = policy.skipped_imports();
    for import_name in &metadata.imports {
        if skipped.contains(import_name.as_str()) {
            continue;
        }
        run(
            vec![
                python.display().to_string(),
                "-I".to_string(),
                "-c".to_string(),
                format!("__import__({import_name:?})"),
            ],
            &format!("import {import_name}"),
        )?;
    }
    Ok(())
}

fn probe_running_python() -> Option<(String, u64, u64)> {
    let python = crate::platforms::python_binary();
    let out = std::process::Command::new(python)
        .args(["-c", "import sys; print(sys.implementation.name, sys.version_info.major, sys.version_info.minor)"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    Some((parts.next()?.to_string(), parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

pub fn validate_wheel(
    wheel: &Path,
    target: &Target,
    wheel_set: &Path,
    policy: &ValidationPolicy,
    linux_repair_policy: &str,
) -> Result<WheelMetadata> {
    let metadata = inspect_wheel(wheel, target, linux_repair_policy)?;
    if !wheel.is_file() {
        return Err(failure(Some(&metadata.identity.package), target, format!("wheel does not exist: {}", wheel.display())));
    }
    run_validation(&metadata, wheel, wheel_set, target, policy)?;
    Ok(metadata)
}

pub fn validate_wheels(
    wheels: &[PathBuf],
    targets: &[Target],
    wheel_set: &Path,
    policies: &BTreeMap<PackageVersion, ValidationPolicy>,
    linux_repair_policy: &str,
) -> Result<Vec<(WheelMetadata, Target)>> {
    if wheels.is_empty() {
        return Ok(Vec::new());
    }
    if targets.is_empty() {
        return Err(Error::validation("validation requires at least one target", Stage::Validation));
    }
    {
        let mut seen = BTreeSet::new();
        for t in targets {
            if !seen.insert((t.python_version.to_string(), t.platform.clone())) {
                return Err(Error::validation("validation targets must be unique", Stage::Validation));
            }
        }
    }
    let mut out = Vec::new();
    let mut sorted = wheels.to_vec();
    sorted.sort();
    for wheel in &sorted {
        let filename = wheel.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let identity = WheelIdentity::parse(filename).map_err(|e| {
            Error::validation(format!("invalid wheel {filename}: {}", e.detail), Stage::Validation)
        })?;
        for target in targets {
            let supported = supported_tags(target, linux_repair_policy)?;
            if triple_intersects(&filename_tags_set(filename)?, &supported) {
                let policy = policies.get(&identity.package).cloned().unwrap_or_default();
                let meta = validate_wheel(wheel, target, wheel_set, &policy, linux_repair_policy)?;
                out.push((meta, target.clone()));
            }
        }
    }
    Ok(out)
}

fn filename_tags_set(filename: &str) -> Result<BTreeSet<String>> {
    Ok(crate::platforms::wheel_tag_triples(filename)
        .map_err(|e| Error::validation(e.detail, Stage::Validation))?
        .iter()
        .map(|t| t.as_string())
        .collect())
}

fn triple_intersects(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    a.intersection(b).next().is_some()
}

pub fn canonical_package_name(name: &str) -> String {
    canonicalize_name(name)
}

#[allow(dead_code)]
fn version_parse_unused(s: &str) -> std::result::Result<pep440_rs::Version, pep440_rs::VersionParseError> {
    pep440_rs::Version::from_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target::new("3.15", "macos-arm64").unwrap()
    }

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/mypypi/wheels/validation_fixture-1.0-py3-none-any.whl")
    }

    #[test]
    fn inspects_fixture_wheel() {
        let path = fixture();
        if !path.exists() {
            return;
        }
        let meta = inspect_wheel(&path, &target(), "manylinux_2_28").unwrap();
        assert_eq!(meta.identity.package.name, "validation-fixture");
        assert_eq!(meta.requires_python, ">=3.15,<3.16");
        assert!(!meta.tags.is_empty());
    }

    #[test]
    fn rejects_bad_policy_construction() {
        assert!(ValidationException::new(ExceptionKind::Extras, "", "reason").is_err());
        assert!(ValidationException::new(ExceptionKind::Extras, "a,a", "reason").is_err());
        assert!(ValidationException::new(ExceptionKind::SkipImport, "not an ident!", "reason").is_err());
        assert!(ValidationException::new(ExceptionKind::MissingDependency, "http://x", "reason").is_err());
        let policy = ValidationPolicy::new(vec![
            ValidationException::new(ExceptionKind::Extras, "a", "r").unwrap(),
            ValidationException::new(ExceptionKind::Extras, "b", "r").unwrap(),
        ]);
        assert!(policy.is_err());
    }

    #[test]
    fn install_command_renders_extras_and_deps() {
        let policy = ValidationPolicy::new(vec![
            ValidationException::new(ExceptionKind::Extras, "socks", "why").unwrap(),
            ValidationException::new(ExceptionKind::MissingDependency, "requests>=2", "why").unwrap(),
        ])
        .unwrap();
        let cmd = install_command(Path::new("/venv/bin/python"), Path::new("/ws/foo-1.0-py3-none-any.whl"), Path::new("/ws"), &policy);
        assert!(cmd.iter().any(|a| a.contains("[socks]")));
        assert!(cmd.iter().any(|a| a == "requests>=2"));
    }

    #[test]
    fn supported_tags_cover_pure_wheel() {
        let t = target();
        let tags = supported_tags(&t, "manylinux_2_28").unwrap();
        assert!(tags.iter().any(|tag| tag == "py3-none-any"));
    }

    #[test]
    fn accepts_noncanonical_metadata_name() {
        // Upstream wheels keep the project's original capitalization in
        // METADATA's Name field (e.g. Pygments); identity comparison must
        // normalize, not reject.
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg-1.0-py3-none-any.whl");
        let metadata = b"Metadata-Version: 2.1\nName: Pkg\nVersion: 1.0\nRequires-Python: >=3.9\n";
        let wheel = b"Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n";
        let record_row = |name: &str, bytes: &[u8]| {
            let mut h = Sha256::new();
            h.update(bytes);
            format!("{name},sha256={},{}", base64_url_nopad(&h.finalize()), bytes.len())
        };
        let record = format!(
            "{}\n{}\n{},,\n",
            record_row("pkg-1.0.dist-info/METADATA", metadata),
            record_row("pkg-1.0.dist-info/WHEEL", wheel),
            "pkg-1.0.dist-info/RECORD"
        );
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("pkg-1.0.dist-info/METADATA", opts).unwrap();
            use std::io::Write;
            zip.write_all(metadata).unwrap();
            zip.start_file("pkg-1.0.dist-info/WHEEL", opts).unwrap();
            zip.write_all(wheel).unwrap();
            zip.start_file("pkg-1.0.dist-info/RECORD", opts).unwrap();
            zip.write_all(record.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        let meta = inspect_wheel(&path, &target(), "manylinux_2_28").unwrap();
        assert_eq!(meta.identity.package.name, "pkg");
    }
}
