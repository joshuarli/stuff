#![forbid(unsafe_code)]

use crate::archive::inspect_package_archive;
use crate::config::{Config, Target};
use crate::error::{Error, Result};
use crate::fetch_budget::FetchByteBudget;
use crate::inventory::{
    record_verified_artifact, validate_artifact_record, ArtifactRecord, Inventory, PackageRecord,
};
use crate::inventory_store::{load_inventory, update_inventory};
use crate::limits;
use h12tiny_client_sync::{http, Client};
use sha2::{Digest, Sha256, Sha384, Sha512};
use sha1::Sha1;
use std::collections::{BTreeMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{IpAddr, ToSocketAddrs};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const USER_AGENT: &str = "mynpm/0.1.0";
const REDIRECT_STATUSES: [u16; 5] = [301, 302, 303, 307, 308];

pub fn is_restricted_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let n = u32::from(v4);
            const SUBNETS: &[(u32, u32)] = &[
                (0x00000000, 8),
                (0x0A000000, 8),
                (0x64400000, 10),
                (0x7F000000, 8),
                (0xA9FE0000, 16),
                (0xAC100000, 12),
                (0xC0000000, 24),
                (0xC0000200, 24),
                (0xC0586300, 24),
                (0xC0A80000, 16),
                (0xC6120000, 15),
                (0xC6336400, 24),
                (0xCB007100, 24),
                (0xE0000000, 4),
                (0xF0000000, 4),
            ];
            SUBNETS.iter().any(|(base, prefix)| {
                let mask = if *prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
                (n & mask) == (base & mask)
            })
        }
        IpAddr::V6(v6) => {
            if v6.to_ipv4_mapped().is_some() {
                return true;
            }
            let n = u128::from(v6);
            const SUBNETS: &[(u128, u32)] = &[
                (0x00000000000000000000000000000000, 128),
                (0x00000000000000000000000000000001, 128),
                (0x0064FF9B000000000000000000000000, 96),
                (0x01000000000000000000000000000000, 64),
                (0x20010000000000000000000000000000, 23),
                (0x20010DB8000000000000000000000000, 32),
                (0x20010020000000000000000000000000, 28),
                (0xFC000000000000000000000000000000, 7),
                (0xFE800000000000000000000000000000, 10),
                (0xFF000000000000000000000000000000, 8),
            ];
            SUBNETS.iter().any(|(base, prefix)| {
                let mask = if *prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
                (n & mask) == (base & mask)
            })
        }
    }
}

pub fn resolve_public_host(hostname: &str) -> Result<Vec<IpAddr>> {
    let mut addrs: Vec<IpAddr> = (hostname, 443)
        .to_socket_addrs()
        .map_err(|_| Error::Operation(format!("could not resolve upstream host {hostname}")))?
        .map(|s| s.ip())
        .collect();
    addrs.sort();
    addrs.dedup();
    if addrs.is_empty() || addrs.iter().any(|ip| is_restricted_address(*ip)) {
        return Err(Error::Operation(format!(
            "upstream host {hostname} resolved to a non-public address"
        )));
    }
    Ok(addrs)
}

fn validate_source_url(raw: &str, expected_host: &str) -> Result<url::Url> {
    let url = url::Url::parse(raw)
        .map_err(|_| Error::Operation("upstream returned an invalid URL".to_string()))?;
    let host = url.host_str().unwrap_or("").to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    if url.scheme() != "https"
        || host != expected_host
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || host.parse::<IpAddr>().is_ok()
    {
        return Err(Error::Operation(format!(
            "upstream URL is outside the allowlisted HTTPS registry {expected_host}: got {raw}"
        )));
    }
    Ok(url)
}

pub struct FetchedResponse {
    pub status: u16,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

pub trait Exchange: Send + Sync {
    fn fetch(&self, url: &url::Url) -> Result<FetchedResponse>;
}

pub struct HttpsExchange {
    client: Client,
}

impl HttpsExchange {
    pub fn new() -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(limits::REQUEST_MILLISECONDS))
            .build();
        Self { client }
    }
}

impl Default for HttpsExchange {
    fn default() -> Self {
        Self::new()
    }
}

impl Exchange for HttpsExchange {
    fn fetch(&self, url: &url::Url) -> Result<FetchedResponse> {
        let host = url.host_str().unwrap_or("").to_string();
        let _ = resolve_public_host(&host)?;
        let timeout = Duration::from_millis(limits::REQUEST_MILLISECONDS);
        let req = http::Request::builder()
            .method(http::Method::GET)
            .uri(url.as_str())
            .header("accept", "application/json")
            .header("user-agent", USER_AGENT)
            .body(Vec::new())
            .map_err(|e| Error::Operation(format!("upstream request failed for {url}: {e}")))?;
        let mut resp = self
            .client
            .request_with_timeout(req, Some(timeout))
            .map_err(|e| Error::Operation(format!("upstream request failed for {url}: {e}")))?;
        let status = resp.status().as_u16();
        if REDIRECT_STATUSES.contains(&status) {
            let location = resp
                .headers()
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            return Ok(FetchedResponse { status, location, body: Vec::new() });
        }
        if status != 200 {
            return Err(Error::Operation(format!("upstream returned HTTP {status} for {url}")));
        }
        if let Some(raw) = resp.headers().get(http::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok())
            && (raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit())) {
                return Err(Error::Operation("upstream returned an invalid Content-Length".to_string()));
            }
        let mut body = Vec::new();
        {
            let reader = resp.body_mut();
            let _ = reader.set_read_timeout(Some(timeout));
            let mut buf = [0u8; 65536];
            loop {
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| Error::Operation(format!("upstream response failed for {url}: {e}")))?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
                if body.len() as u64 > limits::TARBALL_BYTES + 64 * 1024 * 1024 {
                    return Err(Error::Operation(format!(
                        "upstream response exceeds the {} limit",
                        limits::format_bytes(limits::TARBALL_BYTES)
                    )));
                }
            }
        }
        Ok(FetchedResponse { status, location: None, body })
    }
}

