#![forbid(unsafe_code)]

use crate::config::{Config, Target};
use crate::error::{Error, Result};
use crate::inventory::{
    ArtifactRecord, Manager, PackageRecord,
    ProjectRecord,
};
use crate::inventory_store::load_inventory;
use crate::limits;
use crate::lock_npm::{parse_npm_lock, NpmOptions};
use crate::lock_pnpm::{parse_pnpm_lock, PnpmOptions};
use crate::process::{run_process, ProcessOptions, StdoutMode};
use crate::project_snapshot::assert_restorable_snapshot;
use crate::publish::{publish_site, PublishOptions};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sha2::Sha512;

pub const CLIENT_TIMEOUT_MS: u64 = 60 * 60 * 1000;

const WORKSPACE_PREFIX: &str = "mynpm-validate-";
const STALE_WORKSPACE_AGE: Duration = Duration::from_secs(60 * 60);

fn sweep_stale_workspaces(scratch_root: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(scratch_root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(WORKSPACE_PREFIX) {
            continue;
        }
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub client: String,
    pub version: String,
    pub node_version: String,
    pub target: String,
    pub package_count: usize,
    pub platform_skipped_count: usize,
    pub project_count: usize,
    pub root_count: usize,
    pub request_count: usize,
    pub generation_id: String,
    pub executables: Vec<(String, String)>,
}

fn target_for_current_platform(config: &Config) -> Result<Target> {
    let os = match std::env::consts::OS {
        "macos" => Some("darwin"),
        "linux" => Some("linux"),
        _ => None,
    };
    let cpu = match std::env::consts::ARCH {
        "aarch64" => Some("arm64"),
        "x86_64" => Some("x64"),
        _ => None,
    };
    let libc: Option<&str> = if os == Some("linux") {
        Some(if cfg!(target_env = "musl") { "musl" } else { "glibc" })
    } else {
        None
    };
    config
        .targets
        .iter()
        .find(|t| {
            Some(t.os()) == os && Some(t.cpu()) == cpu
                && match t {
                    Target::Linux(l) => Some(l.libc.as_str()) == libc,
                    Target::Darwin(_) => libc.is_none(),
                }
        })
        .cloned()
        .ok_or_else(|| {
            Error::Input(format!(
                "the current platform is not configured as a target: {}/{}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                libc.unwrap_or("")
            ))
        })
}

fn supports_platform_value(values: &Option<Vec<String>>, value: Option<&str>) -> bool {
    let Some(values) = values else { return true };
    let Some(value) = value else { return true };
    let excluded: Vec<&str> = values.iter().filter(|v| v.starts_with('!')).map(|v| &v[1..]).collect();
    let included: Vec<&str> = values.iter().filter(|v| !v.starts_with('!')).map(|v| v.as_str()).collect();
    !excluded.contains(&value) && (included.is_empty() || included.contains(&value))
}

fn supports_target(platform: &crate::inventory::PlatformConstraints, target: &Target) -> bool {
    let libc: Option<&str> = match target {
        Target::Linux(t) => Some(t.libc.as_str()),
        Target::Darwin(_) => None,
    };
    supports_platform_value(&platform.os, Some(target.os()))
        && supports_platform_value(&platform.cpu, Some(target.cpu()))
        && supports_platform_value(&platform.libc, libc)
}

fn package_slug(value: &str) -> String {
    let mut slug: String = value
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "project".to_string() } else { slug }
}

