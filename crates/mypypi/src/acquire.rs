#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};
use crate::json::{self, JVal};
use crate::models::{PackageVersion, Target, WheelIdentity, canonicalize_name};
use crate::platforms::{native_architectures_from_bytes, target_python_version, wheel_rank, wheel_tag_triples};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ReleaseFile {
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub packagetype: String,
    pub yanked: bool,
    pub requires_python: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub package: PackageVersion,
    pub requires_python: String,
    pub wheels: Vec<ReleaseFile>,
    pub sdists: Vec<ReleaseFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilityCategory {
    WheelTag,
    PythonVersion,
    Architecture,
    RepairPolicy,
}

impl CompatibilityCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompatibilityCategory::WheelTag => "wheel-tag",
            CompatibilityCategory::PythonVersion => "python-version",
            CompatibilityCategory::Architecture => "architecture",
            CompatibilityCategory::RepairPolicy => "repair-policy",
        }
    }
}

#[derive(Debug)]
pub struct CompatibilityRejection {
    pub category: CompatibilityCategory,
    pub detail: String,
    pub package: Option<PackageVersion>,
    pub target: Option<Target>,
}

impl CompatibilityRejection {
    pub fn to_error(&self, stage: Stage) -> Error {
        let mut e = Error::new(Kind::Incompatible, self.detail.clone(), stage);
        if let Some(p) = &self.package {
            e = e.with_package(p);
        }
        if let Some(t) = &self.target {
            e = e.with_target(&t.python_version.to_string(), &t.platform);
        }
        e
    }
}

impl std::fmt::Display for CompatibilityRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for CompatibilityRejection {}