pub fn transfer(
    initial: &str,
    max_bytes: u64,
    expected_host: &str,
    exchange: &dyn Exchange,
    budget: &Mutex<FetchByteBudget>,
) -> Result<(Vec<u8>, String)> {
    let mut current = validate_source_url(initial, expected_host)?.to_string();
    for redirects in 0..=limits::REDIRECTS as usize {
        current = validate_source_url(&current, expected_host)?.to_string();
        let url = url::Url::parse(&current)
            .map_err(|_| Error::Operation("upstream returned an invalid URL".to_string()))?;
        let response = exchange.fetch(&url)?;
        if REDIRECT_STATUSES.contains(&response.status) {
            if redirects >= limits::REDIRECTS as usize {
                return Err(Error::Operation(format!(
                    "upstream exceeded the {} redirect limit",
                    limits::REDIRECTS
                )));
            }
            let location = response.location.filter(|s| !s.is_empty()).ok_or_else(|| {
                Error::Operation("upstream redirect omitted Location".to_string())
            })?;
            let next = url.join(&location).map_err(|_| {
                Error::Operation("upstream returned an invalid redirect URL".to_string())
            })?;
            current = next.to_string();
            continue;
        }
        if response.status != 200 {
            return Err(Error::Operation(format!(
                "upstream returned HTTP {} for {current}",
                response.status
            )));
        }
        if response.body.len() as u64 > max_bytes {
            return Err(Error::Operation(format!(
                "upstream response exceeds the {} limit",
                limits::format_bytes(max_bytes)
            )));
        }
        budget
            .lock()
            .map_err(|_| Error::Operation("fetch byte limit exceeded".to_string()))?
            .take(response.body.len() as u64)
            .map_err(|_| Error::Operation("fetch byte limit exceeded".to_string()))?;
        return Ok((response.body, current));
    }
    Err(Error::Operation(format!(
        "upstream exceeded the {} redirect limit",
        limits::REDIRECTS
    )))
}