fn integrity_key(
    value: Option<&str>,
    artifact: Option<&ArtifactRecord>,
    identity: &str,
) -> Result<String> {
    let Some(artifact) = artifact else {
        return Err(Error::Corrupt(format!("no verified artifact is available for {identity}")));
    };
    let mut expected: FxHashMap<String, String> = FxHashMap::default();
    expected.insert("sha512".to_string(), artifact.artifact.sha512.clone());
    if let Some(ref shasum) = artifact.artifact.shasum {
        expected.insert("sha1".to_string(), shasum.clone());
    }
    if let Some(text) = value {
        for token in text.split_whitespace() {
            let claim = token.split('?').next().unwrap_or("");
            let (algorithm, digest) = match claim.find('-') {
                Some(i) => (claim[..i].to_string(), claim[i + 1..].to_string()),
                None => (
                    claim.get(..claim.len().saturating_sub(1)).unwrap_or("").to_string(),
                    claim.to_string(),
                ),
            };
            let want = expected.get(&algorithm);
            let matches = want.is_some_and(|hex| crate::value::lenient_hex(&digest) == *hex);
            if !matches {
                return Err(Error::Conflict(format!(
                    "{} integrity does not match verified artifact for {identity}",
                    if algorithm.is_empty() { "unknown".to_string() } else { algorithm }
                )));
            }
        }
    }
    Ok(artifact.artifact.integrity.clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedPackage {
    identity: String,
    integrity: String,
}

fn package_selection(
    packages: &[PackageRecord],
    target: &Target,
    artifacts: &[ArtifactRecord],
    allow_local_host: bool,
    local_origin: Option<&str>,
) -> Result<Vec<SelectedPackage>> {
    let by_identity: FxHashMap<(String, String), &ArtifactRecord> = artifacts
        .iter()
        .map(|r| ((r.name.clone(), r.version.clone()), r))
        .collect();
    let mut selected: FxHashMap<String, SelectedPackage> = FxHashMap::default();
    for item in packages {
        let Some(ref version) = item.version else { continue };
        if item.source_kind == crate::inventory::SourceKind::Local || !supports_target(&item.platform, target) {
            continue;
        }
        if !allow_local_host && item.source_kind != crate::inventory::SourceKind::Registry {
            continue;
        }
        if allow_local_host {
            if item.source_kind == crate::inventory::SourceKind::Git {
                return Err(Error::Conflict(format!(
                    "validation lock contains a git source for {}@{version}",
                    item.name
                )));
            }
            if let Some(ref resolved) = item.resolved {
                let parsed = stuff_url::Url::parse(resolved).map_err(|_| {
                    Error::Conflict(format!(
                        "validation lock contains an invalid source for {}@{version}",
                        item.name
                    ))
                })?;
                if parsed.origin_ascii() != local_origin.unwrap_or("") {
                    return Err(Error::Conflict(format!(
                        "validation lock resolved {}@{version} outside the local static site",
                        item.name
                    )));
                }
            }
        }
        let identity = format!("{}@{version}", item.name);
        let key = format!(
            "{identity}\0{}",
            integrity_key(
                item.integrity.as_deref(),
                by_identity.get(&(item.name.clone(), version.clone())).copied(),
                &identity
            )?
        );
        selected.insert(
            key.clone(),
            SelectedPackage { identity, integrity: key.split('\0').nth(1).unwrap_or("").to_string() },
        );
    }
    let mut out: Vec<SelectedPackage> = selected.into_values().collect();
    out.sort_by_key(|a| (a.identity.clone(), a.integrity.clone()));
    Ok(out)
}

fn selection_key(item: &SelectedPackage) -> String {
    format!("{}\0{}", item.identity, item.integrity)
}

fn compare_selections(expected: &[SelectedPackage], actual: &[SelectedPackage], project_id: &str, manager: &Manager) -> Result<()> {
    let expected_set: FxHashSet<String> = expected.iter().map(selection_key).collect();
    let actual_set: FxHashSet<String> = actual.iter().map(selection_key).collect();
    let mut missing: Vec<String> = expected_set.difference(&actual_set).cloned().collect();
    let mut extra: Vec<String> = actual_set.difference(&expected_set).cloned().collect();
    missing.sort();
    extra.sort();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(Error::Conflict(format!(
            "{} validation changed package/version/integrity selections for {project_id}; missing {}, added {}",
            manager.as_str(),
            missing.len(),
            extra.len()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct StaticRequest {
    method: String,
    pathname: String,
    prefix: Option<String>,
    status: Option<u16>,
    content_type: Option<String>,
    response_bytes: usize,
    response_sha512: Option<String>,
    complete: bool,
}

struct StaticServer {
    port: u16,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StaticServer {
    fn close(mut self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn safe_static_path(pathname: &str, mount_prefix: &str) -> Option<Vec<String>> {
    if mount_prefix != "/" && !pathname.starts_with(mount_prefix) {
        return None;
    }
    let relative = if mount_prefix == "/" { pathname.strip_prefix('/').unwrap_or(pathname) } else { pathname.strip_prefix(mount_prefix).unwrap_or("") };
    let segments: Vec<String> = relative.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    if segments.iter().any(|s| s == "." || s == ".." || s.contains('\\')) {
        return None;
    }
    Some(segments)
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Error",
    }
}

fn start_static_server(mirror_dir: PathBuf, requests: Arc<Mutex<Vec<StaticRequest>>>, shutdown: Arc<std::sync::atomic::AtomicBool>) -> Result<StaticServer> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| Error::Operation(format!("could not start local validation server: {e}")))?;
    listener.set_nonblocking(true).map_err(|e| Error::Operation(format!("could not start local validation server: {e}")))?;
    let port = listener.local_addr().map_err(|e| Error::Operation(format!("could not start local validation server: {e}")))?.port();
    let thread_requests = requests.clone();
    let thread_shutdown = shutdown.clone();
    let handle = std::thread::spawn(move || {
        while !thread_shutdown.load(std::sync::atomic::Ordering::SeqCst) {
            let (stream, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                }
            };
            if stream.set_nonblocking(false).is_err() {
                continue;
            }
            let mirror = mirror_dir.clone();
            let thread_requests = thread_requests.clone();
            std::thread::spawn(move || handle_connection(stream, &mirror, &thread_requests));
        }
    });
    Ok(StaticServer { port, shutdown, handle: Some(handle) })
}

fn handle_connection(mut stream: std::net::TcpStream, mirror_dir: &Path, requests: &Arc<Mutex<Vec<StaticRequest>>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > 65536 {
                    return;
                }
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let head = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let pathname = target.split('?').next().unwrap_or("").to_string();
    let decoded = percent_decode(&pathname).unwrap_or_else(|| pathname.clone());
    let prefix = if decoded.starts_with("/mirror/") {
        Some("/mirror/".to_string())
    } else if decoded.starts_with("/current/") || decoded.starts_with("/tarballs/") {
        Some("/".to_string())
    } else {
        None
    };
    let mut record = StaticRequest {
        method: method.clone(),
        pathname: decoded.clone(),
        prefix: prefix.clone(),
        status: None,
        content_type: None,
        response_bytes: 0,
        response_sha512: None,
        complete: false,
    };
    let mut send = |status: u16, content_type: Option<&str>, body: Option<&[u8]>, cache: Option<&str>, is_head: bool| {
        use sha2::Digest;
        record.status = Some(status);
        record.content_type = content_type.map(|s| s.to_string());
        let out_body = if is_head { None } else { body };
        record.response_bytes = out_body.map(|b| b.len()).unwrap_or(0);
        record.response_sha512 = out_body.map(|b| {
            let mut h = Sha512::new();
            h.update(b);
            crate::lower_hex(h.finalize())
        });
        let body_len = body.map(|b| b.len()).unwrap_or(0);
        let mut response = format!(
            "HTTP/1.1 {status} {}\r\nConnection: close\r\nContent-Length: {body_len}\r\n",
            reason_phrase(status)
        );
        if let Some(ct) = content_type {
            response.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if let Some(cc) = cache {
            response.push_str(&format!("Cache-Control: {cc}\r\n"));
        }
        response.push_str("\r\n");
        let _ = stream.write_all(response.as_bytes());
        if let Some(b) = out_body {
            let _ = stream.write_all(b);
        }
        let _ = stream.flush();
        record.complete = true;
        if let Ok(mut log) = requests.lock() {
            log.push(record.clone());
        }
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
        let mut discard = [0u8; 512];
        while matches!(stream.read(&mut discard), Ok(n) if n > 0) {}
    };
    let is_head = method == "HEAD";
    if method != "GET" && !is_head {
        send(405, None, None, None, false);
        return;
    }
    if target.is_empty() {
        send(400, None, None, None, false);
        return;
    }
    let Some(prefix) = prefix else {
        send(404, None, None, None, false);
        return;
    };
    let Some(segments) = safe_static_path(&decoded, &prefix) else {
        send(404, None, None, None, false);
        return;
    };
    if segments.is_empty() {
        send(404, None, None, None, false);
        return;
    }
    let is_legacy_tarball = segments[0] == "tarballs" && segments.len() == 2;
    let is_nested_tarball = segments.len() == 4 && segments[0] == "current" && segments[1] == "-" && segments[2] == "tarballs";
    let is_renderer_two_tarball = segments.len() == 3 && segments[0] == "current" && segments[1] == "tarballs";
    let is_metadata = segments[0] == "current" && segments.len() >= 2 && segments[1] != "-";
    if !is_legacy_tarball && !is_nested_tarball && !is_renderer_two_tarball && !is_metadata {
        send(404, None, None, None, false);
        return;
    }
    let mut file_path = mirror_dir.to_path_buf();
    for segment in &segments {
        file_path.push(segment);
    }
    let root = mirror_dir.to_string_lossy().to_string();
    let joined = file_path.to_string_lossy().to_string();
    if joined != root && !joined.starts_with(&format!("{root}/")) {
        send(404, None, None, None, false);
        return;
    }
    let bytes = match std::fs::read(&file_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            send(404, None, None, None, false);
            return;
        }
        Err(_) => {
            send(500, None, None, None, false);
            return;
        }
    };
    if is_metadata {
        send(200, Some("application/json"), Some(&bytes), Some("no-cache"), is_head);
    } else {
        send(200, Some("application/octet-stream"), Some(&bytes), Some("public, max-age=31536000, immutable"), is_head);
    }
}

fn check_client_version(client: &Manager, target: &Target, cwd: &str, env: &HashMap<String, String>) -> Result<String> {
    let expected = client.pinned_version();
    let node_out = run_process(
        "node",
        &["--version".to_string()],
        &ProcessOptions {
            cwd: cwd.to_string(),
            env: env.clone(),
            timeout_ms: 30_000,
            stdout_mode: StdoutMode::Tail(16384),
        },
    )
    .map_err(|e| Error::Operation(format!("could not check Node.js version: {e}")))?;
    let node_version = node_out.stdout.split_whitespace().next().unwrap_or("").to_string();
    if node_version != format!("v{}", target.node()) {
        return Err(Error::Operation(format!(
            "{} validation requires Node.js {}; found {node_version}",
            client.as_str(),
            target.node()
        )));
    }
    let result = run_process(
        client.as_str(),
        &["--version".to_string()],
        &ProcessOptions {
            cwd: cwd.to_string(),
            env: env.clone(),
            timeout_ms: 30_000,
            stdout_mode: StdoutMode::Tail(16384),
        },
    )?;
    let version = result.stdout.split_whitespace().next().unwrap_or("").to_string();
    if version != expected {
        return Err(Error::Operation(format!(
            "{} {expected} is required for validation; found {}",
            client.as_str(),
            if version.is_empty() { "no version output".to_string() } else { version }
        )));
    }
    Ok(version)
}

fn canonical_text(value: &deser_value::Value) -> Result<String> {
    crate::inventory::canonical_string(value)
}

fn parse_project_lock(manager: &Manager, text: &str, options: LockParseOptions) -> Result<ProjectRecord> {
    if *manager == Manager::Npm {
        parse_npm_lock(
            text,
            NpmOptions {
                project_id: options.project_id,
                package_manager_pin: options.package_manager_pin,
                lockfile_path: options.lockfile_path,
                target_ids: options.target_ids,
                snapshot: options.snapshot,
            },
        )
        .map_err(|e| Error::Input(format!("could not parse npm package-lock.json: {e}")))
    } else {
        parse_pnpm_lock(
            text,
            PnpmOptions {
                project_id: options.project_id,
                package_manager_pin: options.package_manager_pin,
                lockfile_path: options.lockfile_path,
                target_ids: options.target_ids,
                snapshot: options.snapshot,
            },
        )
        .map_err(|e| match e {
            Error::Input(_) => e,
            other => Error::Input(format!("could not parse pnpm lockfile: {other}")),
        })
    }
}

struct LockParseOptions {
    project_id: String,
    package_manager_pin: Option<String>,
    lockfile_path: String,
    target_ids: Vec<String>,
    snapshot: Option<crate::inventory::ProjectSnapshot>,
}

fn project_manifest_text(project: &ProjectRecord) -> Result<String> {
    project
        .snapshot
        .as_ref()
        .and_then(|s| s.manifest.clone())
        .ok_or_else(|| Error::Input(format!("validation requires an imported package.json snapshot for {}", project.id)))
}

fn assert_snapshot_restorable(project: &ProjectRecord) -> Result<()> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires an imported package.json snapshot for {}", project.id))
    })?;
    let files: FxHashMap<&str, &str> = snapshot
        .workspace_files
        .iter()
        .map(|f| (f.path.as_str(), f.contents.as_str()))
        .collect();
    if project.manager() == Manager::Npm {
        let lock: deser_value::Value = deser_json::from_str(&snapshot.lockfile)
            .map_err(|_| Error::Corrupt(format!("saved npm lockfile for {}", project.id)))?;
        let root = lock.as_map().ok_or_else(|| Error::Corrupt(format!("saved npm lockfile for {}", project.id)))?;
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let packages = root.get("packages").unwrap_or(&null);
        if !crate::value::is_null(packages) {
            let pmap = packages.as_map().ok_or_else(|| Error::Corrupt(format!("saved npm lockfile for {}", project.id)))?;
            for i in 0..pmap.len() {
                let Some((_, entry_value)) = pmap.get_index(i) else { continue };
                let Some(entry) = entry_value.as_map() else { continue };
                let is_link = entry.get("link").and_then(|v| v.as_bool()) == Some(true);
                let resolved = entry.get("resolved").and_then(|v| v.as_str()).unwrap_or("");
                if !is_link || resolved.is_empty() {
                    continue;
                }
                let workspace_path = safe_local_path(resolved).ok_or_else(|| {
                    Error::Input(format!("validation cannot restore local workspace {resolved} for {}", project.id))
                })?;
                if !files.contains_key(format!("{workspace_path}/package.json").as_str()) {
                    return Err(Error::Input(format!(
                        "validation cannot restore local workspace {resolved} for {}",
                        project.id
                    )));
                }
            }
        }
        for item in &project.packages {
            if item.source_kind == crate::inventory::SourceKind::Local && !item.flags.link && item.version.is_some() {
                return Err(Error::Input(format!(
                    "validation cannot restore local file package {}@{} for {}",
                    item.name,
                    item.version.clone().unwrap_or_default(),
                    project.id
                )));
            }
        }
    } else {
        for importer in project.importers.iter().filter(|i| *i != ".") {
            if !files.contains_key(format!("{importer}/package.json").as_str()) {
                return Err(Error::Input(format!(
                    "validation cannot restore pnpm workspace importer {importer} for {}",
                    project.id
                )));
            }
        }
        if project.importers.iter().any(|i| i != ".") && !files.contains_key("pnpm-workspace.yaml") {
            return Err(Error::Input(format!(
                "validation cannot restore pnpm workspace configuration for {}",
                project.id
            )));
        }
        for item in &project.packages {
            if item.source_kind == crate::inventory::SourceKind::Local && !item.flags.link && item.version.is_some() {
                return Err(Error::Input(format!(
                    "validation cannot restore local pnpm package {}@{} for {}",
                    item.name,
                    item.version.clone().unwrap_or_default(),
                    project.id
                )));
            }
        }
    }
    Ok(())
}

fn safe_local_path(value: &str) -> Option<String> {
    let normalized = value.strip_prefix("file:").unwrap_or(value).strip_prefix("./").unwrap_or(value.strip_prefix("file:").unwrap_or(value));
    if normalized.starts_with('/') || normalized.contains('\\') || normalized.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return None;
    }
    Some(normalized.to_string())
}

fn assert_snapshot_matches_project(project: &ProjectRecord, target: &Target) -> Result<()> {
    assert_snapshot_restorable(project).map_err(|e| {
        Error::Input(format!("saved project snapshot is not restorable for {}: {e}", project.id))
    })?;
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires an imported snapshot for {}", project.id))
    })?;
    assert_restorable_snapshot(&project.manager(), snapshot)
        .map_err(|e| Error::Input(format!("saved project configuration is not restorable for {}: {e}", project.id)))?;
    let restored = parse_project_lock(
        &project.manager(),
        &snapshot.lockfile,
        LockParseOptions {
            project_id: project.id.clone(),
            package_manager_pin: project.package_manager_pin.clone(),
            lockfile_path: project.lockfile_path.clone(),
            target_ids: project.targets.clone(),
            snapshot: None,
        },
    )?;
    let mut without_restored = restored.clone();
    without_restored.snapshot = None;
    let mut without_project = project.clone();
    without_project.snapshot = None;
    if canonical_text(&crate::inventory::project_to_value(&without_restored)?)?
        != canonical_text(&crate::inventory::project_to_value(&without_project)?)?
    {
        return Err(Error::Corrupt(format!(
            "saved {} lockfile no longer matches imported project {}",
            project.manager().as_str(),
            project.id
        )));
    }
    let manifest_text = project_manifest_text(project)?;
    let manifest: deser_value::Value = deser_json::from_str(&manifest_text)
        .map_err(|_| Error::Corrupt(format!("saved package.json for {}", project.id)))?;
    let pin = manifest.as_map().and_then(|m| m.get("packageManager")).and_then(|v| v.as_str());
    if pin != project.package_manager_pin.as_deref() {
        return Err(Error::Corrupt(format!(
            "saved package.json pin no longer matches imported project {}",
            project.id
        )));
    }
    let expected_pin = format!("{}@{}", project.manager().as_str(), project.manager().pinned_version());
    if let Some(ref commit_pin) = project.package_manager_pin {
        let base = commit_pin.split('+').next().unwrap_or("");
        if base != expected_pin {
            return Err(Error::Input(format!(
                "validation requires {expected_pin}; project {} is pinned to {commit_pin}",
                project.id
            )));
        }
    }
    if let Some(unsupported) = project.packages.iter().find(|item| {
        item.version.is_some()
            && supports_target(&item.platform, target)
            && item.source_kind != crate::inventory::SourceKind::Registry
            && item.source_kind != crate::inventory::SourceKind::Local
    }) {
        return Err(Error::Input(format!(
            "validation rejects {} source {}@{}",
            unsupported.source_kind.as_str(),
            unsupported.name,
            unsupported.version.clone().unwrap_or_default()
        )));
    }
    Ok(())
}