fn is_lower_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn release_from_metadata(
    package: &PackageVersion,
    data: &str,
    allow_yanked_reason: Option<&str>,
) -> Result<Release> {
    let root = json::parse(data).map_err(|e| {
        Error::integrity(format!("invalid PyPI release metadata: {e}"), Stage::Acquisition)
            .with_package(package)
    })?;
    let fail = |d: String| Error::integrity(format!("invalid PyPI release metadata: {d}"), Stage::Acquisition).with_package(package.clone());
    let info = root.get("info").ok_or_else(|| fail("missing info".to_string()))?;
    let name = info.get("name").and_then(|v| v.as_str()).ok_or_else(|| fail("missing name".to_string()))?;
    let version_text = info.get("version").and_then(|v| v.as_str()).ok_or_else(|| fail("missing version".to_string()))?;
    let release_version = pep440_rs::Version::from_str(version_text).map_err(|e| fail(e.to_string()))?;
    if canonicalize_name(name) != package.name || release_version != package.version {
        return Err(fail("PyPI release metadata does not match requested package and version".to_string()));
    }
    let requires_python = info
        .get("requires_python")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // validate specifier parses
    let _: pep440_rs::VersionSpecifiers = requires_python.parse().map_err(|e| fail(format!("invalid Requires-Python: {e}")))?;
    let files = root.get("urls").and_then(|v| v.as_arr()).ok_or_else(|| fail("missing urls".to_string()))?;
    let mut wheels = Vec::new();
    let mut sdists = Vec::new();
    for item in files {
        let filename = item.get("filename").and_then(|v| v.as_str()).ok_or_else(|| fail("missing filename".to_string()))?;
        let file_url = item.get("url").and_then(|v| v.as_str()).ok_or_else(|| fail("missing url".to_string()))?;
        check_artifact_url(file_url).map_err(|_| fail(format!("artifact URL is not credential-free HTTPS: {filename}")))?;
        let digest = item
            .get("digests")
            .and_then(|d| d.get("sha256"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| fail(format!("invalid SHA-256 metadata for {filename}")))?;
        if !is_lower_hex64(digest) {
            return Err(fail(format!("invalid SHA-256 metadata for {filename}")));
        }
        let yanked_val = item.get("yanked");
        let yanked = match yanked_val {
            None | Some(JVal::Bool(false)) => false,
            Some(JVal::Str(s)) if s.is_empty() => false,
            Some(_) => true,
        };
        if yanked && allow_yanked_reason.is_none() {
            continue;
        }
        let file_rp = match item.get("requires_python") {
            None | Some(JVal::Null) => None,
            Some(v) => match v.as_str() {
                Some(s) => {
                    let _: pep440_rs::VersionSpecifiers =
                        s.parse().map_err(|_| fail(format!("invalid Requires-Python metadata for {filename}")))?;
                    Some(s.to_string())
                }
                None => return Err(fail(format!("invalid Requires-Python metadata for {filename}"))),
            },
        };
        let packagetype = item
            .get("packagetype")
            .and_then(|v| v.as_str())
            .ok_or_else(|| fail("missing packagetype".to_string()))?;
        let artifact = ReleaseFile {
            filename: filename.to_string(),
            url: file_url.to_string(),
            sha256: digest.to_string(),
            packagetype: packagetype.to_string(),
            yanked,
            requires_python: file_rp,
        };
        if packagetype == "bdist_wheel" {
            let identity = WheelIdentity::parse(filename)
                .map_err(|e| fail(format!("invalid wheel filename in PyPI metadata: {filename}: {e}")))?;
            if identity.package != *package {
                return Err(fail(format!("wheel filename does not match PyPI release identity: {filename}")));
            }
            wheels.push(artifact);
        } else if packagetype == "sdist" {
            sdists.push(artifact);
        }
    }
    Ok(Release { package: package.clone(), requires_python, wheels, sdists })
}

fn check_artifact_url(url: &str) -> std::result::Result<(), ()> {
    let parsed: stuff_url::Url = url.parse().map_err(|_| ())?;
    if parsed.scheme() != "https" {
        return Err(());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(());
    }
    Ok(())
}

fn spec_contains(spec_text: &str, version: &pep440_rs::Version) -> bool {
    if spec_text.trim().is_empty() {
        return true;
    }
    match spec_text.parse::<pep440_rs::VersionSpecifiers>() {
        Ok(s) => s.contains(version),
        Err(_) => false,
    }
}

// --- HTTP fetching with redirect/SSRF/size-limit semantics ---

const MAX_REDIRECTS: usize = 5;
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;

fn http_client() -> h12tiny_client_sync::Client {
    h12tiny_client_sync::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
}

fn check_url_security(raw: &str) -> Result<stuff_url::Url> {
    let parsed: stuff_url::Url = raw.parse().map_err(|_| {
        Error::integrity("artifact download requires HTTPS", Stage::Acquisition)
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::integrity("artifact redirect must remain credential-free HTTPS", Stage::Acquisition));
    }
    match parsed.scheme() {
        "https" => Ok(parsed),
        _ => Err(Error::integrity("artifact download requires HTTPS", Stage::Acquisition)),
    }
}

fn resolve_url(base: &str, location: &str) -> Result<String> {
    if location.contains("://") {
        return Ok(location.to_string());
    }
    let base_url: stuff_url::Url = base
        .parse()
        .map_err(|_| Error::network(format!("invalid redirect base: {base}"), Stage::Acquisition))?;
    base_url.join(location).map(|u| u.to_string()).map_err(|_| {
        Error::integrity(
            format!("invalid redirect target: {location} (from {base})"),
            Stage::Acquisition,
        )
    })
}

struct Fetched {
    final_url: String,
    body: Vec<u8>,
}

fn http_get(url: &str, accept: &str, max_bytes: u64, timeout: Duration) -> Result<Fetched> {
    let client = http_client();
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        check_url_security(&current)?;
        let req = h12tiny_client_sync::http::Request::builder()
            .method(h12tiny_client_sync::http::Method::GET)
            .uri(current.clone())
            .header("user-agent", "mypypi/0.1.0")
            .header("accept", accept)
            .body(Vec::new())
            .map_err(|e| Error::network(format!("could not read PyPI metadata: {e}"), Stage::Acquisition))?;
        let mut resp = client
            .request_with_timeout(req, Some(timeout))
            .map_err(|e| Error::network(format!("could not download: {e}"), Stage::Acquisition))?;
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(h12tiny_client_sync::http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| Error::integrity("redirect without Location", Stage::Acquisition))?
                .to_string();
            current = resolve_url(&current, &loc)?;
            // credential-free HTTPS enforced at top of next loop iteration
            continue;
        }
        if status.as_u16() == 404 {
            return Err(Error::missing_artifact(format!("not found: {current}"), Stage::Acquisition));
        }
        if !status.is_success() {
            return Err(Error::network(
                format!("failed to fetch {current}: HTTP {}", status.as_u16()),
                Stage::Acquisition,
            ));
        }
        let mut body = Vec::new();
        {
            let reader = resp.body_mut();
            let _ = reader.set_read_timeout(Some(timeout));
            let mut buf = [0u8; 65536];
            loop {
                let n = reader.read(&mut buf).map_err(|e| {
                    Error::network(format!("stream read error: {e}"), Stage::Acquisition)
                })?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
                if body.len() as u64 > max_bytes {
                    return Err(Error::limit("response exceeds size limit", Stage::Acquisition));
                }
            }
        }
        check_url_security(&current)?;
        return Ok(Fetched { final_url: current, body });
    }
    Err(Error::network(format!("too many redirects fetching {url}"), Stage::Acquisition))
}

pub fn is_not_found(e: &Error) -> bool {
    matches!(e.kind, Kind::MissingArtifact) && e.detail.starts_with("not found: ")
}

pub fn fetch_release(package: &PackageVersion, allow_yanked_reason: Option<&str>) -> Result<Release> {
    fetch_release_with_timeout(package, allow_yanked_reason, 3600.0)
}