pub fn package_route(name: &str) -> String {
    if !name.starts_with('@') {
        return percent_encode(name);
    }
    match name.find('/') {
        Some(i) => format!("{}%2f{}", &name[..i], percent_encode(&name[i + 1..])),
        None => percent_encode(name),
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn record_tarball(record: &ArtifactRecord) -> Option<String> {
    dist_tarball(&metadata_dist(&record.metadata))
}

pub fn parse_integrity_claims(text: Option<&str>, label: &str) -> Result<BTreeMap<String, String>> {
    let Some(text) = text else { return Ok(BTreeMap::new()) };
    if text.is_empty() {
        return Err(Error::Operation(format!("{label} is malformed")));
    }
    let mut claims = BTreeMap::new();
    for token in text.split_whitespace() {
        let claim = token.split('?').next().unwrap_or("");
        let Some(sep) = claim.find('-') else {
            return Err(Error::Operation(format!("{label} contains an unsupported integrity token")));
        };
        let (algo, digest) = (&claim[..sep], &claim[sep + 1..]);
        if !["sha1", "sha256", "sha384", "sha512"].contains(&algo)
            || digest.is_empty()
            || !digest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        {
            return Err(Error::Operation(format!("{label} contains an unsupported integrity token")));
        }
        let raw = base64_engine_decode(digest)
            .ok_or_else(|| Error::Operation(format!("{label} contains a malformed {algo} digest")))?;
        let expected_len = match algo {
            "sha1" => 20,
            "sha256" => 32,
            "sha384" => 48,
            "sha512" => 64,
            _ => unreachable!(),
        };
        if raw.len() != expected_len || base64_engine_encode(&raw) != digest {
            return Err(Error::Operation(format!("{label} contains a malformed {algo} digest")));
        }
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        if let Some(prior) = claims.get(algo)
            && prior != &hex {
                return Err(Error::Conflict(format!("{label} contains conflicting {algo} integrity claims")));
            }
        claims.insert(algo.to_string(), hex);
    }
    Ok(claims)
}

fn base64_engine_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn base64_engine_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn merge_claims(records: &[PackageRecord], metadata: &deser_value::Value) -> Result<(BTreeMap<String, String>, bool)> {
    let mut claims: BTreeMap<String, String> = BTreeMap::new();
    for record in records {
        let label = format!("lockfile integrity for {}@{}", record.name, record.version.clone().unwrap_or_default());
        for (algo, digest) in parse_integrity_claims(record.integrity.as_deref(), &label)? {
            if let Some(prior) = claims.get(&algo)
                && prior != &digest
            {
                return Err(Error::Conflict(format!(
                    "upstream {algo} integrity conflicts for {}@{}",
                    metadata_name(metadata),
                    metadata_version(metadata)
                )));
            }
            claims.insert(algo, digest);
        }
    }
    let dist = metadata_dist(metadata);
    let upstream = parse_integrity_claims(dist_integrity(&dist).as_deref(), "upstream metadata integrity")?;
    for (algo, digest) in upstream {
        if let Some(prior) = claims.get(&algo)
            && prior != &digest {
                return Err(Error::Conflict(format!(
                    "upstream {algo} integrity conflicts for {}@{}",
                    metadata_name(metadata),
                    metadata_version(metadata)
                )));
            }
        claims.insert(algo, digest);
    }
    if let Some(shasum) = dist_shasum(&dist) {
        if !crate::value::is_sha1_hex(&shasum) {
            return Err(Error::Operation("upstream shasum is malformed".to_string()));
        }
        if let Some(prior) = claims.get("sha1")
            && prior != &shasum
        {
            return Err(Error::Conflict(format!(
                "upstream SHA-1 integrity conflicts for {}@{}",
                metadata_name(metadata),
                metadata_version(metadata)
            )));
        }
        claims.insert("sha1".to_string(), shasum);
    }
    if claims.is_empty() {
        return Err(Error::Operation(format!(
            "upstream supplied no integrity claim for {}@{}",
            metadata_name(metadata),
            metadata_version(metadata)
        )));
    }
    let legacy = claims.keys().all(|a| a == "sha1");
    Ok((claims, legacy))
}

fn lock_claims(records: &[PackageRecord]) -> Result<BTreeMap<String, String>> {
    let mut claims = BTreeMap::new();
    for record in records {
        let label = format!("lockfile integrity for {}@{}", record.name, record.version.clone().unwrap_or_default());
        for (algo, digest) in parse_integrity_claims(record.integrity.as_deref(), &label)? {
            if let Some(prior) = claims.get(&algo)
                && prior != &digest
            {
                return Err(Error::Conflict(format!(
                    "lockfiles disagree about {algo} for {}@{}",
                    record.name,
                    record.version.clone().unwrap_or_default()
                )));
            }
            claims.insert(algo, digest);
        }
    }
    Ok(claims)
}

fn metadata_name(m: &deser_value::Value) -> String {
    m.as_map().and_then(|x| x.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn metadata_version(m: &deser_value::Value) -> String {
    m.as_map().and_then(|x| x.get("version")).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn metadata_dist(m: &deser_value::Value) -> Option<deser_value::Value> {
    m.as_map().and_then(|x| x.get("dist")).cloned()
}

fn dist_integrity(dist: &Option<deser_value::Value>) -> Option<String> {
    dist.as_ref()?.as_map()?.get("integrity")?.as_str().map(|s| s.to_string())
}

fn dist_shasum(dist: &Option<deser_value::Value>) -> Option<String> {
    dist.as_ref()?.as_map()?.get("shasum")?.as_str().map(|s| s.to_string())
}

fn dist_tarball(dist: &Option<deser_value::Value>) -> Option<String> {
    dist.as_ref()?.as_map()?.get("tarball")?.as_str().map(|s| s.to_string())
}

fn validate_platform_claims(records: &[PackageRecord], metadata: &deser_value::Value) -> Result<()> {
    let name = metadata_name(metadata);
    let version = metadata_version(metadata);
    for record in records {
        for (field, locked) in [
            ("os", &record.platform.os),
            ("cpu", &record.platform.cpu),
            ("libc", &record.platform.libc),
        ] {
            let Some(locked) = locked else { continue };
            let upstream_raw = metadata.as_map().and_then(|m| m.get(field));
            let normalized: Option<Vec<String>> = match upstream_raw {
                None => None,
                Some(v) if crate::value::is_null(v) => None,
                Some(v) => {
                    let mut items = if let Some(seq) = v.as_seq() {
                        let mut out = Vec::new();
                        for item in seq.iter() {
                            out.push(item.as_str().unwrap_or("").to_string());
                        }
                        out
                    } else {
                        vec![v.as_str().unwrap_or("").to_string()]
                    };
                    items.sort();
                    items.dedup();
                    Some(items)
                }
            };
            let mut locked_sorted = locked.clone();
            locked_sorted.sort();
            if normalized.as_ref() != Some(&locked_sorted) {
                return Err(Error::Conflict(format!(
                    "{name}@{version} upstream {field} metadata conflicts with the lockfile"
                )));
            }
        }
    }
    Ok(())
}

fn supports_target(platform: &crate::inventory::PlatformConstraints, target: &Target) -> bool {
    let libc: Option<&str> = match target {
        Target::Linux(t) => Some(t.libc.as_str()),
        Target::Darwin(_) => None,
    };
    for (values, value) in [
        (&platform.os, Some(target.os())),
        (&platform.cpu, Some(target.cpu())),
        (&platform.libc, libc),
    ] {
        let Some(values) = values else { continue };
        let Some(value) = value else { continue };
        let excluded: Vec<&str> = values.iter().filter(|v| v.starts_with('!')).map(|v| &v[1..]).collect();
        let included: Vec<&str> = values.iter().filter(|v| !v.starts_with('!')).map(|v| v.as_str()).collect();
        if excluded.contains(&value) || (!included.is_empty() && !included.contains(&value)) {
            return false;
        }
    }
    true
}

pub struct PackageTask {
    pub name: String,
    pub version: String,
    pub records: Vec<PackageRecord>,
    pub targets: Vec<Target>,
    pub upstream_registry: String,
    pub artifact_record: Option<ArtifactRecord>,
}

pub fn group_packages(inventory: &Inventory, config: &Config) -> Result<Vec<PackageTask>> {
    let mut packages: BTreeMap<(String, String), PackageTask> = BTreeMap::new();
    let mut artifacts: BTreeMap<(String, String), ArtifactRecord> = BTreeMap::new();
    for artifact in &inventory.artifacts {
        artifacts.insert((artifact.name.clone(), artifact.version.clone()), artifact.clone());
    }
    for project in &inventory.projects {
        for record in &project.packages {
            if record.source_kind == crate::inventory::SourceKind::Local {
                continue;
            }
            if record.source_kind != crate::inventory::SourceKind::Registry {
                return Err(Error::Input(format!(
                    "fetch rejects {} source {}{}",
                    record.source_kind.as_str(),
                    record.name,
                    record.version.clone().map(|v| format!("@{v}")).unwrap_or_default()
                )));
            }
            let Some(ref version) = record.version else {
                return Err(Error::Input(format!("fetch requires an exact version for {}", record.name)));
            };
            let mut targets = Vec::new();
            for id in &record.targets {
                let target = config
                    .targets
                    .iter()
                    .find(|t| t.id() == id)
                    .ok_or_else(|| Error::Input(format!("inventory package {}@{version} references unknown target {id}", record.name)))?;
                if supports_target(&record.platform, target) {
                    targets.push(target.clone());
                }
            }
            if targets.is_empty() {
                continue;
            }
            let key = (record.name.clone(), version.clone());
            let entry = packages.entry(key.clone()).or_insert_with(|| PackageTask {
                name: record.name.clone(),
                version: version.clone(),
                records: Vec::new(),
                targets: Vec::new(),
                upstream_registry: config.upstream_registry.clone(),
                artifact_record: artifacts.get(&key).cloned(),
            });
            entry.records.push(record.clone());
            for target in targets {
                if !entry.targets.iter().any(|t| t.id() == target.id()) {
                    entry.targets.push(target);
                }
            }
        }
    }
    let mut out: Vec<PackageTask> = packages.into_values().collect();
    out.sort_by_key(|a| (a.name.clone(), a.version.clone()));
    Ok(out)
}

fn validate_metadata_response(value: &deser_value::Value, task: &PackageTask, expected_host: &str) -> Result<(deser_value::Value, deser_value::Value, String)> {
    let root = value.as_map().ok_or_else(|| Error::Operation(format!("upstream package metadata does not match {}", task.name)))?;
    let name_ok = root.get("name").and_then(|v| v.as_str()) == Some(task.name.as_str());
    let versions = root.get("versions").and_then(|v| v.as_map());
    if !name_ok || versions.is_none() {
        return Err(Error::Operation(format!("upstream package metadata does not match {}", task.name)));
    }
    let versions = versions.expect("versions");
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let meta_val = versions.get(task.version.as_str()).unwrap_or(&null);
    let meta = meta_val.as_map().ok_or_else(|| {
        Error::Operation(format!("upstream metadata is missing exact version {}@{}", task.name, task.version))
    })?;
    if meta.get("name").and_then(|v| v.as_str()) != Some(task.name.as_str())
        || meta.get("version").and_then(|v| v.as_str()) != Some(task.version.as_str())
    {
        return Err(Error::Operation(format!(
            "upstream metadata is missing exact version {}@{}",
            task.name, task.version
        )));
    }
    let dist = meta.get("dist").and_then(|v| v.as_map()).ok_or_else(|| {
        Error::Operation(format!("upstream metadata has no tarball for {}@{}", task.name, task.version))
    })?;
    let tarball_raw = dist.get("tarball").and_then(|v| v.as_str()).ok_or_else(|| {
        Error::Operation(format!("upstream metadata has no tarball for {}@{}", task.name, task.version))
    })?;
    let tarball_url = validate_source_url(tarball_raw, expected_host)
        .map_err(|_| Error::Operation(format!("upstream metadata has no tarball for {}@{}", task.name, task.version)))?
        .to_string();
    let mut packument_map = deser_value::Map::new();
    for i in 0..root.len() {
        if let Some((k, v)) = root.get_index(i)
            && k.as_str() != Some("versions") {
                packument_map.insert(k.clone(), v.clone());
            }
    }
    packument_map.insert("name".to_string(), deser_value::Value::from(task.name.clone()));
    let packument = deser_value::Value::from(packument_map);
    let mut metadata_map = deser_value::Map::new();
    for i in 0..meta.len() {
        if let Some((k, v)) = meta.get_index(i) {
            metadata_map.insert(k.clone(), v.clone());
        }
    }
    let metadata = deser_value::Value::from(metadata_map);
    Ok((packument, metadata, tarball_url))
}

fn hash_stream<R: Read>(mut reader: R, max_bytes: u64, label: &str) -> Result<(u64, BTreeMap<String, String>)> {
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut sha384 = Sha384::new();
    let mut sha512 = Sha512::new();
    let mut bytes = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf).map_err(|_| Error::Operation(format!("could not read stored artifact {label}")))?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > max_bytes {
            return Err(Error::Operation(format!(
                "stored artifact exceeds the {} compressed size limit: {label}",
                limits::format_bytes(limits::TARBALL_BYTES)
            )));
        }
        sha1.update(&buf[..n]);
        sha256.update(&buf[..n]);
        sha384.update(&buf[..n]);
        sha512.update(&buf[..n]);
    }
    let mut out = BTreeMap::new();
    out.insert("sha1".to_string(), format!("{:x}", sha1.finalize()));
    out.insert("sha256".to_string(), format!("{:x}", sha256.finalize()));
    out.insert("sha384".to_string(), format!("{:x}", sha384.finalize()));
    out.insert("sha512".to_string(), format!("{:x}", sha512.finalize()));
    Ok((bytes, out))
}

fn check_digests(expected: &BTreeMap<String, String>, actual: &BTreeMap<String, String>, identity: &str) -> Result<()> {
    for (algo, digest) in expected {
        if actual.get(algo) != Some(digest) {
            let display = if algo == "sha512" { "SHA-512".to_string() } else { algo.to_uppercase() };
            return Err(Error::Conflict(format!("{display} integrity mismatch for {identity}")));
        }
    }
    Ok(())
}

fn object_path(state_dir: &Path, digest: &str) -> PathBuf {
    state_dir.join("objects").join("sha512").join(format!("{digest}.tgz"))
}

fn ensure_managed_dir(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory)
        .map_err(|e| Error::Operation(format!("could not prepare managed directory {}: {e}", directory.display())))?;
    let meta = std::fs::symlink_metadata(directory)
        .map_err(|e| Error::Operation(format!("could not inspect managed directory {}: {e}", directory.display())))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::Operation(format!("managed directory is not a real directory: {}", directory.display())));
    }
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| Error::Operation(format!("could not set managed directory permissions {}: {e}", directory.display())))?;
    Ok(())
}