fn package_artifact<'a>(_project: &ProjectRecord, item: &PackageRecord, artifacts: &'a [ArtifactRecord]) -> Result<&'a ArtifactRecord> {
    let found = artifacts.iter().find(|r| r.name == item.name && Some(r.version.clone()) == item.version);
    integrity_key(
        item.integrity.as_deref(),
        found,
        &format!("{}@{}", item.name, item.version.clone().unwrap_or_default()),
    )?;
    found.ok_or_else(|| {
        Error::Corrupt(format!("no verified artifact is available for {}@{}", item.name, item.version.clone().unwrap_or_default()))
    })
}

fn local_tarball_url(registry: &str, digest: &str) -> String {
    format!("{registry}-/tarballs/{digest}.tgz")
}

fn unavailable_tarball_url(registry: &str) -> String {
    format!("{registry}-/tarballs/unavailable.tgz")
}

fn rewrite_npm_lock(project: &ProjectRecord, target: &Target, artifacts: &[ArtifactRecord], registry: &str) -> Result<String> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires a saved snapshot for {}", project.id))
    })?;
    let mut lock: deser_value::Value = deser_json::from_str(&snapshot.lockfile)
        .map_err(|_| Error::Corrupt(format!("saved npm lockfile for {}", project.id)))?;
    let lock_map = lock.as_map_mut().ok_or_else(|| Error::Corrupt(format!("saved npm lockfile for {}", project.id)))?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let packages_val = lock_map.get("packages").unwrap_or(&null).clone();
    let packages = packages_val.as_map().ok_or_else(|| Error::Corrupt(format!("saved npm lockfile has no package entries for {}", project.id)))?;
    let mut packages_mut = packages.clone();
    for item in &project.packages {
        let Some(ref version) = item.version else { continue };
        if item.source_kind == crate::inventory::SourceKind::Local {
            continue;
        }
        if item.source_kind != crate::inventory::SourceKind::Registry {
            return Err(Error::Input(format!(
                "validation cannot migrate {} source {}@{version}",
                item.source_kind.as_str(),
                item.name
            )));
        }
        let entry_val = packages_mut.get(item.key.as_str()).cloned().unwrap_or_else(|| deser_value::Value::from(deser_value::Kind::Null));
        let entry = entry_val.as_map().cloned().ok_or_else(|| {
            Error::Corrupt(format!("saved npm lockfile is missing {}", item.key))
        })?;
        let mut entry = entry;
        let artifact = package_artifact(project, item, artifacts)?;
        entry.insert(
            "resolved".to_string(),
            deser_value::Value::from(if supports_target(&item.platform, target) {
                local_tarball_url(registry, &artifact.artifact.sha512)
            } else {
                unavailable_tarball_url(registry)
            }),
        );
        packages_mut.insert(item.key.clone(), deser_value::Value::from(entry));
    }
    if let Some(lock_map) = lock.as_map_mut() {
        lock_map.insert("packages".to_string(), deser_value::Value::from(packages_mut.clone()));
        rewrite_npm_legacy(lock_map, project, target, artifacts, registry, &packages_mut)?;
    }
    crate::inventory::canonical_json(&lock)
}

fn rewrite_npm_legacy(
    lock_map: &mut deser_value::Map,
    project: &ProjectRecord,
    target: &Target,
    artifacts: &[ArtifactRecord],
    registry: &str,
    _packages: &deser_value::Map,
) -> Result<()> {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    if lock_map.get("dependencies").unwrap_or(&null).as_map().is_none() {
        return Ok(());
    }
    let by_key: FxHashMap<&str, &PackageRecord> = project.packages.iter().map(|p| (p.key.as_str(), p)).collect();
    fn visit(
        entries: &mut deser_value::Map,
        parent: &str,
        project: &ProjectRecord,
        by_key: &FxHashMap<&str, &PackageRecord>,
        target: &Target,
        artifacts: &[ArtifactRecord],
        registry: &str,
    ) -> Result<()> {
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let keys: Vec<String> = (0..entries.len())
            .filter_map(|i| entries.get_index(i).and_then(|(k, _)| k.as_str().map(|s| s.to_string())))
            .collect();
        for requested in keys {
            let key = if parent.is_empty() {
                format!("node_modules/{requested}")
            } else {
                format!("{parent}/node_modules/{requested}")
            };
            {
                let entry_val = entries.get(requested.as_str()).unwrap_or(&null).clone();
                let Some(entry) = entry_val.as_map().cloned() else { continue };
                let mut entry = entry;
                if let Some(item) = by_key.get(key.as_str())
                    && item.version.is_some()
                    && item.source_kind == crate::inventory::SourceKind::Registry {
                        let artifact = package_artifact(project, item, artifacts)?;
                        entry.insert(
                            "resolved".to_string(),
                            deser_value::Value::from(if supports_target(&item.platform, target) {
                                local_tarball_url(registry, &artifact.artifact.sha512)
                            } else {
                                unavailable_tarball_url(registry)
                            }),
                        );
                        entries.insert(requested.clone(), deser_value::Value::from(entry.clone()));
                    }
                if entry.get("dependencies").and_then(|v| v.as_map()).is_some() {
                    let mut sub = entry.get("dependencies").expect("deps").as_map().cloned().expect("map");
                    visit(&mut sub, &key, project, by_key, target, artifacts, registry)?;
                    let mut updated = entry;
                    updated.insert("dependencies".to_string(), deser_value::Value::from(sub));
                    entries.insert(requested.clone(), deser_value::Value::from(updated));
                }
            }
        }
        Ok(())
    }
    let mut deps = lock_map.get("dependencies").expect("deps").as_map().cloned().expect("map");
    visit(&mut deps, "", project, &by_key, target, artifacts, registry)?;
    lock_map.insert("dependencies".to_string(), deser_value::Value::from(deps));
    Ok(())
}

fn normalize_npm_legacy(lock_map: &mut deser_value::Map, project: &ProjectRecord) -> Result<()> {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    if lock_map.get("dependencies").unwrap_or(&null).as_map().is_none() {
        return Ok(());
    }
    let by_key: FxHashMap<&str, &PackageRecord> = project.packages.iter().map(|p| (p.key.as_str(), p)).collect();
    fn visit(entries: &mut deser_value::Map, parent: &str, by_key: &FxHashMap<&str, &PackageRecord>) {
        let keys: Vec<String> = (0..entries.len())
            .filter_map(|i| entries.get_index(i).and_then(|(k, _)| k.as_str().map(|s| s.to_string())))
            .collect();
        for requested in keys {
            let key = if parent.is_empty() {
                format!("node_modules/{requested}")
            } else {
                format!("{parent}/node_modules/{requested}")
            };
            let null = deser_value::Value::from(deser_value::Kind::Null);
            let entry_val = entries.get(requested.as_str()).unwrap_or(&null).clone();
            let Some(entry) = entry_val.as_map().cloned() else { continue };
            let mut entry = entry;
            if let Some(item) = by_key.get(key.as_str())
                && item.version.is_some()
                && item.source_kind == crate::inventory::SourceKind::Registry {
                    entry.insert("resolved".to_string(), deser_value::Value::from("mynpm-local-tarball".to_string()));
                    entries.insert(requested.clone(), deser_value::Value::from(entry.clone()));
                }
            if entry.get("dependencies").and_then(|v| v.as_map()).is_some() {
                let mut sub = entry.get("dependencies").expect("d").as_map().cloned().expect("m");
                visit(&mut sub, &key, by_key);
                let mut updated = entry;
                updated.insert("dependencies".to_string(), deser_value::Value::from(sub));
                entries.insert(requested, deser_value::Value::from(updated));
            }
        }
    }
    let mut deps = lock_map.get("dependencies").expect("d").as_map().cloned().expect("m");
    visit(&mut deps, "", &by_key);
    lock_map.insert("dependencies".to_string(), deser_value::Value::from(deps));
    Ok(())
}