pub fn fetch_release_with_timeout(
    package: &PackageVersion,
    allow_yanked_reason: Option<&str>,
    retry_timeout_secs: f64,
) -> Result<Release> {
    let url = format!("https://pypi.org/pypi/{}/{}/json", package.name, package.version);
    let start = std::time::Instant::now();
    let mut delay = 1.0f64;
    loop {
        match http_get(&url, "application/json", MAX_JSON_BYTES as u64, Duration::from_secs(60)) {
            Ok(fetched) => {
                let text = String::from_utf8(fetched.body).map_err(|e| {
                    Error::missing_artifact(
                        format!("could not read PyPI metadata for {package}: {e}"),
                        Stage::Acquisition,
                    )
                    .with_package(package)
                })?;
                return release_from_metadata(package, &text, allow_yanked_reason);
            }
            Err(e) if is_not_found(&e) => {
                let elapsed = start.elapsed().as_secs_f64();
                if elapsed >= retry_timeout_secs {
                    return Err(Error::missing_artifact(
                        format!("could not read PyPI metadata for {package}"),
                        Stage::Acquisition,
                    )
                    .with_package(package.clone()));
                }
                let wait = delay.min(retry_timeout_secs - elapsed);
                std::thread::sleep(Duration::from_secs_f64(wait));
                delay = (delay * 2.0).min(60.0);
            }
            Err(mut e) => {
                if e.package.is_none() {
                    e.package = Some(package.to_string());
                }
                return Err(e);
            }
        }
    }
}

pub fn compatible_wheels(
    release: &Release,
    target: &Target,
    linux_repair_policy: &str,
) -> Result<Vec<ReleaseFile>> {
    let execution = target_python_version(target);
    if !spec_contains(&release.requires_python, &execution) {
        return Ok(Vec::new());
    }
    let preference = crate::platforms::tag_preference(target, linux_repair_policy).map_err(|e| {
        let mut err = Error::incompatible(e.detail, Stage::Acquisition);
        err.package = Some(release.package.to_string());
        err.target = Some(format!("Python {} on {}", target.python_version, target.platform));
        err
    })?;
    let mut ranked: Vec<(usize, String, ReleaseFile)> = Vec::new();
    for wheel in &release.wheels {
        if let Some(rp) = &wheel.requires_python
            && !spec_contains(rp, &execution)
        {
            continue;
        }
        let identity = match WheelIdentity::parse(&wheel.filename) {
            Ok(i) => i,
            Err(_) => continue,
        };
        if identity.package != release.package {
            continue;
        }
        if let Some(rank) = wheel_rank(&wheel.filename, &preference) {
            ranked.push((rank, wheel.filename.clone(), wheel.clone()));
        }
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(ranked.into_iter().map(|(_, _, w)| w).collect())
}

// --- Wheel verification ---

pub fn safe_member_ok(filename: &str) -> bool {
    if filename.is_empty() || filename.contains('\\') || filename.contains('\0') {
        return false;
    }
    if filename.starts_with('/') {
        return false;
    }
    let parts: Vec<&str> = filename.rstrip_slash().split('/').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return false;
    }
    if parts[0].contains(':') {
        return false;
    }
    true
}

trait RStrip {
    fn rstrip_slash(&self) -> &str;
}

impl RStrip for str {
    fn rstrip_slash(&self) -> &str {
        self.strip_suffix('/').unwrap_or(self)
    }
}

pub fn validate_wheel_architectures(path: &Path, target: &Target) -> std::result::Result<(), CompatibilityRejection> {
    validate_wheel_architectures_at(path, target, Stage::Acquisition).map_err(|e| match e {
        ArchError::Rejection(r) => r,
        ArchError::Integrity(detail) => CompatibilityRejection {
            category: CompatibilityCategory::Architecture,
            detail,
            package: package_of_filename(path),
            target: Some(target.clone()),
        },
    })
}

enum ArchError {
    Rejection(CompatibilityRejection),
    Integrity(String),
}

fn package_of_filename(path: &Path) -> Option<PackageVersion> {
    let name = path.file_name()?.to_str()?;
    WheelIdentity::parse(name).ok().map(|w| w.package)
}