fn assert_safe_object_dirs(state_dir: &Path, corrupt: bool) -> Result<()> {
    for directory in [state_dir.join("objects"), state_dir.join("objects").join("sha512")] {
        match std::fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                let msg = format!("could not inspect object store directory {}: {e}", directory.display());
                return Err(if corrupt { Error::Corrupt(msg) } else { Error::Operation(msg) });
            }
            Ok(meta) => {
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    let msg = format!("object store directory is not a real directory: {}", directory.display());
                    return Err(if corrupt { Error::Corrupt(msg) } else { Error::Operation(msg) });
                }
            }
        }
    }
    Ok(())
}

fn inspect_object(
    path: &Path,
    task: &PackageTask,
    metadata: &deser_value::Value,
    expected_claims: &BTreeMap<String, String>,
    category: &str,
) -> Result<(u64, BTreeMap<String, String>)> {
    let fail = |msg: String| {
        if category == "corrupt" {
            Error::Corrupt(msg)
        } else {
            Error::Operation(msg)
        }
    };
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| fail(format!("could not verify stored artifact {}@{}", task.name, task.version)))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(fail(format!("stored artifact is not a regular file: {}", path.display())));
    }
    if meta.len() > limits::TARBALL_BYTES {
        return Err(fail(format!(
            "stored artifact exceeds the {} compressed size limit: {}@{}",
            limits::format_bytes(limits::TARBALL_BYTES),
            task.name,
            task.version
        )));
    }
    let file = std::fs::File::open(path)
        .map_err(|_| fail(format!("could not verify stored artifact {}@{}", task.name, task.version)))?;
    let (bytes, actual) = hash_stream(file, limits::TARBALL_BYTES, &format!("{}@{}", task.name, task.version))
        .map_err(|e| fail(format!("could not verify stored artifact {}@{}: {e}", task.name, task.version)))?;
    check_digests(expected_claims, &actual, &format!("{}@{}", task.name, task.version))
        .map_err(|e| fail(format!("could not verify stored artifact {}@{}: {e}", task.name, task.version)))?;
    if let Some(stored) = task
        .artifact_record
        .as_ref()
        .map(|r| r.artifact.sha512.clone())
        .or_else(|| expected_claims.get("sha512").cloned())
        && actual.get("sha512") != Some(&stored)
    {
        return Err(fail(format!(
            "stored artifact SHA-512 does not match inventory for {}@{}",
            task.name, task.version
        )));
    }
    let dist = metadata_dist(metadata);
    if let Some(shasum) = dist_shasum(&dist)
        && actual.get("sha1") != Some(&shasum)
    {
        return Err(fail(format!(
            "stored artifact SHA-1 does not match metadata for {}@{}",
            task.name, task.version
        )));
    }
    let inspected = inspect_package_archive(path.to_str().unwrap_or(""), &task.name, &task.version).map_err(|cause| {
        fail(format!("stored package archive is invalid for {}@{}: {cause}", task.name, task.version))
    })?;
    validate_platform_claims(&task.records, &inspected.manifest)
        .map_err(|e| fail(format!("stored package metadata conflicts for {}@{}: {e}", task.name, task.version)))?;
    validate_platform_claims(&task.records, metadata)
        .map_err(|e| fail(format!("stored package metadata conflicts for {}@{}: {e}", task.name, task.version)))?;
    Ok((bytes, actual))
}

pub struct AcquisitionContext<'a> {
    pub state_directory: PathBuf,
    pub upstream_registry: String,
    pub allow_legacy_sha1_only: bool,
    pub expected_host: String,
    pub exchange: &'a dyn Exchange,
    pub budget: &'a Mutex<FetchByteBudget>,
    pub staging_directory: PathBuf,
}

pub struct PackageResult {
    pub task_name: String,
    pub task_version: String,
    pub metadata: deser_value::Value,
    pub packument: deser_value::Value,
    pub artifact_sha512: String,
    pub artifact_integrity: String,
    pub artifact_shasum: Option<String>,
    pub artifact_source_url: String,
    pub artifact_legacy_sha1_only: bool,
    pub stage_path: Option<PathBuf>,
    pub byte_count: u64,
    pub reused: bool,
}

fn fetch_metadata(
    task: &PackageTask,
    ctx: &AcquisitionContext,
    deadline: Instant,
) -> Result<(deser_value::Value, deser_value::Value, String)> {
    if Instant::now() >= deadline {
        return Err(Error::Operation("fetch was cancelled".to_string()));
    }
    let base = task.upstream_registry.trim_end_matches('/');
    let url = format!("{}/{}", base, package_route(&task.name));
    let (body, _) = transfer(&url, limits::METADATA_BYTES, &ctx.expected_host, ctx.exchange, ctx.budget)?;
    let text = String::from_utf8(body)
        .map_err(|_| Error::Operation(format!("upstream metadata is not valid UTF-8 JSON for {}", task.name)))?;
    let parsed: deser_value::Value = deser_json::from_str(&text)
        .map_err(|_| Error::Operation(format!("upstream metadata is not valid UTF-8 JSON for {}", task.name)))?;
    if !is_json_value(&parsed) {
        return Err(Error::Operation(format!(
            "upstream metadata is not valid UTF-8 JSON for {}",
            task.name
        )));
    }
    validate_metadata_response(&parsed, task, &ctx.expected_host)
        .map_err(|_| Error::Operation(format!("upstream metadata is invalid for {}", task.name)))
}