fn rewrite_pnpm_lock(project: &ProjectRecord, target: &Target, artifacts: &[ArtifactRecord], registry: &str) -> Result<String> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires a saved snapshot for {}", project.id))
    })?;
    let values = crate::lock_pnpm::decode_pnpm_documents(&snapshot.lockfile)
        .map_err(|e| Error::Corrupt(format!("could not migrate saved pnpm lockfile for {}: {e}", project.id)))?;
    let by_key: FxHashMap<&str, &PackageRecord> = project.packages.iter().map(|p| (p.key.as_str(), p)).collect();
    let mut docs = values;
    for (document_index, document) in docs.iter_mut().enumerate() {
        let Some(doc_map) = document.as_map_mut() else { continue };
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let packages_val = doc_map.get("packages").unwrap_or(&null).clone();
        let Some(packages) = packages_val.as_map().cloned() else { continue };
        let mut packages_mut = packages;
        let keys: Vec<String> = (0..packages_mut.len())
            .filter_map(|i| packages_mut.get_index(i).and_then(|(k, _)| k.as_str().map(|s| s.to_string())))
            .collect();
        for key in keys {
            let Some(item) = by_key.get(key.as_str()) else { continue };
            if item.version.is_none() || item.source_kind == crate::inventory::SourceKind::Local {
                continue;
            }
            if item.source_kind != crate::inventory::SourceKind::Registry {
                if supports_target(&item.platform, target) {
                    return Err(Error::Input(format!(
                        "validation cannot migrate {} source {}@{}",
                        item.source_kind.as_str(),
                        item.name,
                        item.version.clone().unwrap_or_default()
                    )));
                }
                continue;
            }
            let entry_val = packages_mut.get(key.as_str()).unwrap_or(&null).clone();
            let Some(entry) = entry_val.as_map().cloned() else { continue };
            let mut entry = entry;
            let resolution_val = entry.get("resolution").unwrap_or(&null).clone();
            let mut resolution = resolution_val.as_map().cloned().ok_or_else(|| {
                Error::Corrupt(format!(
                    "saved pnpm lockfile package resolution for {}@{}",
                    item.name,
                    item.version.clone().unwrap_or_default()
                ))
            })?;
            let artifact = package_artifact(project, item, artifacts)?;
            resolution.insert(
                "tarball".to_string(),
                deser_value::Value::from(if supports_target(&item.platform, target) {
                    local_tarball_url(registry, &artifact.artifact.sha512)
                } else {
                    unavailable_tarball_url(registry)
                }),
            );
            entry.insert("resolution".to_string(), deser_value::Value::from(resolution));
            packages_mut.insert(key.clone(), deser_value::Value::from(entry));
            let _ = document_index;
        }
        if let Some(doc_map) = document.as_map_mut() {
            doc_map.insert("packages".to_string(), deser_value::Value::from(packages_mut));
        }
    }
    let marker = if snapshot.lockfile.lines().next().is_some_and(|l| {
        let t = l.trim_start();
        t == "---" || t.starts_with("--- ")
    }) {
        "---\n"
    } else {
        ""
    };
    let mut out = String::from(marker);
    for (i, doc) in docs.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        out.push_str(&deser_yaml::to_string(doc).map_err(|_| {
            Error::Corrupt(format!("could not migrate saved pnpm lockfile for {}", project.id))
        })?);
    }
    Ok(out)
}

fn normalize_pnpm_tarballs(values: &mut [deser_value::Value], project: &ProjectRecord) -> Result<()> {
    for item in &project.packages {
        if item.version.is_none() || item.source_kind != crate::inventory::SourceKind::Registry {
            continue;
        }
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let mut found = false;
        for document in values.iter_mut() {
            let Some(doc) = document.as_map_mut() else { continue };
            let packages_val = doc.get("packages").unwrap_or(&null).clone();
            let Some(mut packages) = packages_val.as_map().cloned() else { continue };
            if let Some(entry_val) = packages.get(item.key.as_str()).cloned()
                && let Some(mut entry) = entry_val.as_map().cloned()
                && let Some(mut resolution) = entry.get("resolution").and_then(|v| v.as_map()).cloned() {
                    resolution.insert("tarball".to_string(), deser_value::Value::from("mynpm-local-tarball".to_string()));
                    entry.insert("resolution".to_string(), deser_value::Value::from(resolution));
                    packages.insert(item.key.clone(), deser_value::Value::from(entry));
                    doc.insert("packages".to_string(), deser_value::Value::from(packages));
                    found = true;
                }
        }
        if !found {
            return Err(Error::Corrupt(format!(
                "saved pnpm lockfile is missing {}@{}",
                item.name,
                item.version.clone().unwrap_or_default()
            )));
        }
    }
    Ok(())
}

fn assert_migration_preserves_graph(project: &ProjectRecord, migrated_lock: &str) -> Result<()> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires a saved snapshot for {}", project.id))
    })?;
    if project.manager() == Manager::Npm {
        let mut original: deser_value::Value = deser_json::from_str(&snapshot.lockfile)
            .map_err(|_| Error::Corrupt(format!("saved npm lockfile for {}", project.id)))?;
        let mut migrated: deser_value::Value = deser_json::from_str(migrated_lock)
            .map_err(|_| Error::Corrupt(format!("migrated npm lockfile for {}", project.id)))?;
        normalize_npm_lockfiles(&mut original, &mut migrated, project)?;
        if canonical_text(&original)? != canonical_text(&migrated)? {
            return Err(Error::Conflict(format!(
                "registry migration changed the saved npm dependency graph for {}",
                project.id
            )));
        }
        return Ok(());
    }
    let mut original = crate::lock_pnpm::decode_pnpm_documents(&snapshot.lockfile)
        .map_err(|e| Error::Conflict(format!("registry migration changed the saved pnpm dependency graph for {}: {e}", project.id)))?;
    let mut migrated = crate::lock_pnpm::decode_pnpm_documents(migrated_lock)
        .map_err(|e| Error::Conflict(format!("registry migration changed the saved pnpm dependency graph for {}: {e}", project.id)))?;
    normalize_pnpm_tarballs(&mut original, project)?;
    normalize_pnpm_tarballs(&mut migrated, project)?;
    if canonical_text(&deser_value::Value::from(values_to_seq(&original)))?
        != canonical_text(&deser_value::Value::from(values_to_seq(&migrated)))?
    {
        return Err(Error::Conflict(format!(
            "registry migration changed the saved pnpm dependency graph for {}",
            project.id
        )));
    }
    Ok(())
}

fn values_to_seq(values: &[deser_value::Value]) -> deser_value::Seq {
    let mut seq = deser_value::Seq::new();
    for v in values {
        seq.push(v.clone());
    }
    seq
}

fn normalize_npm_lockfiles(original: &mut deser_value::Value, migrated: &mut deser_value::Value, project: &ProjectRecord) -> Result<()> {
    for lock in [original, migrated] {
        let Some(root) = lock.as_map_mut() else { continue };
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let packages_val = root.get("packages").unwrap_or(&null).clone();
        let Some(mut packages) = packages_val.as_map().cloned() else { continue };
        for item in &project.packages {
            if item.version.is_none() || item.source_kind != crate::inventory::SourceKind::Registry {
                continue;
            }
            if let Some(entry_val) = packages.get(item.key.as_str()).cloned()
                && let Some(mut entry) = entry_val.as_map().cloned() {
                    entry.insert("resolved".to_string(), deser_value::Value::from("mynpm-local-tarball".to_string()));
                    packages.insert(item.key.clone(), deser_value::Value::from(entry));
                }
        }
        root.insert("packages".to_string(), deser_value::Value::from(packages.clone()));
        normalize_npm_legacy(root, project)?;
    }
    Ok(())
}

fn lock_graph_text(manager: &Manager, text: &str, project: &ProjectRecord) -> Result<deser_value::Value> {
    if *manager == Manager::Npm {
        let mut lock: deser_value::Value = deser_json::from_str(text)
            .map_err(|_| Error::Input(format!("could not read npm lockfile for {}", project.id)))?;
        normalize_npm_graph(&mut lock, project)?;
        Ok(lock)
    } else {
        let mut values = crate::lock_pnpm::decode_pnpm_documents(text)
            .map_err(|_| Error::Corrupt(format!("could not read pnpm lockfile for {}", project.id)))?;
        normalize_pnpm_tarballs(&mut values, project)?;
        Ok(deser_value::Value::from(values_to_seq(&values)))
    }
}

fn normalize_npm_graph(lock: &mut deser_value::Value, project: &ProjectRecord) -> Result<()> {
    let Some(root) = lock.as_map_mut() else {
        return Err(Error::Input(format!("could not read npm lockfile for {}", project.id)));
    };
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let packages_val = root.get("packages").unwrap_or(&null).clone();
    if packages_val.as_map().is_none() {
        return Err(Error::Corrupt(format!("saved npm lockfile has no package entries for {}", project.id)));
    }
    let mut packages = packages_val.as_map().cloned().expect("packages");
    for item in &project.packages {
        if item.version.is_none() || item.source_kind != crate::inventory::SourceKind::Registry {
            continue;
        }
        if !packages.get(item.key.as_str()).and_then(|v| v.as_map()).is_some() {
            return Err(Error::Corrupt(format!("saved npm lockfile is missing {}", item.key)));
        }
        if let Some(mut entry) = packages.get(item.key.as_str()).and_then(|v| v.as_map()).cloned() {
            entry.insert("resolved".to_string(), deser_value::Value::from("mynpm-local-tarball".to_string()));
            packages.insert(item.key.clone(), deser_value::Value::from(entry));
        }
    }
    root.insert("packages".to_string(), deser_value::Value::from(packages));
    normalize_npm_legacy(root, project)?;
    Ok(())
}