fn validate_wheel_architectures_at(
    path: &Path,
    target: &Target,
    stage: Stage,
) -> std::result::Result<(), ArchError> {
    let _ = stage;
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let package = package_of_filename(path);
    let expected = crate::platforms::expected_architectures(filename).map_err(|e| ArchError::Integrity(e.detail))?;
    let actual_target = target.platform.rsplit('-').next().unwrap_or(&target.platform);
    // map arm64<->aarch64 equivalence for target arch naming
    let arch_matches = |arch: &str| arch == actual_target || (actual_target == "arm64" && arch == "aarch64") || (actual_target == "aarch64" && arch == "arm64");
    if !expected.is_empty() && !expected.iter().any(|a| arch_matches(a)) {
        let mut sorted: Vec<&String> = expected.iter().collect();
        sorted.sort();
        return Err(ArchError::Rejection(CompatibilityRejection {
            category: CompatibilityCategory::WheelTag,
            detail: format!(
                "wheel tag architectures {:?} do not include target {actual_target}",
                sorted.iter().map(|s| s.as_str()).collect::<Vec<_>>()
            ),
            package: package.clone(),
            target: Some(target.clone()),
        }));
    }
    let file = std::fs::File::open(path).map_err(|e| ArchError::Integrity(format!("invalid or incompatible wheel {filename}: {e}")))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| ArchError::Integrity(format!("invalid or incompatible wheel {filename}: {e}")))?;
    let names = safe_member_names(&mut archive, filename).map_err(ArchError::Integrity)?;
    for name in &names {
        if name.ends_with('/') {
            continue;
        }
        let is_native_hint = is_native_member_name(name);
        let mut content = Vec::new();
        {
            let member = archive.by_name(name).map_err(|e| ArchError::Integrity(format!("invalid or incompatible wheel {filename}: {e}")))?;
            member.take(1024 * 1024).read_to_end(&mut content).map_err(|e| ArchError::Integrity(format!("invalid or incompatible wheel {filename}: {e}")))?;
        }
        if !is_native_hint && content.starts_with(b"#!") {
            continue;
        }
        if content.starts_with(b"#!") && !is_native_hint {
            continue;
        }
        let arches = native_architectures_from_bytes(&content)
            .map_err(|e| ArchError::Integrity(format!("malformed native artifact {name}: {e}")))?;
        let native = is_native_hint || !arches.is_empty();
        if arches.is_empty() {
            if !native {
                continue;
            }
            return Err(ArchError::Integrity(format!("could not inspect native artifact {name}")));
        }
        let expected_for_file: BTreeSet<String> = if expected.is_empty() {
            [actual_target.to_string()].into_iter().collect()
        } else {
            expected.clone()
        };
        // normalize arm64/aarch64 equivalence
        let covers = expected_for_file.iter().all(|need| {
            arches.iter().any(|have| have == need || (need == "arm64" && have == "aarch64") || (need == "aarch64" && have == "arm64"))
        });
        if !covers {
            let mut have: Vec<&String> = arches.iter().collect();
            have.sort();
            let mut need: Vec<&String> = expected_for_file.iter().collect();
            need.sort();
            return Err(ArchError::Rejection(CompatibilityRejection {
                category: CompatibilityCategory::Architecture,
                detail: format!(
                    "{name} has architectures {:?}, expected {:?}",
                    have.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    need.iter().map(|s| s.as_str()).collect::<Vec<_>>()
                ),
                package: package.clone(),
                target: Some(target.clone()),
            }));
        }
    }
    Ok(())
}

fn is_native_member_name(name: &str) -> bool {
    let file_name = name.rsplit('/').next().unwrap_or(name);
    [".so", ".dylib", ".pyd", ".dll", ".exe"].iter().any(|suffix| name.ends_with(suffix))
        || file_name.rsplit_once(".so.").is_some_and(|(_, version)| {
            version.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn safe_member_names(
    archive: &mut zip::ZipArchive<std::fs::File>,
    filename: &str,
) -> std::result::Result<Vec<String>, String> {
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i).map_err(|e| format!("invalid or incompatible wheel {filename}: {e}"))?;
        names.push(file.name().map_err(|e| format!("invalid or incompatible wheel {filename}: {e}"))?.into_owned());
    }
    let res = {
        let mut seen = BTreeSet::new();
        names.iter().any(|n| !seen.insert(n.clone()))
    }; if res {
        return Err(format!("invalid or incompatible wheel {filename}: wheel archive contains duplicate paths"));
    }
    for name in &names {
        if !safe_member_ok(name) {
            return Err(format!("invalid or incompatible wheel {filename}: unsafe wheel archive member: {name}"));
        }
    }
    Ok(names)
}

fn parse_email_headers(bytes: &[u8]) -> BTreeMap<String, Vec<String>> {
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
            let key = line[..colon].trim().to_string();
            let val = line[colon + 1..].trim().to_string();
            map.entry(key.clone()).or_default().push(val);
            current = Some(key);
        }
    }
    // normalize keys case-insensitively by lowercasing lookup helper below
    let mut norm: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, v) in map {
        norm.entry(k.to_lowercase()).or_default().extend(v);
    }
    norm
}

fn headers_get<'a>(map: &'a BTreeMap<String, Vec<String>>, key: &str) -> &'a [String] {
    map.get(&key.to_lowercase()).map(|v| v.as_slice()).unwrap_or(&[])
}

fn headers_one(map: &BTreeMap<String, Vec<String>>, key: &str) -> Option<String> {
    let v = headers_get(map, key);
    if v.len() == 1 { Some(v[0].clone()) } else { None }
}

pub fn validate_wheel_metadata(
    path: &Path,
    package: &PackageVersion,
    target: &Target,
    linux_repair_policy: &str,
    enforce_target_tags: bool,
) -> std::result::Result<WheelIdentity, CompatibilityRejection> {
    validate_wheel_metadata_at(path, package, target, linux_repair_policy, enforce_target_tags, Stage::Acquisition)
}