fn is_json_value(v: &deser_value::Value) -> bool {
    match &**v {
        deser_value::Kind::Null
        | deser_value::Kind::Bool(_)
        | deser_value::Kind::U64(_)
        | deser_value::Kind::I64(_)
        | deser_value::Kind::F64(_)
        | deser_value::Kind::Str(_)
        | deser_value::Kind::Char(_) => true,
        deser_value::Kind::F32(_) | deser_value::Kind::Lexical(_) => true,
        _ if v.as_seq().is_some() => v.as_seq().expect("seq").iter().all(is_json_value),
        _ if v.as_map().is_some() => {
            let m = v.as_map().expect("map");
            (0..m.len()).all(|i| m.get_index(i).is_some_and(|(k, item)| k.as_str().is_some() && is_json_value(item)))
        }
        _ => false,
    }
}

fn download_stage(
    stage_path: &Path,
    task: &PackageTask,
    metadata: &deser_value::Value,
    tarball_url: &str,
    expected_claims: &BTreeMap<String, String>,
    ctx: &AcquisitionContext,
    deadline: Instant,
) -> Result<(String, BTreeMap<String, String>, u64)> {
    if Instant::now() >= deadline {
        return Err(Error::Operation("fetch was cancelled".to_string()));
    }
    let (body, _) = transfer(tarball_url, limits::TARBALL_BYTES, &ctx.expected_host, ctx.exchange, ctx.budget)?;
    if body.is_empty() {
        return Err(Error::Operation(format!(
            "upstream returned an empty tarball for {}@{}",
            task.name, task.version
        )));
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(stage_path)
        .map_err(|e| Error::Operation(format!("could not create staged tarball for {}@{}: {e}", task.name, task.version)))?;
    file.write_all(&body)
        .map_err(|e| Error::Operation(format!("could not stage tarball for {}@{}: {e}", task.name, task.version)))?;
    file.sync_all()
        .map_err(|e| Error::Operation(format!("could not sync staged tarball for {}@{}: {e}", task.name, task.version)))?;
    drop(file);
    let stored = std::fs::symlink_metadata(stage_path)
        .map_err(|e| Error::Operation(format!("could not inspect staged tarball for {}@{}: {e}", task.name, task.version)))?;
    if stored.len() != body.len() as u64 {
        let _ = std::fs::remove_file(stage_path);
        return Err(Error::Operation(format!(
            "staged tarball size changed for {}@{}",
            task.name, task.version
        )));
    }
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut sha384 = Sha384::new();
    let mut sha512 = Sha512::new();
    sha1.update(&body);
    sha256.update(&body);
    sha384.update(&body);
    sha512.update(&body);
    let mut digests = BTreeMap::new();
    digests.insert("sha1".to_string(), format!("{:x}", sha1.finalize()));
    digests.insert("sha256".to_string(), format!("{:x}", sha256.finalize()));
    digests.insert("sha384".to_string(), format!("{:x}", sha384.finalize()));
    digests.insert("sha512".to_string(), format!("{:x}", sha512.finalize()));
    check_digests(expected_claims, &digests, &format!("{}@{}", task.name, task.version)).map_err(|e| {
        let _ = std::fs::remove_file(stage_path);
        Error::Conflict(format!("tarball integrity mismatch for {}@{}: {e}", task.name, task.version))
    })?;
    let inspected = inspect_package_archive(stage_path.to_str().unwrap_or(""), &task.name, &task.version).map_err(|cause| {
        let _ = std::fs::remove_file(stage_path);
        Error::Operation(format!("could not inspect package archive for {}@{}: {cause}", task.name, task.version))
    })?;
    validate_platform_claims(&task.records, &inspected.manifest).map_err(|e| {
        let _ = std::fs::remove_file(stage_path);
        Error::Conflict(format!("staged package metadata conflicts for {}@{}: {e}", task.name, task.version))
    })?;
    validate_platform_claims(&task.records, metadata).map_err(|e| {
        let _ = std::fs::remove_file(stage_path);
        Error::Conflict(format!("upstream package metadata conflicts for {}@{}: {e}", task.name, task.version))
    })?;
    let digest = digests.get("sha512").cloned().unwrap_or_default();
    Ok((digest, digests, body.len() as u64))
}

fn acquire_package(task: PackageTask, ctx: &AcquisitionContext, deadline: Instant) -> Result<PackageResult> {
    assert_safe_object_dirs(&ctx.state_directory, false)?;
    if let Some(record) = task.artifact_record.clone() {
        let (mut expected, legacy) = merge_claims(&task.records, &record.metadata)
            .map_err(|e| Error::Corrupt(format!("inventory integrity is invalid for {}@{}: {e}", task.name, task.version)))?;
        if legacy && !ctx.allow_legacy_sha1_only {
            return Err(Error::Corrupt(format!(
                "inventory records SHA-1-only metadata for {}@{}",
                task.name, task.version
            )));
        }
        if let Some(lock_sha) = expected.get("sha512")
            && lock_sha != &record.artifact.sha512
        {
            return Err(Error::Conflict(format!(
                "stored digest conflicts with lockfile for {}@{}",
                task.name, task.version
            )));
        }
        expected.insert("sha512".to_string(), record.artifact.sha512.clone());
        if let Some(ref shasum) = record.artifact.shasum {
            expected.insert("sha1".to_string(), shasum.clone());
        }
        let path = object_path(&ctx.state_directory, &record.artifact.sha512);
        if path.exists() {
            inspect_object(&path, &task, &record.metadata, &expected, "corrupt")?;
            return Ok(PackageResult {
                task_name: task.name.clone(),
                task_version: task.version.clone(),
                metadata: record.metadata.clone(),
                packument: record.packument.clone(),
                artifact_sha512: record.artifact.sha512.clone(),
                artifact_integrity: record.artifact.integrity.clone(),
                artifact_shasum: record.artifact.shasum.clone(),
                artifact_source_url: record.artifact.source_url.clone(),
                artifact_legacy_sha1_only: record.artifact.legacy_sha1_only,
                stage_path: None,
                byte_count: 0,
                reused: true,
            });
        }
        let tarball_url = record_tarball(&record).ok_or_else(|| {
            Error::Operation(format!("upstream tarball URL is invalid for {}@{}", task.name, task.version))
        })?;
        let tarball_url = validate_source_url(&tarball_url, &ctx.expected_host)
            .map_err(|_| Error::Operation(format!("upstream tarball URL is invalid for {}@{}", task.name, task.version)))?
            .to_string();
        let stage_path = ctx.staging_directory.join(format!("{}.part", unique_name()));
        let (digest, _, byte_count) =
            download_stage(&stage_path, &task, &record.metadata, &tarball_url, &expected, ctx, deadline)?;
        if digest != record.artifact.sha512 {
            let _ = std::fs::remove_file(&stage_path);
            return Err(Error::Conflict(format!(
                "refetched artifact digest changed for {}@{}",
                task.name, task.version
            )));
        }
        return Ok(PackageResult {
            task_name: task.name.clone(),
            task_version: task.version.clone(),
            metadata: record.metadata.clone(),
            packument: record.packument.clone(),
            artifact_sha512: record.artifact.sha512.clone(),
            artifact_integrity: record.artifact.integrity.clone(),
            artifact_shasum: record.artifact.shasum.clone(),
            artifact_source_url: record.artifact.source_url.clone(),
            artifact_legacy_sha1_only: record.artifact.legacy_sha1_only,
            stage_path: Some(stage_path),
            byte_count,
            reused: false,
        });
    }

    let (packument, metadata, tarball_url) = fetch_metadata(&task, ctx, deadline)?;
    validate_platform_claims(&task.records, &metadata).map_err(|e| {
        Error::Conflict(format!("upstream package metadata conflicts for {}@{}: {e}", task.name, task.version))
    })?;
    let (claims, legacy) = merge_claims(&task.records, &metadata)
        .map_err(|e| Error::Operation(format!("upstream integrity is invalid for {}@{}: {e}", task.name, task.version)))?;
    if legacy && !ctx.allow_legacy_sha1_only {
        return Err(Error::Operation(format!(
            "refusing SHA-1-only integrity for {}@{}; enable policy.allowLegacySha1Only only after audit",
            task.name, task.version
        )));
    }
    let lock_sha512 = lock_claims(&task.records)
        .map_err(|e| Error::Operation(format!("lockfile integrity is invalid for {}@{}: {e}", task.name, task.version)))?
        .get("sha512")
        .cloned();
    let metadata_sha512 = claims.get("sha512").cloned();
    let expected_sha512 = metadata_sha512.or(lock_sha512);
    let mut expected_claims = claims;
    if let Some(ref sha) = expected_sha512 {
        expected_claims.insert("sha512".to_string(), sha.clone());
    }
    if let Some(ref sha) = expected_sha512 {
        let candidate = object_path(&ctx.state_directory, sha);
        if candidate.exists() {
            let (_, digests) = inspect_object(&candidate, &task, &metadata, &expected_claims, "corrupt")?;
            let sha512 = digests.get("sha512").cloned().unwrap_or_default();
            return Ok(PackageResult {
                task_name: task.name.clone(),
                task_version: task.version.clone(),
                metadata: metadata.clone(),
                packument: packument.clone(),
                artifact_sha512: sha512.clone(),
                artifact_integrity: format!("sha512-{}", base64_engine_encode(&hex_decode(&sha512))),
                artifact_shasum: if expected_claims.contains_key("sha1") {
                    digests.get("sha1").cloned()
                } else {
                    None
                },
                artifact_source_url: dist_tarball(&metadata_dist(&metadata)).unwrap_or_default(),
                artifact_legacy_sha1_only: legacy,
                stage_path: None,
                byte_count: 0,
                reused: true,
            });
        }
    }
    let stage_path = ctx.staging_directory.join(format!("{}.part", unique_name()));
    let (digest, digests, byte_count) =
        download_stage(&stage_path, &task, &metadata, &tarball_url, &expected_claims, ctx, deadline)?;
    Ok(PackageResult {
        artifact_integrity: format!("sha512-{}", base64_engine_encode(&hex_decode(&digest))),
        artifact_shasum: if expected_claims.contains_key("sha1") {
            digests.get("sha1").cloned()
        } else {
            None
        },
        artifact_sha512: digest,
        artifact_source_url: dist_tarball(&metadata_dist(&metadata)).unwrap_or_default(),
        artifact_legacy_sha1_only: legacy,
        task_name: task.name.clone(),
        task_version: task.version.clone(),
        metadata,
        packument,
        stage_path: Some(stage_path),
        byte_count,
        reused: false,
    })
}

fn hex_decode(hex: &str) -> Vec<u8> {
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16).unwrap_or(0);
        let lo = (bytes[i + 1] as char).to_digit(16).unwrap_or(0);
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    out
}