fn assert_lockfile_matches(manager: &Manager, expected_text: &str, actual_text: &str, project: &ProjectRecord) -> Result<()> {
    let expected = lock_graph_text(manager, expected_text, project)?;
    let actual = lock_graph_text(manager, actual_text, project)?;
    if canonical_text(&expected)? != canonical_text(&actual)? {
        return Err(Error::Conflict(format!(
            "{} changed the saved dependency graph for {}",
            manager.as_str(),
            project.id
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct ExpectedInstance {
    key: String,
    name: String,
    version: String,
    optional: bool,
}

fn expected_npm_instances(project: &ProjectRecord, target: &Target) -> Result<(Vec<ExpectedInstance>, usize, FxHashSet<String>)> {
    let mut skipped = 0;
    let mut skipped_identities = FxHashSet::default();
    let mut instances = Vec::new();
    for item in &project.packages {
        if !item.key.starts_with("node_modules/") || item.version.is_none() {
            continue;
        }
        let version = item.version.clone().unwrap_or_default();
        if !supports_target(&item.platform, target) {
            if !item.flags.optional {
                return Err(Error::Conflict(format!(
                    "required npm package {}@{version} does not support target {}",
                    item.name,
                    target.id()
                )));
            }
            skipped += 1;
            skipped_identities.insert(format!("{}\0{version}", item.name));
            continue;
        }
        instances.push(ExpectedInstance {
            key: item.key.clone(),
            name: item.name.clone(),
            version,
            optional: item.flags.optional,
        });
    }
    Ok((instances, skipped, skipped_identities))
}

fn expected_pnpm_instances(project: &ProjectRecord, target: &Target) -> Result<(Vec<ExpectedInstance>, usize, FxHashSet<String>)> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires a saved snapshot for {}", project.id))
    })?;
    let values = crate::lock_pnpm::decode_pnpm_documents(&snapshot.lockfile)
        .map_err(|_| Error::Corrupt(format!("could not inspect saved pnpm installation graph for {}", project.id)))?;
    let by_key: FxHashMap<String, &PackageRecord> = project
        .packages
        .iter()
        .map(|p| (p.key.strip_prefix('/').unwrap_or(&p.key).to_string(), p))
        .collect();
    let mut snapshots: FxHashMap<String, deser_value::Value> = FxHashMap::default();
    for (document_index, document) in values.iter().enumerate() {
        let Some(doc) = document.as_map() else { continue };
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let snaps_val = doc.get("snapshots").unwrap_or(&null).clone();
        let Some(snaps) = snaps_val.as_map() else { continue };
        for i in 0..snaps.len() {
            if let Some((k, v)) = snaps.get_index(i) {
                let key = k.as_str().unwrap_or("");
                let norm = key.strip_prefix('/').unwrap_or(key).to_string();
                if v.as_map().is_none() {
                    return Err(Error::Corrupt(format!("saved pnpm snapshot {key}")));
                }
                let _ = document_index;
                snapshots.insert(norm, v.clone());
            }
        }
    }
    let mut skipped = 0;
    let mut skipped_identities = FxHashSet::default();
    let mut instances = Vec::new();
    let mut snapshot_entries: Vec<_> = snapshots.iter().collect();
    snapshot_entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (key, snapshot_value) in snapshot_entries {
        let artifact_key = key.split('(').next().unwrap_or("").to_string();
        let Some(item) = by_key.get(&artifact_key) else {
            return Err(Error::Corrupt(format!("saved pnpm snapshot has no package entry: {key}")));
        };
        let Some(ref version) = item.version else { continue };
        if item.source_kind == crate::inventory::SourceKind::Local {
            continue;
        }
        let optional = item.flags.optional
            || snapshot_value.as_map().and_then(|m| m.get("optional")).and_then(|v| v.as_bool()) == Some(true);
        if !supports_target(&item.platform, target) {
            if !optional {
                return Err(Error::Conflict(format!(
                    "required pnpm package {}@{version} does not support target {}",
                    item.name,
                    target.id()
                )));
            }
            skipped += 1;
            skipped_identities.insert(format!("{}\0{version}", item.name));
            continue;
        }
        instances.push(ExpectedInstance {
            key: key.clone(),
            name: item.name.clone(),
            version: version.clone(),
            optional,
        });
    }
    Ok((instances, skipped, skipped_identities))
}

fn inspect_installed_package(project_dir: &Path, instance: &ExpectedInstance) -> Result<()> {
    let segments: Vec<&str> = instance.key.split('/').collect();
    if segments.iter().any(|s| s.is_empty() || *s == "." || *s == ".." || s.contains('\\')) {
        return Err(Error::Corrupt(format!(
            "saved lockfile contains an unsafe installed package path: {}",
            instance.key
        )));
    }
    let mut package_dir = project_dir.to_path_buf();
    for segment in &segments {
        package_dir.push(segment);
    }
    let resolved = std::fs::canonicalize(&package_dir).map_err(|_| {
        Error::Conflict(format!(
            "applicable {} package {}@{} is missing from the installed tree at {}",
            if instance.optional { "optional" } else { "required" },
            instance.name,
            instance.version,
            instance.key
        ))
    })?;
    let project_root = std::fs::canonicalize(project_dir)
        .map_err(|_| Error::Operation(format!("could not resolve restored project directory {}", project_dir.display())))?;
    if resolved != project_root && !resolved.starts_with(&project_root) {
        return Err(Error::Conflict(format!("installed package path leaves restored project: {}", instance.key)));
    }
    let manifest_path = resolved.join("package.json");
    let meta = std::fs::symlink_metadata(&manifest_path).map_err(|_| {
        Error::Conflict(format!(
            "installed package {}@{} has no package.json at {}",
            instance.name, instance.version, instance.key
        ))
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::Conflict(format!(
            "installed package {}@{} has an unsafe package.json at {}",
            instance.name, instance.version, instance.key
        )));
    }
    let text = std::fs::read_to_string(&manifest_path).map_err(|_| {
        Error::Conflict(format!("could not read installed package {}/package.json", instance.key))
    })?;
    let manifest: deser_value::Value = deser_json::from_str(&text)
        .map_err(|_| Error::Conflict(format!("installed package {} has invalid metadata", instance.key)))?;
    let mmap = manifest.as_map().ok_or_else(|| {
        Error::Conflict(format!("installed package {} has invalid metadata", instance.key))
    })?;
    let name = mmap.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let version = mmap.get("version").and_then(|v| v.as_str()).unwrap_or("");
    if name != instance.name || version != instance.version {
        return Err(Error::Conflict(format!(
            "installed package at {} is {}@{}, expected {}@{}",
            instance.key,
            if name.is_empty() { "unnamed" } else { name },
            if version.is_empty() { "unversioned" } else { version },
            instance.name,
            instance.version
        )));
    }
    Ok(())
}

fn report_nodes(value: &deser_value::Value, output: &mut Vec<deser_value::Value>, seen: &mut FxHashSet<String>, top_level: bool, dependency_name: Option<&str>) {
    if let Some(seq) = value.as_seq() {
        for item in seq.iter() {
            report_nodes(item, output, seen, true, None);
        }
        return;
    }
    let Some(map) = value.as_map() else { return };
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let path = map.get("path").unwrap_or(&null).as_str().map(|s| s.to_string());
    let name = map
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| dependency_name.map(|s| s.to_string()));
    if !top_level
        && let (Some(name), Some(version)) = (name.clone(), map.get("version").unwrap_or(&null).as_str())
    {
        let dominated = path.clone().is_some_and(|p| seen.contains(&p));
        if !dominated {
            let mut node = deser_value::Map::new();
            for i in 0..map.len() {
                if let Some((k, v)) = map.get_index(i) {
                    node.insert(k.clone(), v.clone());
                }
            }
            node.insert("name".to_string(), deser_value::Value::from(name));
            output.push(deser_value::Value::from(node));
            if let Some(p) = path {
                seen.insert(p);
            }
            let _ = version;
        }
    }
    if let Some(deps) = map.get("dependencies").and_then(|v| v.as_map()) {
        for i in 0..deps.len() {
            if let Some((child_name, child)) = deps.get_index(i) {
                report_nodes(child, output, seen, false, child_name.as_str());
            }
        }
    }
}

fn identity_counts(instances: &[(String, String)]) -> FxHashMap<String, usize> {
    let mut counts = FxHashMap::default();
    for (name, version) in instances {
        *counts.entry(format!("{name}\0{version}")).or_insert(0) += 1;
    }
    counts
}

fn run_package_report(
    client: &Manager,
    project_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<(Vec<deser_value::Value>, Vec<deser_value::Value>)> {
    let args: Vec<String> = if *client == Manager::Npm {
        ["ls", "--all", "--json", "--long", "--ignore-scripts"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        ["list", "--recursive", "--depth", "Infinity", "--json", "--ignore-pnpmfile"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    let result = run_process(
        client.as_str(),
        &args,
        &ProcessOptions {
            cwd: project_dir.to_string_lossy().to_string(),
            env: env.clone(),
            timeout_ms: CLIENT_TIMEOUT_MS,
            stdout_mode: StdoutMode::Full { max_bytes: limits::PACKAGE_REPORT_BYTES },
        },
    )
    .map_err(|e| {
        if e.to_string().contains("output exceeds the") {
            Error::Conflict(format!(
                "{} installed package report exceeds {} bytes",
                client.as_str(),
                limits::PACKAGE_REPORT_BYTES
            ))
        } else {
            Error::Conflict(format!(
                "{} reported an invalid or incomplete installed dependency graph for {}",
                client.as_str(),
                project_dir.display()
            ))
        }
    })?;
    let report: deser_value::Value = deser_json::from_str(&result.stdout)
        .map_err(|_| Error::Conflict(format!("{} returned an invalid installed package report", client.as_str())))?;
    let roots: Vec<deser_value::Value> = if let Some(seq) = report.as_seq() {
        seq.iter().cloned().collect()
    } else {
        vec![report]
    };
    let mut problems = Vec::new();
    for root in &roots {
        if let Some(map) = root.as_map() {
            let null = deser_value::Value::from(deser_value::Kind::Null);
            if let Some(prob_seq) = map.get("problems").unwrap_or(&null).as_seq() {
                for item in prob_seq.iter() {
                    if let Some(s) = item.as_str() {
                        problems.push(s.to_string());
                    }
                }
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Conflict(format!(
            "{} reports installed dependency problems: {}",
            client.as_str(),
            problems.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
        )));
    }
    let mut nodes = Vec::new();
    let mut seen = FxHashSet::default();
    for root in &roots {
        report_nodes(root, &mut nodes, &mut seen, true, None);
    }
    if *client == Manager::Pnpm {
        nodes.retain(|node| {
            node.as_map()
                .and_then(|m| {
                    let null = deser_value::Value::from(deser_value::Kind::Null);
                    m.get("path").unwrap_or(&null).as_str().map(|s| s.to_string())
                })
                .is_some_and(|p| p.contains("node_modules/.pnpm/") || p.contains("node_modules\\.pnpm\\"))
        });
    }
    Ok((roots, nodes))
}

fn verify_reported_links(
    roots: &[deser_value::Value],
    project_dir: &Path,
    skipped: &FxHashSet<String>,
) -> Result<()> {
    let project_root = std::fs::canonicalize(project_dir)
        .map_err(|_| Error::Operation(format!("could not resolve restored project directory {}", project_dir.display())))?;
    let mut visited: FxHashSet<PathBuf> = FxHashSet::default();
    let mut stack: Vec<deser_value::Value> = roots.to_vec();
    let mut order: Vec<deser_value::Value> = Vec::new();
    while let Some(node) = stack.pop() {
        order.push(node.clone());
        if let Some(deps) = node.as_map().and_then(|m| m.get("dependencies")).and_then(|v| v.as_map()) {
            for i in 0..deps.len() {
                if let Some((_, child)) = deps.get_index(i) {
                    stack.push(child.clone());
                }
            }
        }
    }
    order.reverse();
    for node in &order {
        let Some(map) = node.as_map() else { continue };
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let Some(parent_path) = map.get("path").unwrap_or(&null).as_str().map(|s| s.to_string()) else {
            continue;
        };
        let parent = std::fs::canonicalize(&parent_path)
            .map_err(|_| Error::Conflict("package report path is missing from the installed tree".to_string()))?;
        if parent != project_root && !parent.starts_with(&project_root) {
            return Err(Error::Conflict("package report path leaves the restored project".to_string()));
        }
        if !visited.insert(parent.clone()) {
            continue;
        }
        let Some(deps) = map.get("dependencies").and_then(|v| v.as_map()) else { continue };
        for i in 0..deps.len() {
            let Some((dep_name_val, child_val)) = deps.get_index(i) else { continue };
            let dep_name = dep_name_val.as_str().unwrap_or("").to_string();
            let Some(child) = child_val.as_map() else { continue };
            let Some(child_path) = child.get("path").unwrap_or(&null).as_str().map(|s| s.to_string()) else {
                continue;
            };
            let child_name = child
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&dep_name)
                .to_string();
            if let Some(child_version) = child.get("version").and_then(|v| v.as_str())
                && skipped.contains(&format!("{child_name}\0{child_version}")) {
                    continue;
                }
            if dep_name.split('/').any(|s| s.is_empty() || s == "." || s == ".." || s.contains('\\')) {
                return Err(Error::Conflict(format!(
                    "package report contains an unsafe dependency name: {dep_name}"
                )));
            }
            let child_real = std::fs::canonicalize(&child_path)
                .map_err(|_| Error::Conflict(format!("reported dependency {dep_name} is missing from the installed tree")))?;
            let mut search = parent.clone();
            let mut linked: Option<PathBuf> = None;
            loop {
                let mut candidate = search.join("node_modules");
                for segment in dep_name.split('/') {
                    candidate.push(segment);
                }
                match std::fs::symlink_metadata(&candidate) {
                    Ok(_) => {
                        let target = std::fs::canonicalize(&candidate).map_err(|_| {
                            Error::Conflict(format!("installed dependency link {} is broken", candidate.display()))
                        })?;
                        if target != child_real {
                            return Err(Error::Conflict(format!(
                                "installed dependency link {dep_name} points to a different package instance"
                            )));
                        }
                        linked = Some(candidate);
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(Error::Conflict(format!(
                            "could not inspect installed dependency link {}",
                            candidate.display()
                        )));
                    }
                }
                if search == project_root {
                    break;
                }
                match search.parent() {
                    Some(parent_search) => {
                        if parent_search == search {
                            break;
                        }
                        let dominated = parent_search != project_root && !parent_search.starts_with(&project_root);
                        search = parent_search.to_path_buf();
                        if dominated {
                            break;
                        }
                    }
                    None => break,
                }
            }
            if linked.is_none() {
                return Err(Error::Conflict(format!("installed dependency link {dep_name} is missing")));
            }
        }
    }
    Ok(())
}

fn assert_report_matches_expected(
    report_nodes: &[deser_value::Value],
    expected: &[ExpectedInstance],
    client: &Manager,
    project_id: &str,
    project_dir: &Path,
) -> Result<()> {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let by_path: FxHashMap<String, &ExpectedInstance> = expected.iter().map(|i| (i.key.clone(), i)).collect();
    let mut actual_instances: Vec<(String, String)> = Vec::new();
    for item in report_nodes {
        let Some(map) = item.as_map() else { continue };
        let Some(version) = map.get("version").unwrap_or(&null).as_str() else { continue };
        if let Some(path) = map.get("path").unwrap_or(&null).as_str() {
            let rel = pathdiff(&PathBuf::from(path), project_dir);
            if rel.starts_with("..") || rel.starts_with('/') {
                return Err(Error::Conflict(format!(
                    "{} package report points outside the restored project for {project_id}",
                    client.as_str()
                )));
            }
            let key = rel.replace('\\', "/");
            if let Some(installed) = by_path.get(&key) {
                actual_instances.push((installed.name.clone(), version.to_string()));
                continue;
            }
        }
        if let Some(name) = map.get("name").unwrap_or(&null).as_str() {
            actual_instances.push((name.to_string(), version.to_string()));
        }
    }
    let expected_counts = identity_counts(&expected.iter().map(|e| (e.name.clone(), e.version.clone())).collect::<Vec<_>>());
    let actual_counts = identity_counts(&actual_instances);
    let mut identities: Vec<String> = expected_counts.keys().chain(actual_counts.keys()).cloned().collect();
    identities.sort_unstable();
    identities.dedup();
    for identity in identities {
        if expected_counts.get(&identity) != actual_counts.get(&identity) {
            let (name, version) = identity.split_once('\0').unwrap_or((identity.as_str(), ""));
            return Err(Error::Conflict(format!(
                "{} installed package report for {project_id} does not match the selected graph at {name}@{version}; expected {} instance(s), found {}",
                client.as_str(),
                expected_counts.get(&identity).copied().unwrap_or(0),
                actual_counts.get(&identity).copied().unwrap_or(0)
            )));
        }
    }
    Ok(())
}

fn pathdiff(path: &Path, base: &Path) -> String {
    path.strip_prefix(base).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string_lossy().to_string())
}

fn verify_installed_tree(
    project_dir: &Path,
    project: &ProjectRecord,
    target: &Target,
    client: &Manager,
    env: &HashMap<String, String>,
) -> Result<(usize, usize)> {
    let (instances, skipped, skipped_identities) = if *client == Manager::Npm {
        expected_npm_instances(project, target)?
    } else {
        expected_pnpm_instances(project, target)?
    };
    let optional_identities: FxHashSet<String> = instances
        .iter()
        .filter(|i| i.optional)
        .map(|i| format!("{}\0{}", i.name, i.version))
        .collect();
    if *client == Manager::Npm {
        for instance in &instances {
            inspect_installed_package(project_dir, instance)?;
        }
        let _ = &optional_identities;
    }
    let (roots, nodes) = run_package_report(client, project_dir, env)?;
    let filtered: Vec<deser_value::Value> = nodes
        .into_iter()
        .filter(|node| {
            let identity = node
                .as_map()
                .and_then(|m| {
                    let null = deser_value::Value::from(deser_value::Kind::Null);
                    match (m.get("name").unwrap_or(&null).as_str(), m.get("version").unwrap_or(&null).as_str()) {
                        (Some(n), Some(v)) => Some(format!("{n}\0{v}")),
                        _ => None,
                    }
                });
            identity.is_none() || identity.is_some_and(|id| !skipped_identities.contains(&id))
        })
        .collect();
    if *client == Manager::Pnpm {
        for node in &filtered {
            let Some(map) = node.as_map() else {
                return Err(Error::Conflict(format!(
                    "pnpm package report omitted an installed package path for {}",
                    project.id
                )));
            };
            let null = deser_value::Value::from(deser_value::Kind::Null);
            let (Some(path), Some(name), Some(version)) = (
                map.get("path").unwrap_or(&null).as_str(),
                map.get("name").unwrap_or(&null).as_str(),
                map.get("version").unwrap_or(&null).as_str(),
            ) else {
                return Err(Error::Conflict(format!(
                    "pnpm package report omitted an installed package path for {}",
                    project.id
                )));
            };
            if pathdiff(&PathBuf::from(path), project_dir).starts_with("..") {
                return Err(Error::Conflict(format!(
                    "pnpm package report points outside the restored project for {}",
                    project.id
                )));
            }
            inspect_installed_package(
                project_dir,
                &ExpectedInstance {
                    key: pathdiff(&PathBuf::from(path), project_dir).replace('\\', "/"),
                    name: name.to_string(),
                    version: version.to_string(),
                    optional: optional_identities.contains(&format!("{name}\0{version}")),
                },
            )?;
        }
    }
    verify_reported_links(&roots, project_dir, &skipped_identities)?;
    assert_report_matches_expected(&filtered, &instances, client, &project.id, project_dir)?;
    Ok((instances.len(), skipped))
}

fn check_executables(
    project_dir: &Path,
    project: &ProjectRecord,
    roots: &[crate::inventory::RootDependency],
    env: &HashMap<String, String>,
) -> Result<Vec<(String, String)>> {
    let mut outputs = Vec::new();
    let ts_version = roots
        .iter()
        .find(|r| r.package_name == "typescript")
        .and_then(|r| r.package_version.clone())
        .or_else(|| {
            project
                .packages
                .iter()
                .find(|p| p.name == "typescript" && p.key == "node_modules/typescript")
                .and_then(|p| p.version.clone())
        });
    if let Some(version) = ts_version {
        let result = run_process(
            "node",
            &["node_modules/typescript/bin/tsc".to_string(), "--version".to_string()],
            &ProcessOptions {
                cwd: project_dir.to_string_lossy().to_string(),
                env: env.clone(),
                timeout_ms: 30_000,
                stdout_mode: StdoutMode::Tail(16384),
            },
        )?;
        let output = result.stdout.trim().to_string();
        if output != format!("Version {version}") {
            return Err(Error::Conflict(format!(
                "TypeScript executable did not report the selected version {version}"
            )));
        }
        outputs.push(("typescript".to_string(), output));
    }
    let esbuild_version = roots
        .iter()
        .find(|r| r.package_name == "esbuild")
        .and_then(|r| r.package_version.clone())
        .or_else(|| {
            project
                .packages
                .iter()
                .find(|p| p.name == "esbuild" && p.key == "node_modules/esbuild")
                .and_then(|p| p.version.clone())
        });
    if let Some(version) = esbuild_version {
        let result = run_process(
            "node",
            &["node_modules/esbuild/bin/esbuild".to_string(), "--version".to_string()],
            &ProcessOptions {
                cwd: project_dir.to_string_lossy().to_string(),
                env: env.clone(),
                timeout_ms: 30_000,
                stdout_mode: StdoutMode::Tail(16384),
            },
        )?;
        let output = result.stdout.trim().to_string();
        if output != version {
            return Err(Error::Conflict(format!(
                "esbuild executable did not report the selected version {version}"
            )));
        }
        outputs.push(("esbuild".to_string(), output));
    }
    Ok(outputs)
}

fn assert_project_static_responses(
    requests: &[StaticRequest],
    mount: &str,
    project: &ProjectRecord,
    target: &Target,
    artifacts: &[ArtifactRecord],
) -> Result<()> {
    let mut expected_names: FxHashMap<String, Vec<String>> = FxHashMap::default();
    let mut expected_digests: FxHashSet<String> = FxHashSet::default();
    for item in &project.packages {
        let Some(ref version) = item.version else { continue };
        if item.source_kind != crate::inventory::SourceKind::Registry || !supports_target(&item.platform, target) {
            continue;
        }
        let digest = package_artifact(project, item, artifacts)?.artifact.sha512.clone();
        expected_digests.insert(digest.clone());
        expected_names.entry(digest).or_default().push(format!("{}@{version}", item.name));
    }
    for request in requests {
        let platform_skip = request.pathname.ends_with("/current/-/tarballs/unavailable.tgz")
            && request.method == "GET"
            && request.status == Some(404)
            && request.complete;
        if !request.complete || (request.status != Some(200) && !platform_skip) {
            let digest = request
                .pathname
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".tgz"))
                .filter(|f| f.len() == 128 && f.bytes().all(|b| b.is_ascii_hexdigit()));
            let identity = digest.and_then(|d| expected_names.get(d)).map(|v| v.join(", "));
            return Err(Error::Conflict(format!(
                "local static server returned HTTP {} for {}{}",
                request.status.map(|s| s.to_string()).unwrap_or_else(|| "incomplete".to_string()),
                request.pathname,
                identity.map(|i| format!(" ({i})")).unwrap_or_default()
            )));
        }
    }
    let mut digests: Vec<_> = expected_digests.iter().collect();
    digests.sort_unstable();
    for digest in digests {
        let pathname = format!("{mount}current/-/tarballs/{digest}.tgz");
        let matching: Vec<&StaticRequest> =
            requests.iter().filter(|r| r.pathname == pathname && r.method == "GET").collect();
        if let Some(corrupt) = matching.iter().find(|r| r.status == Some(200) && r.response_sha512.as_deref() != Some(digest.as_str())) {
            let _ = corrupt;
            return Err(Error::Corrupt(format!(
                "local static server returned corrupted tarball bytes for {pathname} ({})",
                expected_names.get(digest).map(|v| v.join(", ")).unwrap_or_default()
            )));
        }
        let response = matching
            .iter()
            .find(|r| r.status == Some(200) && r.complete && r.response_sha512.as_deref() == Some(digest.as_str()));
        if response.is_none() {
            return Err(Error::Conflict(format!(
                "package manager did not fetch expected applicable artifact {pathname} ({})",
                expected_names.get(digest).map(|v| v.join(", ")).unwrap_or_default()
            )));
        }
        if response.is_some_and(|r| r.content_type.as_deref() != Some("application/octet-stream")) {
            return Err(Error::Conflict(format!("local server returned the wrong content type for tarball {pathname}")));
        }
    }
    Ok(())
}

fn write_project_snapshot(project_dir: &Path, project: &ProjectRecord, lock_text: &str, lockfile_name: &str) -> Result<()> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires a saved snapshot for {}", project.id))
    })?;
    let manifest = snapshot.manifest.clone().ok_or_else(|| {
        Error::Input(format!("validation requires an imported package.json snapshot for {}", project.id))
    })?;
    std::fs::create_dir_all(project_dir)
        .map_err(|_| Error::Operation(format!("could not create restored project directory {}", project_dir.display())))?;
    std::fs::set_permissions(project_dir, std::fs::Permissions::from_mode(0o700)).ok();
    write_new_file(&project_dir.join("package.json"), manifest.as_bytes(), &project.id)?;
    write_new_file(&project_dir.join(lockfile_name), lock_text.as_bytes(), &project.id)?;
    for file in &snapshot.workspace_files {
        let file_path = project_dir.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| Error::Operation(format!("could not restore workspace directory {}", parent.display())))?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).ok();
        }
        write_new_file(&file_path, file.contents.as_bytes(), &project.id)?;
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8], project_id: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(path)
        .map_err(|_| Error::Operation(format!("could not restore package.json for {project_id}")))?;
    use std::io::Write;
    file.write_all(bytes)
        .map_err(|_| Error::Operation(format!("could not restore package.json for {project_id}")))?;
    file.sync_all().ok();
    Ok(())
}