pub fn validate_wheel_metadata_at(
    path: &Path,
    package: &PackageVersion,
    target: &Target,
    linux_repair_policy: &str,
    enforce_target_tags: bool,
    _stage: Stage,
) -> std::result::Result<WheelIdentity, CompatibilityRejection> {
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let rej = |category: CompatibilityCategory, detail: String| CompatibilityRejection {
        category,
        detail,
        package: Some(package.clone()),
        target: Some(target.clone()),
    };
    let integ = |detail: String| CompatibilityRejection {
        category: CompatibilityCategory::WheelTag,
        detail,
        package: Some(package.clone()),
        target: Some(target.clone()),
    };
    let identity = WheelIdentity::parse(&filename).map_err(|e| integ(format!("invalid wheel filename {filename}: {}", e.detail)))?;
    if identity.package != *package {
        return Err(integ(format!("wheel filename {filename} does not match {package}")));
    }
    let file = std::fs::File::open(path).map_err(|e| integ(format!("invalid wheel metadata in {filename}: {e}")))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| integ(format!("invalid wheel metadata in {filename}: {e}")))?;
    let names = safe_member_names(&mut archive, &filename).map_err(integ)?;
    let name_set: BTreeSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let dist_infos: BTreeSet<&str> = name_set
        .iter()
        .map(|n| n.split('/').next().unwrap_or(""))
        .filter(|d| d.ends_with(".dist-info"))
        .collect();
    if dist_infos.len() != 1 {
        return Err(integ("wheel must contain exactly one .dist-info directory".to_string()));
    }
    let dist_info = dist_infos.iter().next().unwrap().to_string();
    if !crate::wheel_metadata::matches_dist_info(&identity.package.name, &identity.package.version, &dist_info) {
        return Err(integ("wheel .dist-info directory does not match its filename".to_string()));
    }
    let metadata_path = format!("{dist_info}/METADATA");
    let wheel_path = format!("{dist_info}/WHEEL");
    let record_path = format!("{dist_info}/RECORD");
    if !(name_set.contains(metadata_path.as_str()) && name_set.contains(wheel_path.as_str()) && name_set.contains(record_path.as_str())) {
        return Err(integ("wheel requires METADATA, WHEEL, and RECORD".to_string()));
    }
    let mut read_member = |name: &str| -> std::result::Result<Vec<u8>, CompatibilityRejection> {
        let mut buf = Vec::new();
        archive.by_name(name).map_err(|e| integ(format!("invalid wheel metadata in {filename}: {e}")))?.read_to_end(&mut buf).map_err(|e| integ(format!("invalid wheel metadata in {filename}: {e}")))?;
        Ok(buf)
    };
    let metadata_bytes = read_member(&metadata_path)?;
    let wheel_bytes = read_member(&wheel_path)?;
    let metadata = parse_email_headers(&metadata_bytes);
    let wheel_headers = parse_email_headers(&wheel_bytes);
    let meta_name = headers_one(&metadata, "Name").ok_or_else(|| integ("wheel has invalid package metadata".to_string()))?;
    let meta_version = headers_one(&metadata, "Version").ok_or_else(|| integ("wheel has invalid package metadata".to_string()))?;
    let meta_pkg =
        crate::wheel_metadata::metadata_identity(&meta_name, &meta_version).map_err(integ)?;
    if meta_pkg != *package {
        return Err(integ("wheel filename and METADATA identity differ".to_string()));
    }
    let requires_python = headers_get(&metadata, "Requires-Python");
    let requires_python_text = match requires_python.len() {
        0 => String::new(),
        1 => requires_python[0].clone(),
        _ => return Err(integ("wheel metadata contains an invalid Requires-Python".to_string())),
    };
    if !requires_python_text.is_empty() && requires_python_text.parse::<pep440_rs::VersionSpecifiers>().is_err() {
        return Err(integ("wheel metadata contains an invalid Requires-Python".to_string()));
    }
    let wheel_tags: BTreeSet<String> = headers_get(&wheel_headers, "Tag")
        .iter()
        .flat_map(|v| v.split_whitespace().map(|s| s.to_string()))
        .collect();
    let filename_tags: BTreeSet<String> = wheel_tag_triples(&filename).map_err(|e| integ(e.detail))?.iter().map(|t| t.as_string()).collect();
    if wheel_tags.is_empty() || wheel_tags != filename_tags {
        return Err(integ("wheel WHEEL tags do not match its filename".to_string()));
    }
    let execution = target_python_version(target);
    if !spec_contains(&requires_python_text, &execution) {
        return Err(rej(
            CompatibilityCategory::PythonVersion,
            format!("wheel Requires-Python excludes CPython {execution}"),
        ));
    }
    if enforce_target_tags {
        let preference = crate::platforms::tag_preference(target, linux_repair_policy)
            .map_err(|e| integ(e.detail))?;
        if wheel_rank(&filename, &preference).is_none() {
            return Err(rej(
                CompatibilityCategory::WheelTag,
                format!("wheel tags are incompatible with target {}", target.platform),
            ));
        }
    }
    Ok(identity)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| Error::io(format!("could not read wheel {path}: {e}", path = path.display()), Stage::Command))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(format!("could not read wheel: {e}"), Stage::Command))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::lower_hex(h.finalize()))
}