fn unique_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

fn promote_artifact(result: &mut PackageResult, state_dir: &Path) -> Result<()> {
    let Some(stage_path) = result.stage_path.clone() else {
        return Ok(());
    };
    let destination = object_path(state_dir, &result.artifact_sha512);
    if let Some(parent) = destination.parent() {
        ensure_managed_dir(parent)?;
    }
    match std::fs::hard_link(&stage_path, &destination) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut expected = BTreeMap::new();
            expected.insert("sha512".to_string(), result.artifact_sha512.clone());
            if let Some(ref shasum) = result.artifact_shasum {
                expected.insert("sha1".to_string(), shasum.clone());
            }
            let task = PackageTask {
                name: result.task_name.clone(),
                version: result.task_version.clone(),
                records: Vec::new(),
                targets: Vec::new(),
                upstream_registry: String::new(),
                artifact_record: None,
            };
            let (_, actual) = inspect_object(&destination, &task, &result.metadata, &expected, "corrupt")?;
            if actual.get("sha512") != Some(&result.artifact_sha512) {
                return Err(Error::Conflict(format!(
                    "content-addressed object collision for {}",
                    result.artifact_sha512
                )));
            }
        }
        Err(e) => {
            return Err(Error::Operation(format!(
                "could not promote {}@{}: {e}",
                result.task_name, result.task_version
            )));
        }
    }
    std::fs::remove_file(&stage_path)
        .map_err(|e| Error::Operation(format!("could not remove staged tarball for {}@{}: {e}", result.task_name, result.task_version)))?;
    result.stage_path = None;
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    let output = std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .output();
    match output {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            !stderr.contains("No such process")
        }
        Err(_) => true,
    }
}

fn clean_interrupted_staging(staging_root: &Path) -> Result<()> {
    let entries = std::fs::read_dir(staging_root)
        .map_err(|e| Error::Operation(format!("could not inspect private staging directory {}: {e}", staging_root.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            Error::Operation(format!("could not inspect private staging directory {}: {e}", staging_root.display()))
        })?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let rest = name.strip_prefix("mynpm-fetch-").unwrap_or("");
        let pid_part = rest.split('-').next().unwrap_or("");
        if rest.is_empty() {
            continue;
        }
        let pid: u32 = match pid_part.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid_alive(pid) {
            continue;
        }
        std::fs::remove_dir_all(entry.path()).map_err(|e| {
            Error::Operation(format!("could not remove interrupted staging directory {name}: {e}"))
        })?;
    }
    Ok(())
}

pub struct FetchSummary {
    pub downloaded_bytes: u64,
    pub verified_count: usize,
    pub reused_count: usize,
}