fn assert_project_files_unchanged(project_dir: &Path, project: &ProjectRecord, lockfile_name: &str) -> Result<()> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires a saved snapshot for {}", project.id))
    })?;
    let manifest = snapshot.manifest.clone().ok_or_else(|| {
        Error::Input(format!("validation requires an imported package.json snapshot for {}", project.id))
    })?;
    let mut files: Vec<(String, String)> = vec![("package.json".to_string(), manifest)];
    for file in &snapshot.workspace_files {
        files.push((file.path.clone(), file.contents.clone()));
    }
    for (path, contents) in files {
        if path == lockfile_name {
            continue;
        }
        let actual = std::fs::read_to_string(project_dir.join(path.replace('/', std::path::MAIN_SEPARATOR_STR))).map_err(|_| {
            Error::Operation(format!("client removed project snapshot file {} for {}", path, project.id))
        })?;
        if actual != contents {
            return Err(Error::Conflict(format!(
                "{} changed project snapshot file {} for {}",
                project.manager().as_str(),
                path,
                project.id
            )));
        }
    }
    Ok(())
}

type InstallOutcome = (Vec<SelectedPackage>, usize, Vec<(String, String)>);

#[allow(clippy::too_many_arguments)]
fn install_project(
    client: &Manager,
    project: &ProjectRecord,
    target: &Target,
    workspace: &Path,
    registry: &str,
    local_origin: &str,
    project_number: usize,
    smoke_executables: bool,
    artifacts: &[ArtifactRecord],
) -> Result<InstallOutcome> {
    let slug = package_slug(&project.id);
    let project_dir = workspace.join("projects").join(format!("{slug}-{project_number}"));
    let install_workspace = workspace.join("clients").join(format!("{slug}-{project_number}"));
    std::fs::create_dir_all(&install_workspace)
        .map_err(|_| Error::Operation(format!("could not create validation client directory {}", install_workspace.display())))?;
    let lockfile_name = if *client == Manager::Npm { "package-lock.json" } else { "pnpm-lock.yaml" };
    let migrated_lock = if *client == Manager::Npm {
        rewrite_npm_lock(project, target, artifacts, registry)?
    } else {
        rewrite_pnpm_lock(project, target, artifacts, registry)?
    };
    assert_migration_preserves_graph(project, &migrated_lock)?;
    write_project_snapshot(&project_dir, project, &migrated_lock, lockfile_name)?;
    let cache_dir = install_workspace.join("cache");
    let environment = client_environment(&install_workspace, registry, &cache_dir)?;
    let args: Vec<String> = if *client == Manager::Npm {
        let mut base = vec![
            "ci".to_string(),
            format!("--registry={registry}"),
            format!("--cache={}", cache_dir.display()),
            "--ignore-scripts".to_string(),
            "--no-audit".to_string(),
            "--no-fund".to_string(),
            "--fetch-retries=0".to_string(),
        ];
        base.extend(npm_snapshot_install_args(project)?);
        base
    } else {
        vec![
            "install".to_string(),
            "--frozen-lockfile".to_string(),
            format!("--registry={registry}"),
            format!("--store-dir={}", install_workspace.join("pnpm-store").display()),
            "--ignore-scripts".to_string(),
            "--ignore-pnpmfile".to_string(),
        ]
    };
    run_process(
        client.as_str(),
        &args,
        &ProcessOptions {
            cwd: project_dir.to_string_lossy().to_string(),
            env: environment.clone(),
            timeout_ms: CLIENT_TIMEOUT_MS,
            stdout_mode: StdoutMode::Tail(16384),
        },
    )?;
    let actual_lock = std::fs::read_to_string(project_dir.join(lockfile_name)).map_err(|_| {
        Error::Operation(format!("{} removed {lockfile_name} while restoring {}", client.as_str(), project.id))
    })?;
    let actual_project = parse_project_lock(
        client,
        &actual_lock,
        LockParseOptions {
            project_id: project.id.clone(),
            package_manager_pin: project.package_manager_pin.clone(),
            lockfile_path: lockfile_name.to_string(),
            target_ids: vec![target.id().to_string()],
            snapshot: None,
        },
    )?;
    assert_lockfile_matches(client, &migrated_lock, &actual_lock, project)?;
    let (instance_count, platform_skipped) = verify_installed_tree(&project_dir, project, target, client, &environment)?;
    let selection = package_selection(&actual_project.packages, target, artifacts, true, Some(local_origin))
        .map_err(|e| Error::Conflict(format!("could not verify restored package selection for {}: {e}", project.id)))?;
    assert_project_files_unchanged(&project_dir, project, lockfile_name)?;
    let roots: Vec<crate::inventory::RootDependency> =
        project.roots.iter().filter(|r| r.importer == ".").cloned().collect();
    let executables = if smoke_executables {
        check_executables(&project_dir, project, &roots, &environment)?
    } else {
        Vec::new()
    };
    let _ = (instance_count, platform_skipped);
    Ok((selection, platform_skipped, executables))
}