pub fn download_artifact(artifact: &ReleaseFile, destination: &Path) -> Result<PathBuf> {
    check_url_security(&artifact.url)
        .map_err(|_| Error::integrity("artifact download requires HTTPS", Stage::Acquisition))?;
    if artifact.filename.is_empty()
        || artifact.filename == "."
        || artifact.filename == ".."
        || Path::new(&artifact.filename).file_name().and_then(|s| s.to_str()) != Some(&artifact.filename)
        || artifact.filename.contains('/')
        || artifact.filename.contains('\\')
        || artifact.filename.contains('\0')
    {
        return Err(Error::integrity(format!("unsafe artifact filename: {:?}", artifact.filename), Stage::Acquisition));
    }
    let destination = std::path::absolute(destination)
        .map_err(|e| Error::io(format!("could not create artifact directory: {e}"), Stage::Acquisition))?;
    std::fs::create_dir_all(&destination)
        .map_err(|e| Error::io(format!("could not create artifact directory {}: {e}", destination.display()), Stage::Acquisition))?;
    assert_real_dir(&destination)?;
    let path = destination.join(&artifact.filename);
    if let Some(existing) = existing_artifact(&path, artifact)? {
        return Ok(existing);
    }
    let fetched = http_get(&artifact.url, "*/*", MAX_DOWNLOAD_BYTES, Duration::from_secs(60)).map_err(|e| {
        if matches!(e.kind, Kind::Integrity | Kind::Limit | Kind::Network) {
            e
        } else {
            Error::missing_artifact(format!("could not download {}", artifact.filename), Stage::Acquisition)
        }
    })?;
    // verify redirect stayed credential-free HTTPS (http_get enforces per-hop)
    let _ = &fetched.final_url;
    let mut h = Sha256::new();
    h.update(&fetched.body);
    let digest = crate::lower_hex(h.finalize());
    if digest != artifact.sha256 {
        return Err(Error::integrity(format!("SHA-256 mismatch for {}", artifact.filename), Stage::Acquisition));
    }
    // atomic write via temp file + link-or-rename
    let tmp_name = format!(".{}.{:x}.part", artifact.filename, rand_u64());
    let tmp = destination.join(tmp_name);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| Error::io(format!("could not store artifact: {e}"), Stage::Acquisition))?;
        f.write_all(&fetched.body)
            .map_err(|e| Error::io(format!("could not store artifact: {e}"), Stage::Acquisition))?;
        f.sync_all()
            .map_err(|e| Error::io(format!("could not store artifact: {e}"), Stage::Acquisition))?;
    }
    match std::fs::hard_link(&tmp, &path) {
        Ok(()) => {}
        Err(_) => {
            if let Some(existing) = existing_artifact(&path, artifact)? {
                let _ = std::fs::remove_file(&tmp);
                return Ok(existing);
            }
            return Err(Error::conflict(
                format!("artifact destination changed during download: {}", path.display()),
                Stage::Acquisition,
            ));
        }
    }
    sync_parent(&destination)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(path)
}

fn rand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    nanos as u64 ^ (std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15)
}

fn assert_real_dir(path: &Path) -> Result<()> {
    let md = std::fs::symlink_metadata(path)
        .map_err(|e| Error::io(format!("could not inspect directory {}: {e}", path.display()), Stage::Acquisition))?;
    if md.file_type().is_symlink() || !md.file_type().is_dir() {
        return Err(Error::invalid_input(
            format!("artifact destination must be a real directory: {}", path.display()),
            Stage::Acquisition,
        ));
    }
    Ok(())
}

fn sync_parent(dir: &Path) -> Result<()> {
    let f = std::fs::File::open(dir)
        .map_err(|e| Error::io(format!("could not make download durable: {e}"), Stage::Acquisition))?;
    f.sync_all()
        .map_err(|e| Error::io(format!("could not make download durable: {e}"), Stage::Acquisition))?;
    Ok(())
}

fn existing_artifact(path: &Path, artifact: &ReleaseFile) -> Result<Option<PathBuf>> {
    let md = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::io(
                format!("could not inspect artifact destination {}: {e}", path.display()),
                Stage::Acquisition,
            ));
        }
    };
    if !md.file_type().is_file() {
        return Err(Error::conflict(
            format!("artifact destination is not a regular file: {}", path.display()),
            Stage::Acquisition,
        ));
    }
    let actual = sha256_file_raw(path).map_err(|e| {
        Error::io(format!("could not read existing artifact {}: {e}", path.display()), Stage::Acquisition)
    })?;
    if actual != artifact.sha256 {
        return Err(Error::conflict(
            format!("artifact destination contains conflicting bytes: {}", path.display()),
            Stage::Acquisition,
        ));
    }
    Ok(Some(path.to_path_buf()))
}

fn sha256_file_raw(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::lower_hex(h.finalize()))
}