pub fn fetch_inventory(
    state_dir: &Path,
    config: &Config,
    exchange: &dyn Exchange,
) -> Result<FetchSummary> {
    let inventory = load_inventory(state_dir)?;
    let tasks = group_packages(&inventory, config)
        .map_err(|e| Error::Input(format!("inventory contains an invalid package: {e}")))?;
    if tasks.is_empty() {
        return Ok(FetchSummary { downloaded_bytes: 0, verified_count: 0, reused_count: 0 });
    }
    let expected_host = url::Url::parse(&config.upstream_registry)
        .map_err(|_| Error::Input("configured upstream registry is invalid".to_string()))?
        .host_str()
        .unwrap_or("")
        .to_string();
    let budget = Mutex::new(FetchByteBudget::new());
    let staging_root = state_dir.join("tmp");
    ensure_managed_dir(&staging_root)?;
    clean_interrupted_staging(&staging_root)?;
    let staging_dir = staging_root.join(format!("mynpm-fetch-{}-{}", std::process::id(), unique_name()));
    std::fs::create_dir_all(&staging_dir).map_err(|e| {
        Error::Operation(format!("could not create private staging directory under {}: {e}", staging_root.display()))
    })?;
    let deadline = Instant::now() + Duration::from_millis(limits::FETCH_MILLISECONDS);
    let result: Result<Vec<PackageResult>> = std::thread::scope(|scope| {
        let queue = std::sync::Arc::new(Mutex::new(tasks.into_iter().collect::<VecDeque<_>>()));
        let results: std::sync::Arc<Mutex<Vec<PackageResult>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
        let first_error: std::sync::Arc<Mutex<Option<String>>> = std::sync::Arc::new(Mutex::new(None));
        let ctx = std::sync::Arc::new(AcquisitionContext {
            state_directory: state_dir.to_path_buf(),
            upstream_registry: config.upstream_registry.clone(),
            allow_legacy_sha1_only: config.policy.allow_legacy_sha1_only,
            expected_host: expected_host.clone(),
            exchange,
            budget: &budget,
            staging_directory: staging_dir.clone(),
        });
        let mut handles = Vec::new();
        for _ in 0..limits::CONCURRENT_DOWNLOADS {
            let (queue, results, first_error, ctx) =
                (queue.clone(), results.clone(), first_error.clone(), ctx.clone());
            handles.push(scope.spawn(move || {
                loop {
                    if first_error.lock().map(|g| g.is_some()).unwrap_or(true) {
                        break;
                    }
                    let task = queue.lock().map(|mut q| q.pop_front()).unwrap_or(None);
                    let Some(task) = task else { break };
                    match acquire_package(task, &ctx, deadline) {
                        Ok(result) => {
                            results.lock().map(|mut r| r.push(result)).ok();
                        }
                        Err(e) => {
                            first_error.lock().map(|mut g| {
                                if g.is_none() {
                                    *g = Some(e.to_string());
                                }
                            }).ok();
                            break;
                        }
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().map_err(|_| Error::Operation("fetch worker failed".to_string()))?;
        }
        if let Some(msg) = first_error.lock().map(|g| g.clone()).unwrap_or(None) {
            return Err(Error::Operation(msg));
        }
        Ok(results.lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default())
    });
    let mut results = result.inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging_dir);
    })?;
    if Instant::now() >= deadline {
        let _ = std::fs::remove_dir_all(&staging_dir);
        return Err(Error::Operation(format!(
            "fetch exceeded the {} operation limit",
            limits::format_duration(limits::FETCH_MILLISECONDS)
        )));
    }
    let promote_outcome = (|| -> Result<()> {
        for result in results.iter_mut() {
            promote_artifact(result, state_dir)?;
        }
        if !results.is_empty() {
            let objects = state_dir.join("objects").join("sha512");
            stuff_store::sync_dir(&objects)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging_dir);
    promote_outcome?;
    let downloaded = budget.lock().map(|b| limits::FETCH_BYTES - b.remaining()).unwrap_or(0);
    let reused = results.iter().filter(|r| r.reused).count();
    let verified = results.len();
    let state_path = state_dir.to_path_buf();
    update_inventory(&state_path, |current| {
        let mut acc = current.clone();
        for result in &results {
            let value = artifact_value(result)?;
            acc = record_verified_artifact(&acc, &value)?;
        }
        Ok(acc)
    })?;
    Ok(FetchSummary { downloaded_bytes: downloaded, verified_count: verified, reused_count: reused })
}

fn artifact_value(result: &PackageResult) -> Result<deser_value::Value> {
    let artifact = crate::inventory::ArtifactRecord {
        name: result.task_name.clone(),
        version: result.task_version.clone(),
        packument: result.packument.clone(),
        metadata: result.metadata.clone(),
        artifact: crate::inventory::ArtifactVerification {
            sha512: result.artifact_sha512.clone(),
            integrity: result.artifact_integrity.clone(),
            shasum: result.artifact_shasum.clone(),
            source_url: result.artifact_source_url.clone(),
            legacy_sha1_only: result.artifact_legacy_sha1_only,
        },
    };
    validate_artifact_record(&crate::inventory::artifact_to_value(&artifact)?)?;
    crate::inventory::artifact_to_value(&artifact)
}

pub fn verify_inventory_snapshot(
    state_dir: &Path,
    config: &Config,
    inventory: &Inventory,
) -> Result<usize> {
    let tasks = group_packages(inventory, config)
        .map_err(|e| Error::Corrupt(format!("inventory contains an invalid package: {e}")))?;
    assert_safe_object_dirs(state_dir, true)?;
    for task in &tasks {
        let Some(record) = task.artifact_record.clone() else {
            return Err(Error::Corrupt(format!(
                "no verified artifact is recorded for {}@{}",
                task.name, task.version
            )));
        };
        let (mut expected, _) = merge_claims(&task.records, &record.metadata)
            .map_err(|e| Error::Corrupt(format!("inventory integrity is invalid for {}@{}: {e}", task.name, task.version)))?;
        expected.insert("sha512".to_string(), record.artifact.sha512.clone());
        if let Some(ref shasum) = record.artifact.shasum {
            expected.insert("sha1".to_string(), shasum.clone());
        }
        inspect_object(&object_path(state_dir, &record.artifact.sha512), task, &record.metadata, &expected, "corrupt")?;
        validate_platform_claims(&task.records, &record.metadata).map_err(|e| {
            Error::Corrupt(format!("inventory metadata is invalid for {}@{}: {e}", task.name, task.version))
        })?;
    }
    Ok(tasks.len())
}

pub fn verify_inventory(state_dir: &Path, config: &Config) -> Result<usize> {
    let inventory = load_inventory(state_dir)?;
    verify_inventory_snapshot(state_dir, config, &inventory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Policy;
    use crate::inventory::{LockfileVersion, Manager, ProjectRecord};

    #[test]
    fn restricted_addresses() {
        for host in ["127.0.0.1", "10.1.2.3", "192.168.1.1", "169.254.169.254", "224.0.0.1", "0.0.0.0"] {
            assert!(is_restricted_address(host.parse().unwrap()), "{host}");
        }
        assert!(is_restricted_address("::1".parse().unwrap()));
        assert!(is_restricted_address("::ffff:1.2.3.4".parse().unwrap()));
        assert!(is_restricted_address("fc00::1".parse().unwrap()));
        assert!(is_restricted_address("ff02::1".parse().unwrap()));
        assert!(!is_restricted_address("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn scoped_routes_encode() {
        assert_eq!(package_route("flatdemo"), "flatdemo");
        assert_eq!(package_route("@scope/pkg"), "@scope%2fpkg");
    }

    #[test]
    fn rejects_redirect_outside_registry() {
        struct Loop;
        impl Exchange for Loop {
            fn fetch(&self, _url: &url::Url) -> Result<FetchedResponse> {
                Ok(FetchedResponse {
                    status: 302,
                    location: Some("https://evil.test/x".to_string()),
                    body: Vec::new(),
                })
            }
        }
        let budget = Mutex::new(FetchByteBudget::new());
        let err = transfer(
            "https://registry.npmjs.org/flatdemo",
            limits::METADATA_BYTES,
            "registry.npmjs.org",
            &Loop,
            &budget,
        )
        .unwrap_err();
        assert!(format!("{err}").contains("allowlisted"), "{err}");
    }

    fn fixture_config(targets: Vec<Target>) -> Config {
        Config {
            public_base_url: "https://example.test/mirror/".to_string(),
            upstream_registry: "https://registry.npmjs.org/".to_string(),
            targets,
            policy: Policy { allow_legacy_sha1_only: false },
        }
    }

    fn fixture_inventory(name: &str, version: &str, integrity: Option<&str>) -> Inventory {
        let project = ProjectRecord {
            id: "demo".to_string(),
            manager: Manager::Npm,
            package_manager_pin: None,
            lockfile_path: "package-lock.json".to_string(),
            lockfile_version: LockfileVersion::Npm(3),
            targets: vec!["t1".to_string()],
            importers: vec![".".to_string()],
            roots: vec![],
            packages: vec![PackageRecord {
                key: format!("node_modules/{name}"),
                name: name.to_string(),
                version: Some(version.to_string()),
                integrity: integrity.map(|s| s.to_string()),
                resolved: Some(format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz")),
                source_kind: crate::inventory::SourceKind::Registry,
                aliases: vec![],
                flags: crate::inventory::PackageFlags {
                    dev: false,
                    optional: false,
                    peer: false,
                    peer_optional: false,
                    link: false,
                },
                platform: crate::inventory::PlatformConstraints { os: None, cpu: None, libc: None },
                targets: vec!["t1".to_string()],
            }],
            snapshot: None,
        };
        Inventory { projects: vec![project], artifacts: vec![] }
    }

    #[test]
    fn rejects_non_registry_sources_when_grouping() {
        let mut inv = fixture_inventory("a", "1.0.0", None);
        inv.projects[0].packages[0].source_kind = crate::inventory::SourceKind::Git;
        let config = fixture_config(vec![]);
        assert!(group_packages(&inv, &config).is_err());
    }

    struct FakeRegistry {
        tarball: Vec<u8>,
        tarball_url: String,
    }

    impl Exchange for FakeRegistry {
        fn fetch(&self, url: &url::Url) -> Result<FetchedResponse> {
            if url.as_str() == self.tarball_url {
                return Ok(FetchedResponse { status: 200, location: None, body: self.tarball.clone() });
            }
            let integrity = format!("sha512-{}", base64_engine_encode(&{
                use sha2::Digest;
                let mut h = Sha512::new();
                h.update(&self.tarball);
                h.finalize().to_vec()
            }));
            let shasum = {
                use sha2::Digest;
                let mut h = Sha1::new();
                h.update(&self.tarball);
                format!("{:x}", h.finalize())
            };
            let body = format!(
                "{{\"name\":\"flatdemo\",\"versions\":{{\"1.2.3\":{{\"name\":\"flatdemo\",\"version\":\"1.2.3\",\"dist\":{{\"tarball\":\"{}\",\"integrity\":\"{integrity}\",\"shasum\":\"{shasum}\"}}}}}}}}",
                self.tarball_url
            );
            Ok(FetchedResponse { status: 200, location: None, body: body.into_bytes() })
        }
    }

    fn fixture_tarball() -> Vec<u8> {
        let manifest = br#"{"name":"flatdemo","version":"1.2.3"}"#;
        let mut data = Vec::new();
        for (name, contents) in [("package/package.json", &manifest[..]), ("package/index.js", b"module.exports=1;" as &[u8])] {
            let mut header = vec![0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(b"0000644\0");
            header[136..148].copy_from_slice(b"00000000000\0");
            header[148..156].copy_from_slice(b"        ");
            header[156] = b'0';
            header[257..262].copy_from_slice(b"ustar");
            header[263..265].copy_from_slice(b"00");
            let size = format!("{:07o}\0", contents.len());
            header[124..132].copy_from_slice(size.as_bytes());
            let sum: u32 = header.iter().map(|&b| b as u32).sum();
            let chk = format!("{sum:06o}\0 ");
            header[148..156].copy_from_slice(chk.as_bytes());
            data.extend_from_slice(&header);
            data.extend_from_slice(contents);
            while data.len() % 512 != 0 {
                data.push(0);
            }
        }
        data.extend_from_slice(&vec![0u8; 1024]);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn fetches_and_verifies_offline() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let tarball = fixture_tarball();
        let tarball_url = "https://registry.npmjs.org/flatdemo/-/flatdemo-1.2.3.tgz".to_string();
        let exchange = FakeRegistry { tarball: tarball.clone(), tarball_url: tarball_url.clone() };
        let integrity = {
            use sha2::Digest;
            let mut h = Sha512::new();
            h.update(&tarball);
            format!("sha512-{}", base64_engine_encode(&h.finalize()))
        };
        let mut inv = fixture_inventory("flatdemo", "1.2.3", Some(&integrity));
        inv.projects[0].packages[0].resolved = Some(tarball_url);
        let config = fixture_config(vec![Target::Darwin(crate::config::DarwinTarget {
            id: "t1".to_string(),
            cpu: "arm64".to_string(),
            node: "26.12.0".to_string(),
        })]);
        crate::inventory_store::update_inventory(&state, |_| Ok(inv.clone())).unwrap();
        let summary = fetch_inventory(&state, &config, &exchange).unwrap();
        assert_eq!(summary.verified_count, 1);
        assert_eq!(summary.reused_count, 0);
        assert!(summary.downloaded_bytes > 0);
        let verified = verify_inventory(&state, &config).unwrap();
        assert_eq!(verified, 1);
        let summary2 = fetch_inventory(&state, &config, &exchange).unwrap();
        assert_eq!(summary2.reused_count, 1);
    }
}