fn npm_snapshot_install_args(project: &ProjectRecord) -> Result<Vec<String>> {
    let snapshot = project.snapshot.as_ref().ok_or_else(|| {
        Error::Input(format!("validation requires a saved snapshot for {}", project.id))
    })?;
    let mut args = Vec::new();
    if let Some(legacy) = snapshot.npm_install_options.legacy_peer_deps {
        args.push(format!("--legacy-peer-deps={legacy}"));
    }
    if let Some(links) = snapshot.npm_install_options.install_links {
        args.push(format!("--install-links={links}"));
    }
    Ok(args)
}

fn client_environment(workspace: &Path, registry: &str, cache_dir: &Path) -> Result<HashMap<String, String>> {
    for directory in [workspace.join("home"), workspace.join("config"), cache_dir.to_path_buf(), workspace.join("pnpm-store"), workspace.join("tmp")] {
        std::fs::create_dir_all(&directory)
            .map_err(|_| Error::Operation(format!("could not create validation directory {}", directory.display())))?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).ok();
    }
    std::fs::write(workspace.join("user.npmrc"), format!("registry={registry}\naudit=false\nfund=false\nignore-scripts=true\n"))
        .map_err(|_| Error::Operation(format!("could not write validation npm configuration in {}", workspace.display())))?;
    std::fs::write(workspace.join("global.npmrc"), "").map_err(|_| {
        Error::Operation(format!("could not write validation npm configuration in {}", workspace.display()))
    })?;
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
    env.insert("LANG".to_string(), std::env::var("LANG").unwrap_or_else(|_| "C".to_string()));
    env.insert("LC_ALL".to_string(), "C".to_string());
    env.insert("HOME".to_string(), workspace.join("home").to_string_lossy().to_string());
    env.insert("TMPDIR".to_string(), workspace.join("tmp").to_string_lossy().to_string());
    env.insert("XDG_CONFIG_HOME".to_string(), workspace.join("config").to_string_lossy().to_string());
    env.insert("COREPACK_HOME".to_string(), workspace.join("corepack").to_string_lossy().to_string());
    env.insert("CI".to_string(), "true".to_string());
    env.insert("npm_config_userconfig".to_string(), workspace.join("user.npmrc").to_string_lossy().to_string());
    env.insert("npm_config_globalconfig".to_string(), workspace.join("global.npmrc").to_string_lossy().to_string());
    env.insert("npm_config_registry".to_string(), registry.to_string());
    env.insert("npm_config_cache".to_string(), cache_dir.to_string_lossy().to_string());
    env.insert("npm_config_audit".to_string(), "false".to_string());
    env.insert("npm_config_fund".to_string(), "false".to_string());
    env.insert("npm_config_ignore_scripts".to_string(), "true".to_string());
    env.insert("npm_config_update_notifier".to_string(), "false".to_string());
    env.insert("npm_config_fetch_retries".to_string(), "0".to_string());
    env.insert("pnpm_config_ignore_pnpmfile".to_string(), "true".to_string());
    env.insert("pnpm_config_store_dir".to_string(), workspace.join("pnpm-store").to_string_lossy().to_string());
    env.insert("pnpm_config_pm_on_fail".to_string(), "ignore".to_string());
    env.insert("pnpm_config_trust_lockfile".to_string(), "true".to_string());
    Ok(env)
}