fn promote_wheel(staged: &Path, destination: &Path, artifact: &ReleaseFile, package: &PackageVersion, target: &Target) -> Result<PathBuf> {
    let final_path = destination.join(&artifact.filename);
    if let Some(existing) = existing_artifact(&final_path, artifact)? {
        return Ok(existing);
    }
    match std::fs::hard_link(staged, &final_path) {
        Ok(()) => {}
        Err(_) => {
            if let Some(existing) = existing_artifact(&final_path, artifact)? {
                return Ok(existing);
            }
            let mut e = Error::conflict(
                format!("artifact destination changed during promotion: {}", final_path.display()),
                Stage::Acquisition,
            );
            e = e.with_package(package).with_target(&target.python_version.to_string(), &target.platform);
            return Err(e);
        }
    }
    sync_parent(destination)?;
    Ok(final_path)
}

#[allow(clippy::too_many_arguments)]
pub fn select_or_download(
    release: &Release,
    target: &Target,
    destination: &Path,
    linux_repair_policy: &str,
    allow_yanked_reason: Option<&str>,
) -> std::result::Result<Option<PathBuf>, CompatibilityRejection> {
    let execution = target_python_version(target);
    if !spec_contains(&release.requires_python, &execution) {
        return Err(CompatibilityRejection {
            category: CompatibilityCategory::PythonVersion,
            detail: format!("release Requires-Python excludes CPython {execution}"),
            package: Some(release.package.clone()),
            target: Some(target.clone()),
        });
    }
    let candidates = compatible_wheels(release, target, linux_repair_policy).map_err(|e| CompatibilityRejection {
        category: CompatibilityCategory::WheelTag,
        detail: e.detail,
        package: Some(release.package.clone()),
        target: Some(target.clone()),
    })?;
    let mut by_name: BTreeMap<&str, &ReleaseFile> = BTreeMap::new();
    for artifact in &release.wheels {
        if let Some(prev) = by_name.get(artifact.filename.as_str())
            && prev.sha256 != artifact.sha256 {
                let e = Error::conflict(
                    format!("release metadata assigns conflicting bytes to {}", artifact.filename),
                    Stage::Acquisition,
                )
                .with_package(&release.package)
                .with_target(&target.python_version.to_string(), &target.platform);
                return Err(CompatibilityRejection {
                    category: CompatibilityCategory::WheelTag,
                    detail: e.detail.clone(),
                    package: Some(release.package.clone()),
                    target: Some(target.clone()),
                });
            }
        by_name.insert(artifact.filename.as_str(), artifact);
    }
    let destination = std::path::absolute(destination).map_err(|e| CompatibilityRejection {
        category: CompatibilityCategory::WheelTag,
        detail: format!("could not create artifact directory {e}"),
        package: Some(release.package.clone()),
        target: Some(target.clone()),
    })?;
    std::fs::create_dir_all(&destination).map_err(|e| CompatibilityRejection {
        category: CompatibilityCategory::WheelTag,
        detail: format!("could not create artifact directory {}: {e}", destination.display()),
        package: Some(release.package.clone()),
        target: Some(target.clone()),
    })?;
    assert_real_dir(&destination).map_err(|e| CompatibilityRejection {
        category: CompatibilityCategory::WheelTag,
        detail: e.detail.clone(),
        package: Some(release.package.clone()),
        target: Some(target.clone()),
    })?;
    let mut last_rejection: Option<CompatibilityRejection> = None;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for candidate in &candidates {
        if !seen.insert(candidate.filename.as_str()) {
            continue;
        }
        let artifact = by_name[candidate.filename.as_str()];
        if artifact.yanked && allow_yanked_reason.is_none() {
            continue;
        }
        match existing_artifact(&destination.join(&artifact.filename), artifact) {
            Ok(Some(existing)) => {
                match validate_wheel_metadata(&existing, &release.package, target, linux_repair_policy, true)
                    .and_then(|_| validate_wheel_architectures(&existing, target))
                {
                    Ok(()) => return Ok(Some(existing)),
                    Err(r) => {
                        last_rejection = Some(r);
                        continue;
                    }
                }
            }
            Ok(None) => {}
            Err(_) => {
                last_rejection = Some(CompatibilityRejection {
                    category: CompatibilityCategory::WheelTag,
                    detail: format!("artifact destination contains conflicting bytes: {}", artifact.filename),
                    package: Some(release.package.clone()),
                    target: Some(target.clone()),
                });
                continue;
            }
        }
        let stage_dir = destination.join(format!(".mypypi-acquire-{:x}", rand_u64()));
        std::fs::create_dir(&stage_dir).map_err(|e| CompatibilityRejection {
            category: CompatibilityCategory::WheelTag,
            detail: format!("could not create staging directory: {e}"),
            package: Some(release.package.clone()),
            target: Some(target.clone()),
        })?;
        enum Step {
            Done(PathBuf),
            Reject(CompatibilityRejection),
            Fatal(Error),
        }
        let step: Step = (|| {
            let downloaded = match download_artifact(artifact, &stage_dir) {
                Ok(p) => p,
                Err(e) => return Step::Fatal(e),
            };
            if let Err(r) = validate_wheel_metadata(&downloaded, &release.package, target, linux_repair_policy, true) {
                return Step::Reject(r);
            }
            if let Err(r) = validate_wheel_architectures(&downloaded, target) {
                return Step::Reject(r);
            }
            match promote_wheel(&downloaded, &destination, artifact, &release.package, target) {
                Ok(p) => Step::Done(p),
                Err(e) => Step::Fatal(e),
            }
        })();
        let _ = std::fs::remove_dir_all(&stage_dir);
        match step {
            Step::Done(p) => return Ok(Some(p)),
            Step::Reject(r) => {
                last_rejection = Some(r);
            }
            Step::Fatal(mut e) => {
                // real (non-compatibility) failure: attach context and surface
                if e.package.is_none() {
                    e = e.with_package(&release.package);
                }
                if e.target.is_none() {
                    e = e.with_target(&target.python_version.to_string(), &target.platform);
                }
                return Err(CompatibilityRejection {
                    category: CompatibilityCategory::WheelTag,
                    detail: format!("ACQUISITION-ERROR: {}: {}", e.stage.as_str(), e.detail),
                    package: Some(release.package.clone()),
                    target: Some(target.clone()),
                });
            }
        }
    }
    if let Some(r) = last_rejection {
        return Err(r);
    }
    if !release.wheels.is_empty() && candidates.is_empty() {
        return Err(CompatibilityRejection {
            category: CompatibilityCategory::WheelTag,
            detail: format!("no release wheel is compatible with target {}", target.platform),
            package: Some(release.package.clone()),
            target: Some(target.clone()),
        });
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYPI_JSON: &str = r#"{"info": {"name": "sample-pkg", "version": "1.0", "requires_python": ">=3.9"}, "urls": [{"filename": "sample_pkg-1.0-py3-none-any.whl", "url": "https://files.example.test/sample_pkg-1.0-py3-none-any.whl", "digests": {"sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}, "packagetype": "bdist_wheel", "yanked": false, "requires_python": null}, {"filename": "sample-pkg-1.0.tar.gz", "url": "https://files.example.test/sample-pkg-1.0.tar.gz", "digests": {"sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}, "packagetype": "sdist", "yanked": false, "requires_python": null}]}"#;

    fn sample_package() -> PackageVersion {
        PackageVersion::parse("sample-pkg", "1.0").unwrap()
    }

    #[test]
    fn release_metadata_is_parsed_matched_to_identity_and_filtered_by_target() {
        let r = release_from_metadata(&sample_package(), PYPI_JSON, None).unwrap();
        assert_eq!(r.wheels.len(), 1);
        assert_eq!(r.sdists.len(), 1);
        assert_eq!(r.requires_python, ">=3.9");
        let other = PackageVersion::parse("sample-pkg", "2.0").unwrap();
        assert!(release_from_metadata(&other, PYPI_JSON, None).is_err());
        let t = Target::new("3.15", "linux-x86_64").unwrap();
        assert_eq!(compatible_wheels(&r, &t, "manylinux_2_28").unwrap().len(), 1);
    }

    #[test]
    fn validates_fixture_wheel_metadata() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/mypypi/wheels/validation_fixture-1.0-py3-none-any.whl");
        let pkg = PackageVersion::parse("validation-fixture", "1.0").unwrap();
        let t = Target::new("3.15", "macos-arm64").unwrap();
        let id = validate_wheel_metadata(&path, &pkg, &t, "manylinux_2_28", true).unwrap();
        assert_eq!(id.filename, "validation_fixture-1.0-py3-none-any.whl");
        validate_wheel_architectures(&path, &t).unwrap();
    }

    #[test]
    fn accepts_noncanonical_metadata_name() {
        // Real-world wheels keep the project's original capitalization in
        // METADATA's Name field (e.g. Pygments) while filenames are
        // normalized; identity comparison must normalize, not reject.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pkg-1.0-py3-none-any.whl");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("pkg-1.0.dist-info/METADATA", opts).unwrap();
            use std::io::Write;
            zip.write_all(b"Metadata-Version: 2.1\nName: Pkg\nVersion: 1.0\nRequires-Python: >=3.9\n")
                .unwrap();
            zip.start_file("pkg-1.0.dist-info/WHEEL", opts).unwrap();
            zip.write_all(b"Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n")
                .unwrap();
            zip.start_file("pkg-1.0.dist-info/RECORD", opts).unwrap();
            zip.write_all(b"").unwrap();
            zip.finish().unwrap();
        }
        let pkg = PackageVersion::parse("pkg", "1.0").unwrap();
        let t = Target::new("3.15", "macos-arm64").unwrap();
        let id = validate_wheel_metadata(&path, &pkg, &t, "manylinux_2_28", true).unwrap();
        assert_eq!(id.filename, "pkg-1.0-py3-none-any.whl");
    }

    #[test]
    fn dsym_bundle_members_are_not_native_hints() {
        assert!(is_native_member_name("pkg/_ext.cpython-315-darwin.so"));
        assert!(is_native_member_name("pkg/libfoo.so.1"));
        assert!(!is_native_member_name("PyObjCTest/NULL.cpython-314-darwin.so.dSYM/Contents/Info.plist"));
        assert!(!is_native_member_name("PyObjCTest/x.so.dSYM/Contents/Resources/Relocations/aarch64/x.so.yml"));
    }
}