pub struct ValidateOptions<'a> {
    pub client: Manager,
    pub state_directory: &'a Path,
    pub config: &'a Config,
    pub cwd: &'a Path,
    pub smoke_executables: bool,
}

pub fn validate_inventory(options: &ValidateOptions) -> Result<ValidationResult> {
    let target = target_for_current_platform(options.config)?;
    let inventory = load_inventory(options.state_directory)?;
    let projects: Vec<&ProjectRecord> = inventory
        .projects
        .iter()
        .filter(|p| p.manager() == options.client && p.targets.iter().any(|t| t == target.id()))
        .collect();
    if projects.is_empty() {
        return Err(Error::Input(format!(
            "inventory contains no {} projects for target {}",
            options.client.as_str(),
            target.id()
        )));
    }
    let scratch_root = options.cwd.join(".work").join("tmp");
    std::fs::create_dir_all(&scratch_root)
        .map_err(|_| Error::Operation(format!("could not create validation scratch directory {}", scratch_root.display())))?;
    sweep_stale_workspaces(&scratch_root, STALE_WORKSPACE_AGE);
    let workspace = scratch_root.join(format!("{WORKSPACE_PREFIX}{}", unique_suffix()));
    std::fs::create_dir_all(&workspace)
        .map_err(|_| Error::Operation("could not create validation workspace".to_string()))?;
    let result = (|| -> Result<ValidationResult> {
        for project in &projects {
            assert_snapshot_matches_project(project, &target)?;
        }
        let mut expected_by_project: FxHashMap<String, Vec<SelectedPackage>> = FxHashMap::default();
        for project in &projects {
            expected_by_project.insert(
                project.id.clone(),
                package_selection(&project.packages, &target, &inventory.artifacts, false, None).map_err(|e| {
                    Error::Conflict(format!("could not verify imported package selections: {e}"))
                })?,
            );
        }
        let site_dir = workspace.join("site");
        let mirror_dir = site_dir.join("mirror");
        std::fs::create_dir_all(&mirror_dir)
            .map_err(|_| Error::Operation(format!("could not create local site directory {}", mirror_dir.display())))?;
        let requests: Arc<Mutex<Vec<StaticRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = start_static_server(mirror_dir.clone(), requests.clone(), shutdown.clone())?;
        let outcome = (|| -> Result<ValidationResult> {
            let origin = format!("http://127.0.0.1:{}", server.port);
            let mut environment = client_environment(&workspace, &format!("{origin}/current/"), &workspace.join("tool-cache"))?;
            let _ = &mut environment;
            let version = check_client_version(&options.client, &target, &workspace.to_string_lossy(), &environment)?;
            let mut project_number = 0;
            let mut validated: FxHashSet<String> = FxHashSet::default();
            let mut platform_skipped = 0;
            let mut executables: Vec<(String, String)> = Vec::new();
            let mut generation_id: Option<String> = None;
            for mount in ["/mirror/", "/"] {
                let public_base_url = format!("{origin}{mount}");
                let published = publish_site(&PublishOptions {
                    state_directory: options.state_directory,
                    site_directory: &site_dir,
                    config: options.config,
                    render_base_url: Some(public_base_url.clone()),
                })?;
                generation_id = Some(published.generation_id.clone());
                let registry = format!("{public_base_url}current/");
                for project in &projects {
                    project_number += 1;
                    let request_start = requests.lock().map(|r| r.len()).unwrap_or(0);
                    let (selection, skipped, execs) = install_project(
                        &options.client,
                        project,
                        &target,
                        &workspace,
                        &registry,
                        &origin,
                        project_number,
                        options.smoke_executables,
                        &inventory.artifacts,
                    )?;
                    let request_end = requests.lock().map(|r| r.len()).unwrap_or(0);
                    let snapshot: Vec<StaticRequest> = requests
                        .lock()
                        .map(|r| r[request_start.min(r.len())..request_end.min(r.len())].to_vec())
                        .unwrap_or_default();
                    assert_project_static_responses(&snapshot, mount, project, &target, &inventory.artifacts)
                        .map_err(|e| Error::Conflict(format!("local static responses were incomplete for {}: {e}", project.id)))?;
                    let expected = expected_by_project.get(&project.id).ok_or_else(|| {
                        Error::Corrupt(format!("validation lost the expected package selection for {}", project.id))
                    })?;
                    compare_selections(expected, &selection, &project.id, &options.client).map_err(|e| {
                        Error::Conflict(format!("validation changed package selection for {}: {e}", project.id))
                    })?;
                    for item in expected {
                        validated.insert(format!("{}\0{}\0{}", project.id, item.identity, item.integrity));
                    }
                    platform_skipped += skipped;
                    executables.extend(execs);
                }
            }
            if requests.lock().map(|r| r.iter().any(|q| q.prefix.is_none())).unwrap_or(false) {
                let bad = requests
                    .lock()
                    .map(|r| r.iter().find(|q| q.prefix.is_none()).map(|q| q.pathname.clone()).unwrap_or_default())
                    .unwrap_or_default();
                return Err(Error::Conflict(format!("validation served a request outside the local static site: {bad}")));
            }
            let failure = requests.lock().map(|r| {
                r.iter()
                    .find(|q| {
                        let platform_skip = q.pathname.ends_with("/current/-/tarballs/unavailable.tgz")
                            && q.method == "GET"
                            && q.status == Some(404)
                            && q.complete;
                        !q.complete || (q.status != Some(200) && !platform_skip)
                    })
                    .cloned()
            }).unwrap_or(None);
            if let Some(serving_failure) = failure {
                return Err(Error::Conflict(format!(
                    "local static server returned HTTP {} for {}",
                    serving_failure.status.map(|s| s.to_string()).unwrap_or_else(|| "incomplete".to_string()),
                    serving_failure.pathname
                )));
            }
            if !validated.is_empty() && requests.lock().map(|r| r.is_empty()).unwrap_or(true) {
                return Err(Error::Operation("package manager made no requests to the local static site".to_string()));
            }
            let generation_id = generation_id
                .ok_or_else(|| Error::Operation("validation did not publish a site generation".to_string()))?;
            let node_version = run_process(
                "node",
                &["--version".to_string()],
                &ProcessOptions {
                    cwd: workspace.to_string_lossy().to_string(),
                    env: environment,
                    timeout_ms: 30_000,
                    stdout_mode: StdoutMode::Tail(16384),
                },
            )
            .map(|r| r.stdout.trim().to_string())
            .unwrap_or_default();
            Ok(ValidationResult {
                client: options.client.as_str().to_string(),
                version,
                node_version,
                target: target.id().to_string(),
                package_count: validated.len(),
                platform_skipped_count: platform_skipped,
                project_count: projects.len(),
                root_count: projects.iter().map(|p| p.roots.len()).sum(),
                request_count: requests.lock().map(|r| r.len()).unwrap_or(0),
                generation_id,
                executables,
            })
        })();
        server.close();
        outcome
    })();
    let _ = std::fs::remove_dir_all(&workspace);
    result
}

fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_server_answers_a_request_that_arrives_after_accept() {
        let dir = tempfile::tempdir().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = start_static_server(dir.path().to_path_buf(), requests, shutdown).unwrap();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        stream.write_all(b"GET /current/missing HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        server.close();
        assert!(response.starts_with("HTTP/1.1 404 "), "{response:?}");
    }

    #[test]
    fn sweeps_only_stale_validation_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("mynpm-validate-stale");
        let fresh = dir.path().join("mynpm-validate-fresh");
        let unrelated = dir.path().join("other");
        for path in [&stale, &fresh, &unrelated] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::File::open(&stale)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::SystemTime::now() - Duration::from_secs(7200)),
            )
            .unwrap();
        sweep_stale_workspaces(dir.path(), STALE_WORKSPACE_AGE);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(unrelated.exists());
    }
}
