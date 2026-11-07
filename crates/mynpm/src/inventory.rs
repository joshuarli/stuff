#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u64 = 2;
pub const METADATA_ONLY_SCHEMA_VERSION: u64 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Corrupt,
    Input,
    Conflict,
}

fn err(category: Category, message: impl Into<String>) -> Error {
    match category {
        Category::Corrupt => Error::Corrupt(message.into()),
        Category::Input => Error::Input(message.into()),
        Category::Conflict => Error::Conflict(message.into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Manager {
    Npm,
    Pnpm,
}

impl Manager {
    pub fn as_str(&self) -> &'static str {
        match self {
            Manager::Npm => "npm",
            Manager::Pnpm => "pnpm",
        }
    }

    pub fn pinned_version(&self) -> &'static str {
        match self {
            Manager::Npm => "12.2.1",
            Manager::Pnpm => "12.15.0",
        }
    }
}

impl std::fmt::Display for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Registry,
    Local,
    Git,
    ExternalUrl,
    Unknown,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Registry => "registry",
            SourceKind::Local => "local",
            SourceKind::Git => "git",
            SourceKind::ExternalUrl => "external-url",
            SourceKind::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<SourceKind> {
        match s {
            "registry" => Some(SourceKind::Registry),
            "local" => Some(SourceKind::Local),
            "git" => Some(SourceKind::Git),
            "external-url" => Some(SourceKind::ExternalUrl),
            "unknown" => Some(SourceKind::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockfileVersion {
    Npm(u64),
    Pnpm(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFlags {
    pub dev: bool,
    pub optional: bool,
    pub peer: bool,
    pub peer_optional: bool,
    pub link: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformConstraints {
    pub os: Option<Vec<String>>,
    pub cpu: Option<Vec<String>>,
    pub libc: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFile {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmInstallOptions {
    pub legacy_peer_deps: Option<bool>,
    pub install_links: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSnapshot {
    pub manifest: Option<String>,
    pub lockfile: String,
    pub workspace_files: Vec<SnapshotFile>,
    pub npm_install_options: NpmInstallOptions,
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
    pub flags: PackageFlags,
    pub platform: PlatformConstraints,
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
    pub manager: Manager,
    pub package_manager_pin: Option<String>,
    pub lockfile_path: String,
    pub lockfile_version: LockfileVersion,
    pub targets: Vec<String>,
    pub importers: Vec<String>,
    pub roots: Vec<RootDependency>,
    pub packages: Vec<PackageRecord>,
    pub snapshot: Option<ProjectSnapshot>,
}

impl ProjectRecord {
    pub fn manager(&self) -> Manager {
        self.manager
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactVerification {
    pub sha512: String,
    pub integrity: String,
    pub shasum: Option<String>,
    pub source_url: String,
    pub legacy_sha1_only: bool,
    pub first_party: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub name: String,
    pub version: String,
    pub packument: deser_value::Value,
    pub metadata: deser_value::Value,
    pub artifact: ArtifactVerification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataOnlyRecord {
    pub name: String,
    pub version: String,
    pub integrity: String,
    pub published_at: String,
    pub platform: PlatformConstraints,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    pub projects: Vec<ProjectRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    pub metadata_only: Vec<MetadataOnlyRecord>,
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

pub fn valid_package_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix('@') {
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
        !name.contains('/') && valid_name_part(name)
    }
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s.len() == 1 || !s.starts_with('0'))
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

pub fn valid_version(version: &str) -> bool {
    let mut rest = version;
    for _ in 0..2 {
        match rest.find('.') {
            Some(i) => {
                if !is_number(&rest[..i]) {
                    return false;
                }
                rest = &rest[i + 1..];
            }
            None => return false,
        }
    }
    let patch_end = rest.find(['-', '+']).unwrap_or(rest.len());
    if !is_number(&rest[..patch_end]) {
        return false;
    }
    let mut tail = &rest[patch_end..];
    if let Some(stripped) = tail.strip_prefix('-') {
        let end = stripped.find('+').unwrap_or(stripped.len());
        if !valid_ident(&stripped[..end]) {
            return false;
        }
        tail = &stripped[end..];
    }
    if let Some(stripped) = tail.strip_prefix('+') {
        if !valid_ident(stripped) {
            return false;
        }
    } else if !tail.is_empty() {
        return false;
    }
    true
}

fn valid_manager_pin(pin: &str, manager: &Manager) -> bool {
    let Some((mgr_part, rest)) = pin.split_once('@') else {
        return false;
    };
    if mgr_part != manager.as_str() {
        return false;
    }
    let (ver_part, build) = match rest.split_once('+') {
        Some((v, b)) => (v, Some(b)),
        None => (rest, None),
    };
    if !valid_version(ver_part) || ver_part.contains('+') {
        return false;
    }
    match build {
        None => true,
        Some(b) => {
            b.strip_prefix("sha512.").is_some_and(|d| {
                !d.is_empty() && d.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/' || c == b'=' || c == b'_' || c == b'-')
            })
        }
    }
}

fn is_sha1_hex(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn is_sha512_hex(s: &str) -> bool {
    s.len() == 128 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn integrity_lengths(algo: &str) -> Option<usize> {
    match algo {
        "sha1" => Some(20),
        "sha256" => Some(32),
        "sha384" => Some(48),
        "sha512" => Some(64),
        _ => None,
    }
}

pub fn validate_integrity(value: Option<&str>, label: &str, category: Category) -> Result<Option<String>> {
    let Some(text) = value else { return Ok(None) };
    if text.trim().is_empty() {
        return Err(err(category, format!("{label} is malformed")));
    }
    let mut claims: BTreeMap<String, String> = BTreeMap::new();
    for token in text.split_whitespace() {
        let claim = token.split('?').next().unwrap_or("");
        let Some(sep) = claim.find('-') else {
            return Err(err(category, format!("{label} contains an unsupported integrity token")));
        };
        let (algo, digest) = (&claim[..sep], &claim[sep + 1..]);
        let len = integrity_lengths(algo).ok_or_else(|| {
            err(category, format!("{label} contains an unsupported integrity token"))
        })?;
        if digest.is_empty()
            || !digest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        {
            return Err(err(category, format!("{label} contains an unsupported integrity token")));
        }
        let bytes = B64.decode(digest).map_err(|_| {
            err(category, format!("{label} contains a malformed {algo} digest"))
        })?;
        if bytes.len() != len || B64.encode(&bytes) != digest {
            return Err(err(category, format!("{label} contains a malformed {algo} digest")));
        }
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        if let Some(prev) = claims.get(algo)
            && prev != &hex {
                return Err(err(category, format!("{label} contains conflicting {algo} claims")));
            }
        claims.insert(algo.to_string(), hex);
    }
    Ok(Some(text.to_string()))
}

fn lenient_hex(bytes_text: &str) -> String {
    let filtered: String = bytes_text
        .bytes()
        .filter(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/' || *b == b'=')
        .map(|b| b as char)
        .collect();
    B64.decode(filtered)
        .map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
        .unwrap_or_default()
}

fn digest_claims(integrity: Option<&str>, label: &str, category: Category) -> Result<BTreeMap<String, String>> {
    let mut claims = BTreeMap::new();
    let Some(text) = integrity else { return Ok(claims) };
    for token in text.split_whitespace() {
        let claim = token.split('?').next().unwrap_or("");
        let sep = claim.find('-').ok_or_else(|| {
            err(category, format!("{label} contains conflicting integrity claims"))
        })?;
        let (algo, digest) = (&claim[..sep], &claim[sep + 1..]);
        let hex = lenient_hex(digest);
        if let Some(prev) = claims.get(algo)
            && prev != &hex {
                return Err(err(category, format!("{label} contains conflicting {algo} claims")));
            }
        claims.insert(algo.to_string(), hex);
    }
    Ok(claims)
}

fn add_digest_claims(
    target: &mut BTreeMap<String, String>,
    source: &BTreeMap<String, String>,
    label: &str,
    category: Category,
) -> Result<()> {
    for (algo, digest) in source {
        if let Some(existing) = target.get(algo)
            && existing != digest {
                return Err(err(category, format!("conflicting {algo} integrity for {label}")));
            }
        target.insert(algo.clone(), digest.clone());
    }
    Ok(())
}

pub fn sort_canonical_value(value: &mut deser_value::Value) {
    if let Some(seq) = value.as_seq_mut() {
        for item in seq.iter_mut() {
            sort_canonical_value(item);
        }
        return;
    }
    if let Some(map) = value.as_map_mut() {
        for i in 0..map.len() {
            if let Some((_, v)) = map.get_index_mut(i) {
                sort_canonical_value(v);
            }
        }
        map.sort_by(|k1, _, k2, _| {
            let s1 = k1.as_str().map(|s| s.to_string()).unwrap_or_else(|| format!("{k1:?}"));
            let s2 = k2.as_str().map(|s| s.to_string()).unwrap_or_else(|| format!("{k2:?}"));
            s1.cmp(&s2)
        });
    }
}

pub fn canonical_string(value: &deser_value::Value) -> Result<String> {
    let mut owned = value.clone();
    sort_canonical_value(&mut owned);
    deser_json::to_string(&owned).map_err(|e| Error::Input(format!("value is not JSON serializable: {e}")))
}

fn map_keys(map: &deser_value::Map) -> Vec<String> {
    let mut out = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        if let Some((k, _)) = map.get_index(i)
            && let Some(s) = k.as_str() {
                out.push(s.to_string());
            }
    }
    out
}

fn require_record<'a>(v: &'a deser_value::Value, label: &str, category: Category) -> Result<&'a deser_value::Map> {
    v.as_map()
        .ok_or_else(|| err(category, format!("{label} must be an object")))
}

fn exact_keys(map: &deser_value::Map, keys: &[&str], label: &str, category: Category) -> Result<()> {
    for k in map_keys(map) {
        if !keys.contains(&k.as_str()) {
            return Err(err(category, format!("{label} has unknown field {k}")));
        }
    }
    for k in keys {
        if map.get(*k).is_none() {
            return Err(err(category, format!("{label} is missing field {k}")));
        }
    }
    Ok(())
}

fn require_text(v: &deser_value::Value, label: &str, category: Category) -> Result<String> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s.trim() == s => Ok(s.to_string()),
        _ => Err(err(category, format!("{label} must be a non-empty string"))),
    }
}

fn require_nullable_text(v: &deser_value::Value, label: &str, category: Category) -> Result<Option<String>> {
    if matches!(&**v, deser_value::Kind::Null) {
        return Ok(None);
    }
    if v.as_str().is_none() {
        return Err(err(category, format!("{label} must be a string or null")));
    }
    Ok(Some(require_text(v, label, category)?))
}

fn require_list(v: &deser_value::Value, label: &str, category: Category, allow_empty: bool) -> Result<Vec<String>> {
    let Some(seq) = v.as_seq() else {
        return Err(err(category, format!("{label} must be {}an array", if allow_empty { "an" } else { "a non-empty" })));
    };
    if !allow_empty && seq.is_empty() {
        return Err(err(category, format!("{label} must be a non-empty array")));
    }
    let mut out = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        match item.as_str() {
            Some(s) if !s.is_empty() => out.push(s.to_string()),
            _ => return Err(err(category, format!("{label} must contain non-empty strings"))),
        }
    }
    let mut sorted = out.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != out.len() {
        return Err(err(category, format!("{label} must not contain duplicates")));
    }
    sorted.dedup();
    let mut out_sorted = out;
    out_sorted.sort();
    Ok(out_sorted)
}

fn normalize_importer(v: &deser_value::Value, label: &str, category: Category) -> Result<String> {
    let importer = require_text(v, label, category)?;
    if importer == "." {
        return Ok(importer);
    }
    if importer.starts_with('/')
        || importer.contains('\\')
        || importer.split('/').any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(err(category, format!("{label} must be a safe project-relative path")));
    }
    Ok(importer)
}

fn normalize_snapshot_path(v: &deser_value::Value, label: &str, category: Category) -> Result<String> {
    let path = require_text(v, label, category)?;
    if path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(err(category, format!("{label} must be a safe project-relative path")));
    }
    Ok(path)
}

fn normalize_snapshot(
    v: &deser_value::Value,
    label: &str,
    category: Category,
    manager: &Manager,
    lockfile_version: &LockfileVersion,
) -> Result<Option<ProjectSnapshot>> {
    if matches!(&**v, deser_value::Kind::Null) {
        return Ok(None);
    }
    let record = require_record(v, label, category)?;
    {
        let keys = map_keys(record);
        for expected in ["manifest", "lockfile", "workspaceFiles"] {
            if !keys.iter().any(|k| k == expected) {
                return Err(err(category, format!("{label} is missing field {expected}")));
            }
        }
        for k in keys {
            if !["manifest", "lockfile", "workspaceFiles", "npmInstallOptions"].contains(&k.as_str()) {
                return Err(err(category, format!("{label} has unknown field {k}")));
            }
        }
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let manifest = match record.get("manifest").unwrap_or(&null) {
        mv if matches!(&**mv, deser_value::Kind::Null) => None,
        mv => {
            let text = mv.as_str().ok_or_else(|| err(category, format!("{label}.manifest must be a string")))?.to_string();
            let parsed: deser_value::Value = deser_json::from_str(&text)
                .map_err(|_| err(category, format!("{label}.manifest is not valid JSON")))?;
            if parsed.as_map().is_none() {
                return Err(err(category, format!("{label}.manifest must contain an object")));
            }
            Some(text)
        }
    };
    let lockfile = record.get("lockfile").unwrap_or(&null).as_str().ok_or_else(|| err(category, format!("{label}.lockfile must be a string")))?.to_string();
    if lockfile.is_empty() {
        return Err(err(category, format!("{label}.lockfile must not be empty")));
    }
    if *manager == Manager::Npm {
        let parsed: deser_value::Value = deser_json::from_str(&lockfile)
            .map_err(|_| err(category, format!("{label}.lockfile is not valid JSON")))?;
        let ok = parsed.as_map().is_some_and(|m| {
            let expected = match lockfile_version {
                LockfileVersion::Npm(n) => *n,
                _ => 0,
            };
            m.get("lockfileVersion").and_then(|v| v.as_u64()) == Some(expected)
        });
        if !ok {
            return Err(err(category, format!("{label}.lockfile does not match its npm project")));
        }
    }
    let ws = record.get("workspaceFiles").unwrap_or(&null);
    let Some(ws_seq) = ws.as_seq() else {
        return Err(err(category, format!("{label}.workspaceFiles must be an array")));
    };
    let mut workspace_files = Vec::new();
    for (index, file) in ws_seq.iter().enumerate() {
        let file_label = format!("{label}.workspaceFiles[{index}]");
        let fr = require_record(file, &file_label, category)?;
        exact_keys(fr, &["path", "contents"], &file_label, category)?;
        let path = normalize_snapshot_path(fr.get("path").unwrap_or(&null), &format!("{file_label}.path"), category)?;
        let contents = fr.get("contents").unwrap_or(&null).as_str().ok_or_else(|| err(category, format!("{file_label}.contents must be a string")))?.to_string();
        let is_ws_manifest = path.ends_with("/package.json");
        let is_pnpm_cfg = *manager == Manager::Pnpm && path == "pnpm-workspace.yaml";
        if !is_ws_manifest && !is_pnpm_cfg {
            return Err(err(category, format!("{file_label}.path is not a workspace manifest or pnpm workspace configuration")));
        }
        let dup_root = path == "package.json" || path == if *manager == Manager::Npm { "package-lock.json" } else { "pnpm-lock.yaml" };
        if dup_root {
            return Err(err(category, format!("{file_label}.path duplicates a project snapshot file")));
        }
        if path.ends_with("/package.json") || path == "package.json" {
            let parsed: deser_value::Value = deser_json::from_str(&contents)
                .map_err(|_| err(category, format!("{file_label}.contents is not valid JSON")))?;
            if parsed.as_map().is_none() {
                return Err(err(category, format!("{file_label}.contents must contain an object")));
            }
        }
        workspace_files.push(SnapshotFile { path, contents });
    }
    workspace_files.sort_by(|a, b| a.path.cmp(&b.path));
    let paths: Vec<&str> = workspace_files.iter().map(|f| f.path.as_str()).collect();
    let mut dedup = paths.clone();
    dedup.sort();
    dedup.dedup();
    if dedup.len() != paths.len() {
        return Err(err(category, format!("{label}.workspaceFiles contains duplicate paths")));
    }
    let mut npm_install_options = NpmInstallOptions { legacy_peer_deps: None, install_links: None };
    if let Some(opts) = record.get("npmInstallOptions")
        && !matches!(&**opts, deser_value::Kind::Null) {
            let om = require_record(opts, &format!("{label}.npmInstallOptions"), category)?;
            exact_keys(om, &["legacyPeerDeps", "installLinks"], &format!("{label}.npmInstallOptions"), category)?;
            for (key, field) in [("legacyPeerDeps", true), ("installLinks", false)] {
                match om.get(key) {
                    Some(bv) if matches!(&**bv, deser_value::Kind::Null) => {}
                    Some(bv) => {
                        let b = bv.as_bool().ok_or_else(|| err(category, format!("{label}.npmInstallOptions.{key} must be a boolean or null")))?;
                        if field {
                            npm_install_options.legacy_peer_deps = Some(b);
                        } else {
                            npm_install_options.install_links = Some(b);
                        }
                    }
                    None => {}
                }
            }
        }
    if *manager == Manager::Pnpm && (npm_install_options.legacy_peer_deps.is_some() || npm_install_options.install_links.is_some()) {
        return Err(err(category, format!("{label}.npmInstallOptions must be empty for pnpm")));
    }
    Ok(Some(ProjectSnapshot { manifest, lockfile, workspace_files, npm_install_options }))
}

fn normalize_flags(v: &deser_value::Value, label: &str, category: Category) -> Result<PackageFlags> {
    let record = require_record(v, label, category)?;
    exact_keys(record, &["dev", "optional", "peer", "peerOptional", "link"], label, category)?;
    let get = |key: &str| {
        record.get(key).and_then(|b| b.as_bool()).ok_or_else(|| err(category, format!("{label}.{key} must be a boolean")))
    };
    Ok(PackageFlags {
        dev: get("dev")?,
        optional: get("optional")?,
        peer: get("peer")?,
        peer_optional: get("peerOptional")?,
        link: get("link")?,
    })
}

fn normalize_platform(v: &deser_value::Value, label: &str, category: Category) -> Result<PlatformConstraints> {
    let record = require_record(v, label, category)?;
    exact_keys(record, &["os", "cpu", "libc"], label, category)?;
    let read = |key: &str| -> Result<Option<Vec<String>>> {
        let null = deser_value::Value::from(deser_value::Kind::Null);
        let field = record.get(key).unwrap_or(&null);
        if matches!(&**field, deser_value::Kind::Null) {
            return Ok(None);
        }
        Ok(Some(require_list(field, &format!("{label}.{key}"), category, false)?))
    };
    Ok(PlatformConstraints { os: read("os")?, cpu: read("cpu")?, libc: read("libc")? })
}

fn normalize_json_value(v: &deser_value::Value, label: &str, category: Category) -> Result<deser_value::Value> {
    match &**v {
        deser_value::Kind::Null | deser_value::Kind::Bool(_) | deser_value::Kind::U64(_) | deser_value::Kind::I64(_)
        | deser_value::Kind::F32(_) | deser_value::Kind::F64(_) | deser_value::Kind::Str(_) | deser_value::Kind::Char(_) => {
            Ok(v.clone())
        }
        _ if v.as_seq().is_some() => {
            let seq = v.as_seq().expect("seq");
            let mut out = Vec::with_capacity(seq.len());
            for (i, item) in seq.iter().enumerate() {
                out.push(normalize_json_value(item, &format!("{label}[{i}]"), category)?);
            }
            let mut values = Vec::with_capacity(out.len());
            for item in out {
                values.push(item);
            }
            let mut seq_out = deser_value::Seq::new();
            for item in values {
                seq_out.push(item);
            }
            Ok(deser_value::Value::from(seq_out))
        }
        _ if v.as_map().is_some() => {
            let map = v.as_map().expect("map");
            let mut out = deser_value::Map::new();
            for i in 0..map.len() {
                let Some((k, item)) = map.get_index(i) else { continue };
                let key = k.as_str().ok_or_else(|| err(category, format!("{label} has a non-string key")))?.to_string();
                let item_label = format!("{label}.{key}");
                out.insert(key, normalize_json_value(item, &item_label, category)?);
            }
            Ok(deser_value::Value::from(out))
        }
        _ => Err(err(category, format!("{label} is not JSON serializable"))),
    }
}

fn canonical_json_object(v: &deser_value::Value, label: &str, category: Category) -> Result<deser_value::Value> {
    let serialized = deser_json::to_string(v).map_err(|_| err(category, format!("{label} is not JSON serializable")))?;
    let reparsed: deser_value::Value = deser_json::from_str(&serialized)
        .map_err(|_| err(category, format!("{label} is not JSON serializable")))?;
    let normalized = normalize_json_value(&reparsed, label, category)?;
    let mut owned = normalized;
    sort_canonical_value(&mut owned);
    Ok(owned)
}

fn normalize_metadata(
    v: &deser_value::Value,
    label: &str,
    category: Category,
    name: &str,
    version: &str,
) -> Result<Option<deser_value::Value>> {
    if matches!(&**v, deser_value::Kind::Null) {
        return Ok(None);
    }
    let record = require_record(v, label, category)?;
    let mut canonical = canonical_json_object(v, label, category)?;
    let cmap = canonical.as_map().ok_or_else(|| err(category, format!("{label} must be an object")))?;
    let cname = cmap.get("name").and_then(|x| x.as_str()).unwrap_or("");
    let cversion = cmap.get("version").and_then(|x| x.as_str()).unwrap_or("");
    if cname != name || cversion != version {
        return Err(err(category, format!("{label} identity does not match {name}@{version}")));
    }
    if let Some(dist) = cmap.get("dist").cloned() {
        if matches!(&*dist, deser_value::Kind::Null) {
            // leave null dist as-is
        } else {
            let dmap = require_record(&dist, &format!("{label}.dist"), category)?;
            let mut normalized: BTreeMap<String, deser_value::Value> = BTreeMap::new();
            for i in 0..dmap.len() {
                if let Some((k, item)) = dmap.get_index(i) {
                    let key = k.as_str().unwrap_or("").to_string();
                    normalized.insert(key, item.clone());
                }
            }
            if let Some(integrity) = normalized.get("integrity").cloned()
                && !matches!(&*integrity, deser_value::Kind::Null) {
                    let text = integrity.as_str().ok_or_else(|| err(category, format!("{label}.dist.integrity must be a string")))?;
                    validate_integrity(Some(text), &format!("{label}.dist.integrity"), category)?;
                }
            if let Some(shasum) = normalized.get("shasum").cloned()
                && !matches!(&*shasum, deser_value::Kind::Null) {
                    let text = shasum.as_str().unwrap_or("");
                    if !is_sha1_hex(text) {
                        return Err(err(category, format!("{label}.dist.shasum must be a lowercase SHA-1 hex digest")));
                    }
                }
            let mut new_map = deser_value::Map::new();
            for (k, item) in normalized {
                new_map.insert(k, item);
            }
            if let Some(cmap_mut) = canonical.as_map_mut() {
                cmap_mut.insert("dist", deser_value::Value::from(new_map));
            }
            sort_canonical_value(&mut canonical);
        }
    }
    let serialized = canonical_string(&canonical).map_err(|_| err(category, format!("{label} is not JSON serializable")))?;
    if serialized.len() as u64 > crate::limits::METADATA_BYTES {
        return Err(err(category, format!("{label} exceeds the {} metadata limit", crate::limits::format_bytes(crate::limits::METADATA_BYTES))));
    }
    let _ = record;
    Ok(Some(canonical))
}

fn normalize_packument_metadata(
    v: &deser_value::Value,
    label: &str,
    category: Category,
    name: &str,
) -> Result<Option<deser_value::Value>> {
    if matches!(&**v, deser_value::Kind::Null) {
        return Ok(None);
    }
    require_record(v, label, category)?;
    let canonical = canonical_json_object(v, label, category)?;
    let cmap = canonical.as_map().ok_or_else(|| err(category, format!("{label} must be an object")))?;
    let cname = cmap.get("name").and_then(|x| x.as_str()).unwrap_or("");
    if cname != name || cmap.get("versions").is_some() {
        return Err(err(category, format!("{label} identity or shape does not match {name}")));
    }
    let serialized = canonical_string(&canonical).map_err(|_| err(category, format!("{label} is not JSON serializable")))?;
    if serialized.len() as u64 > crate::limits::METADATA_BYTES {
        return Err(err(category, format!("{label} exceeds the {} metadata limit", crate::limits::format_bytes(crate::limits::METADATA_BYTES))));
    }
    Ok(Some(canonical))
}

fn normalize_artifact(
    v: &deser_value::Value,
    label: &str,
    category: Category,
    metadata: Option<&deser_value::Value>,
) -> Result<Option<(deser_value::Value, ArtifactVerification)>> {
    if matches!(&**v, deser_value::Kind::Null) {
        return Ok(None);
    }
    let record = require_record(v, label, category)?;
    let first_party = match record.get("firstParty") {
        None => false,
        Some(flag) if flag.as_bool() == Some(true) => true,
        Some(_) => return Err(err(category, format!("{label}.firstParty must be true when present"))),
    };
    let mut keys = vec!["sha512", "integrity", "shasum", "sourceUrl", "legacySha1Only"];
    if first_party {
        keys.push("firstParty");
    }
    exact_keys(record, &keys, label, category)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let sha512 = require_text(record.get("sha512").unwrap_or(&null), &format!("{label}.sha512"), category)?;
    if !is_sha512_hex(&sha512) {
        return Err(err(category, format!("{label}.sha512 must be a lowercase SHA-512 hex digest")));
    }
    let integrity = require_text(record.get("integrity").unwrap_or(&null), &format!("{label}.integrity"), category)?;
    let raw = (0..sha512.len()).step_by(2).map(|i| u8::from_str_radix(&sha512[i..i + 2], 16).unwrap_or(0)).collect::<Vec<u8>>();
    let expected = format!("sha512-{}", B64.encode(&raw));
    if integrity != expected {
        return Err(err(category, format!("{label}.integrity does not match {label}.sha512")));
    }
    let shasum = require_nullable_text(record.get("shasum").unwrap_or(&null), &format!("{label}.shasum"), category)?;
    if let Some(ref s) = shasum
        && !is_sha1_hex(s) {
            return Err(err(category, format!("{label}.shasum must be a lowercase SHA-1 hex digest")));
        }
    let source_url = require_text(record.get("sourceUrl").unwrap_or(&null), &format!("{label}.sourceUrl"), category)?;
    let url = stuff_url::Url::parse(&source_url).map_err(|_| err(category, format!("{label}.sourceUrl must be a public npm HTTPS URL")))?;
    let host = url.host_str().unwrap_or("");
    let is_loopback = url.scheme() == "http"
        && matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    let scheme_allowed = url.scheme() == "https" || (first_party && is_loopback);
    let source_host_allowed = first_party || host == "registry.npmjs.org";
    if !scheme_allowed || !source_host_allowed || (!first_party && url.port().is_some()) || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(err(category, format!("{label}.sourceUrl must be a public npm HTTPS URL")));
    }
    let dist = metadata.and_then(|m| m.as_map()).and_then(|m| m.get("dist")).cloned();
    let dist_map = match dist {
        Some(d) if !matches!(&*d, deser_value::Kind::Null) => Some(require_record(&d, &format!("{label} metadata dist"), category)?.clone()),
        _ => None,
    };
    let dist_tarball = dist_map.as_ref().and_then(|m| m.get("tarball")).and_then(|t| t.as_str()).map(|s| s.to_string());
    if metadata.is_none() || dist_tarball.as_deref() != Some(source_url.as_str()) {
        return Err(err(category, format!("{label}.sourceUrl does not match upstream metadata")));
    }
    let dist_shasum = dist_map.as_ref().and_then(|m| m.get("shasum")).and_then(|t| t.as_str()).map(|s| s.to_string());
    if dist_shasum.is_some() && dist_shasum != shasum {
        return Err(err(category, format!("{label}.shasum does not match upstream metadata")));
    }
    let legacy = match record.get("legacySha1Only").unwrap_or(&null) {
        bv if bv.as_bool().is_some() => bv.as_bool().expect("bool"),
        _ => return Err(err(category, format!("{label}.legacySha1Only must be a boolean"))),
    };
    let mut meta_dist = deser_value::Map::new();
    if let Some(dm) = dist_map {
        for i in 0..dm.len() {
            if let Some((k, item)) = dm.get_index(i) {
                meta_dist.insert(k.clone(), item.clone());
            }
        }
    }
    meta_dist.insert("tarball".to_string(), deser_value::Value::from(source_url.clone()));
    let mut meta_out = deser_value::Map::new();
    if let Some(m) = metadata.and_then(|m| m.as_map()) {
        for i in 0..m.len() {
            if let Some((k, item)) = m.get_index(i) {
                meta_out.insert(k.clone(), item.clone());
            }
        }
    }
    meta_out.insert("dist".to_string(), deser_value::Value::from(meta_dist));
    let mut meta_val = deser_value::Value::from(meta_out);
    sort_canonical_value(&mut meta_val);
    Ok(Some((meta_val, ArtifactVerification { sha512, integrity, shasum, source_url, legacy_sha1_only: legacy, first_party })))
}

pub fn normalize_artifact_record(v: &deser_value::Value, label: &str, category: Category) -> Result<ArtifactRecord> {
    let record = require_record(v, label, category)?;
    exact_keys(record, &["name", "version", "packument", "metadata", "artifact"], label, category)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let name = require_text(record.get("name").unwrap_or(&null), &format!("{label}.name"), category)?;
    let version = require_text(record.get("version").unwrap_or(&null), &format!("{label}.version"), category)?;
    if !valid_package_name(&name) || !valid_version(&version) {
        return Err(err(category, format!("{label} has an invalid package identity")));
    }
    let packument = normalize_packument_metadata(record.get("packument").unwrap_or(&null), &format!("{label}.packument"), category, &name)?;
    let metadata = normalize_metadata(record.get("metadata").unwrap_or(&null), &format!("{label}.metadata"), category, &name, &version)?;
    let normalized = normalize_artifact(record.get("artifact").unwrap_or(&null), &format!("{label}.artifact"), category, metadata.as_ref())?;
    let (Some(packument), Some(metadata), Some((metadata_out, artifact))) = (packument, metadata, normalized) else {
        return Err(err(category, format!("{label} must include packument metadata, version metadata, and a verified artifact")));
    };
    let combined = format!("{}{}", canonical_string(&packument).unwrap_or_default(), canonical_string(&metadata_out).unwrap_or_default());
    if combined.len() as u64 > crate::limits::METADATA_BYTES {
        return Err(err(category, format!("{label} exceeds the {} metadata limit", crate::limits::format_bytes(crate::limits::METADATA_BYTES))));
    }
    let _ = metadata;
    Ok(ArtifactRecord { name, version, packument, metadata: metadata_out, artifact })
}

fn validate_artifact_digests(record: &ArtifactRecord, category: Category) -> Result<()> {
    let dist_integrity = record.metadata.as_map().and_then(|m| m.get("dist")).and_then(|d| d.as_map()).and_then(|m| m.get("integrity")).and_then(|v| v.as_str());
    let mut claims = digest_claims(dist_integrity, &format!("upstream metadata for {}@{}", record.name, record.version), category)?;
    let dist_shasum = record.metadata.as_map().and_then(|m| m.get("dist")).and_then(|d| d.as_map()).and_then(|m| m.get("shasum")).and_then(|v| v.as_str());
    if let Some(shasum) = dist_shasum {
        let mut extra = BTreeMap::new();
        extra.insert("sha1".to_string(), shasum.to_string());
        add_digest_claims(&mut claims, &extra, &format!("{}@{}", record.name, record.version), category)?;
    }
    let legacy = !claims.is_empty() && claims.keys().all(|a| a == "sha1");
    if record.artifact.legacy_sha1_only != legacy {
        return Err(err(category, format!("legacy SHA-1 policy marker is inconsistent for {}@{}", record.name, record.version)));
    }
    let mut extra = BTreeMap::new();
    extra.insert("sha512".to_string(), record.artifact.sha512.clone());
    add_digest_claims(&mut claims, &extra, &format!("{}@{}", record.name, record.version), category)?;
    if let Some(ref s) = record.artifact.shasum {
        let mut extra = BTreeMap::new();
        extra.insert("sha1".to_string(), s.clone());
        add_digest_claims(&mut claims, &extra, &format!("{}@{}", record.name, record.version), category)?;
    }
    Ok(())
}

pub fn validate_artifact_record(v: &deser_value::Value) -> Result<ArtifactRecord> {
    let record = normalize_artifact_record(v, "verified artifact", Category::Corrupt)?;
    validate_artifact_digests(&record, Category::Corrupt)?;
    Ok(record)
}

fn valid_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 20
        && b.ends_with(b"Z")
        && b[..19].iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            _ => c.is_ascii_digit(),
        })
}

pub fn normalize_metadata_only_record(v: &deser_value::Value, label: &str, category: Category) -> Result<MetadataOnlyRecord> {
    let record = require_record(v, label, category)?;
    exact_keys(record, &["name", "version", "integrity", "publishedAt", "platform"], label, category)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let name = require_text(record.get("name").unwrap_or(&null), &format!("{label}.name"), category)?;
    let version = require_text(record.get("version").unwrap_or(&null), &format!("{label}.version"), category)?;
    if !valid_package_name(&name) || !valid_version(&version) {
        return Err(err(category, format!("{label} has an invalid package identity")));
    }
    let integrity = require_text(record.get("integrity").unwrap_or(&null), &format!("{label}.integrity"), category)?;
    validate_integrity(Some(&integrity), &format!("{label}.integrity"), category)?;
    let claims = digest_claims(Some(&integrity), &format!("{label}.integrity"), category)?;
    if claims.len() != 1 || !claims.contains_key("sha512") || integrity.split_whitespace().count() != 1 {
        return Err(err(category, format!("{label}.integrity must be a single sha512 claim")));
    }
    let published_at = require_text(record.get("publishedAt").unwrap_or(&null), &format!("{label}.publishedAt"), category)?;
    if !valid_timestamp(&published_at) {
        return Err(err(category, format!("{label}.publishedAt is not an RFC 3339 UTC timestamp")));
    }
    let platform = normalize_platform(record.get("platform").unwrap_or(&null), &format!("{label}.platform"), category)?;
    Ok(MetadataOnlyRecord { name, version, integrity, published_at, platform })
}

fn validate_metadata_only(
    projects: &[ProjectRecord],
    artifacts: &[ArtifactRecord],
    records: &[MetadataOnlyRecord],
    category: Category,
) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for record in records {
        let identity = format!("{}@{}", record.name, record.version);
        if !seen.insert(identity.clone()) {
            return Err(err(category, format!("inventory contains duplicate metadata-only identities: {identity}")));
        }
        if artifacts.iter().any(|a| a.name == record.name && a.version == record.version) {
            return Err(err(category, format!("{identity} is both hosted and metadata-only")));
        }
        let mut sources = Vec::new();
        for project in projects {
            for pkg in &project.packages {
                if pkg.name == record.name && pkg.version.as_deref() == Some(record.version.as_str()) {
                    sources.push(pkg);
                }
            }
        }
        if sources.is_empty() || sources.iter().any(|p| p.source_kind != SourceKind::Registry) {
            return Err(err(category, format!("metadata-only {identity} has no registry package record")));
        }
        let wanted = digest_claims(Some(&record.integrity), &identity, category)?;
        for pkg in sources {
            let locked = digest_claims(pkg.integrity.as_deref(), &identity, category)?;
            if locked.get("sha512") != wanted.get("sha512") {
                return Err(err(category, format!("metadata-only integrity conflicts with the lockfile for {identity}")));
            }
            if pkg.platform != record.platform {
                return Err(err(category, format!("metadata-only platform conflicts with the lockfile for {identity}")));
            }
        }
    }
    Ok(())
}

fn normalize_package(v: &deser_value::Value, label: &str, category: Category) -> Result<PackageRecord> {
    let record = require_record(v, label, category)?;
    exact_keys(record, &["key", "name", "version", "integrity", "resolved", "sourceKind", "aliases", "flags", "platform", "targets"], label, category)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let name = require_text(record.get("name").unwrap_or(&null), &format!("{label}.name"), category)?;
    if !valid_package_name(&name) {
        return Err(err(category, format!("{label}.name is not a valid package name")));
    }
    let version = require_nullable_text(record.get("version").unwrap_or(&null), &format!("{label}.version"), category)?;
    if let Some(ref ver) = version
        && !valid_version(ver) {
            return Err(err(category, format!("{label}.version is not an exact package version")));
        }
    let kind_str = require_text(record.get("sourceKind").unwrap_or(&null), &format!("{label}.sourceKind"), category)?;
    let source_kind = SourceKind::parse(&kind_str).ok_or_else(|| err(category, format!("{label}.sourceKind is unsupported")))?;
    let integrity_raw = match record.get("integrity").unwrap_or(&null) {
        iv if matches!(&**iv, deser_value::Kind::Null) => None,
        iv => Some(iv.as_str().ok_or_else(|| err(category, format!("{label}.integrity must be a string")))?.to_string()),
    };
    let integrity = validate_integrity(integrity_raw.as_deref(), &format!("{label}.integrity"), category)?;
    Ok(PackageRecord {
        key: require_text(record.get("key").unwrap_or(&null), &format!("{label}.key"), category)?,
        name,
        version,
        integrity,
        resolved: require_nullable_text(record.get("resolved").unwrap_or(&null), &format!("{label}.resolved"), category)?,
        source_kind,
        aliases: require_list(record.get("aliases").unwrap_or(&null), &format!("{label}.aliases"), category, true)?,
        flags: normalize_flags(record.get("flags").unwrap_or(&null), &format!("{label}.flags"), category)?,
        platform: normalize_platform(record.get("platform").unwrap_or(&null), &format!("{label}.platform"), category)?,
        targets: require_list(record.get("targets").unwrap_or(&null), &format!("{label}.targets"), category, false)?,
    })
}

fn normalize_root(v: &deser_value::Value, label: &str, category: Category) -> Result<RootDependency> {
    let record = require_record(v, label, category)?;
    exact_keys(record, &["importer", "requestedName", "specifier", "packageName", "packageVersion", "group", "packageKey"], label, category)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let requested = require_text(record.get("requestedName").unwrap_or(&null), &format!("{label}.requestedName"), category)?;
    let pkg_name = require_text(record.get("packageName").unwrap_or(&null), &format!("{label}.packageName"), category)?;
    if !valid_package_name(&requested) || !valid_package_name(&pkg_name) {
        return Err(err(category, format!("{label} contains an invalid package name")));
    }
    let pkg_version = require_nullable_text(record.get("packageVersion").unwrap_or(&null), &format!("{label}.packageVersion"), category)?;
    if let Some(ref ver) = pkg_version
        && !valid_version(ver) {
            return Err(err(category, format!("{label}.packageVersion is not an exact package version")));
        }
    let group = require_text(record.get("group").unwrap_or(&null), &format!("{label}.group"), category)?;
    if !["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"].contains(&group.as_str()) {
        return Err(err(category, format!("{label}.group is unsupported")));
    }
    Ok(RootDependency {
        importer: normalize_importer(record.get("importer").unwrap_or(&null), &format!("{label}.importer"), category)?,
        requested_name: requested,
        specifier: require_text(record.get("specifier").unwrap_or(&null), &format!("{label}.specifier"), category)?,
        package_name: pkg_name,
        package_version: pkg_version,
        group,
        package_key: require_nullable_text(record.get("packageKey").unwrap_or(&null), &format!("{label}.packageKey"), category)?,
    })
}

fn normalize_project(v: &deser_value::Value, label: &str, category: Category) -> Result<ProjectRecord> {
    let record = require_record(v, label, category)?;
    exact_keys(record, &["id", "manager", "packageManagerPin", "lockfilePath", "lockfileVersion", "targets", "importers", "roots", "packages", "snapshot"], label, category)?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let manager_str = require_text(record.get("manager").unwrap_or(&null), &format!("{label}.manager"), category)?;
    let manager = match manager_str.as_str() {
        "npm" => Manager::Npm,
        "pnpm" => Manager::Pnpm,
        _ => return Err(err(category, format!("{label}.manager is unsupported"))),
    };
    let pin = require_nullable_text(record.get("packageManagerPin").unwrap_or(&null), &format!("{label}.packageManagerPin"), category)?;
    if let Some(ref p) = pin
        && !valid_manager_pin(p, &manager) {
            return Err(err(category, format!("{label}.packageManagerPin does not match its package manager")));
        }
    let lockfile_version = match manager {
        Manager::Npm => {
            let n = record.get("lockfileVersion").unwrap_or(&null).as_u64().ok_or_else(|| err(category, format!("{label}.lockfileVersion must be 2 or 3 for npm")))?;
            if n != 2 && n != 3 {
                return Err(err(category, format!("{label}.lockfileVersion must be 2 or 3 for npm")));
            }
            LockfileVersion::Npm(n)
        }
        Manager::Pnpm => {
            let s = record.get("lockfileVersion").unwrap_or(&null).as_str().ok_or_else(|| err(category, format!("{label}.lockfileVersion must be a major.minor string for pnpm")))?.to_string();
            let mut parts = s.split('.');
            let ok = matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if !a.is_empty() && !b.is_empty() && a.bytes().all(|x| x.is_ascii_digit()) && b.bytes().all(|x| x.is_ascii_digit()));
            if !ok {
                return Err(err(category, format!("{label}.lockfileVersion must be a major.minor string for pnpm")));
            }
            LockfileVersion::Pnpm(s)
        }
    };
    let roots_val = record.get("roots").unwrap_or(&null);
    let pkgs_val = record.get("packages").unwrap_or(&null);
    let (Some(roots_seq), Some(pkgs_seq)) = (roots_val.as_seq(), pkgs_val.as_seq()) else {
        return Err(err(category, format!("{label} roots and packages must be arrays")));
    };
    let mut roots = Vec::with_capacity(roots_seq.len());
    for (i, r) in roots_seq.iter().enumerate() {
        roots.push(normalize_root(r, &format!("{label}.roots[{i}]"), category)?);
    }
    roots.sort_by_key(|r| (r.importer.clone(), r.group.clone(), r.requested_name.clone(), r.specifier.clone()));
    let importers_raw = require_list(record.get("importers").unwrap_or(&null), &format!("{label}.importers"), category, true)?;
    let mut importers = Vec::with_capacity(importers_raw.len());
    for (i, imp) in importers_raw.iter().enumerate() {
        let owned = deser_value::Value::from(imp.clone());
        importers.push(normalize_importer(&owned, &format!("{label}.importers[{i}]"), category)?);
    }
    if roots.iter().any(|r| !importers.contains(&r.importer)) {
        return Err(err(category, format!("{label}.roots references an unknown importer")));
    }
    let mut packages = Vec::with_capacity(pkgs_seq.len());
    for (i, p) in pkgs_seq.iter().enumerate() {
        packages.push(normalize_package(p, &format!("{label}.packages[{i}]"), category)?);
    }
    packages.sort_by_key(|p| (p.key.clone(), p.name.clone(), p.version.clone().unwrap_or_default()));
    let keys: Vec<&str> = packages.iter().map(|p| p.key.as_str()).collect();
    let mut dedup = keys.clone();
    dedup.sort();
    dedup.dedup();
    if dedup.len() != keys.len() {
        return Err(err(category, format!("{label}.packages contains duplicate lockfile keys")));
    }
    let snapshot = normalize_snapshot(record.get("snapshot").unwrap_or(&null), &format!("{label}.snapshot"), category, &manager, &lockfile_version)?;
    Ok(ProjectRecord {
        id: require_text(record.get("id").unwrap_or(&null), &format!("{label}.id"), category)?,
        manager,
        package_manager_pin: pin,
        lockfile_path: require_text(record.get("lockfilePath").unwrap_or(&null), &format!("{label}.lockfilePath"), category)?,
        targets: require_list(record.get("targets").unwrap_or(&null), &format!("{label}.targets"), category, false)?,
        importers,
        roots,
        packages,
        snapshot,
        lockfile_version,
    })
}

fn validate_digest_conflicts(projects: &[ProjectRecord], artifacts: &[ArtifactRecord], category: Category) -> Result<()> {
    let mut claims_by_identity: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut sources_by_identity: BTreeMap<String, Vec<SourceKind>> = BTreeMap::new();
    for project in projects {
        for pkg in &project.packages {
            let Some(ref ver) = pkg.version else { continue };
            let identity = format!("{}\0{ver}", pkg.name);
            let claims = claims_by_identity.entry(identity.clone()).or_default();
            let pkg_claims = digest_claims(pkg.integrity.as_deref(), &format!("{}@{ver}", pkg.name), category)?;
            add_digest_claims(claims, &pkg_claims, &format!("{}@{ver}", pkg.name), category)?;
            sources_by_identity.entry(identity).or_default().push(pkg.source_kind.clone());
        }
    }
    for artifact in artifacts {
        let identity = format!("{}\0{}", artifact.name, artifact.version);
        let sources = sources_by_identity.get(&identity).cloned().unwrap_or_default();
        if !sources.contains(&SourceKind::Registry) {
            return Err(err(category, format!("verified artifact has no registry package record for {}@{}", artifact.name, artifact.version)));
        }
        if sources.iter().any(|s| *s != SourceKind::Registry) {
            return Err(err(category, format!("verified artifact conflicts with a non-registry source for {}@{}", artifact.name, artifact.version)));
        }
        let claims = claims_by_identity.entry(identity.clone()).or_default();
        let dist = artifact.metadata.as_map().and_then(|m| m.get("dist")).cloned().unwrap_or_else(|| deser_value::Value::from(deser_value::Kind::Null));
        let dist_integrity = dist.as_map().and_then(|m| m.get("integrity")).and_then(|v| v.as_str());
        let upstream = digest_claims(dist_integrity, &format!("upstream metadata for {}@{}", artifact.name, artifact.version), category)?;
        let dist_shasum = dist.as_map().and_then(|m| m.get("shasum")).and_then(|v| v.as_str());
        let mut upstream_claims = upstream;
        if let Some(shasum) = dist_shasum {
            if !is_sha1_hex(shasum) {
                return Err(err(category, format!("upstream shasum is malformed for {}@{}", artifact.name, artifact.version)));
            }
            let mut extra = BTreeMap::new();
            extra.insert("sha1".to_string(), shasum.to_string());
            add_digest_claims(&mut upstream_claims, &extra, &format!("{}@{}", artifact.name, artifact.version), category)?;
        }
        add_digest_claims(claims, &upstream_claims, &format!("{}@{}", artifact.name, artifact.version), category)?;
        let legacy = !claims.is_empty() && claims.keys().all(|a| a == "sha1");
        if artifact.artifact.legacy_sha1_only != legacy {
            return Err(err(category, format!("legacy SHA-1 policy marker is inconsistent for {}@{}", artifact.name, artifact.version)));
        }
        let mut extra = BTreeMap::new();
        extra.insert("sha512".to_string(), artifact.artifact.sha512.clone());
        add_digest_claims(claims, &extra, &format!("{}@{}", artifact.name, artifact.version), category)?;
        if let Some(ref s) = artifact.artifact.shasum {
            let mut extra = BTreeMap::new();
            extra.insert("sha1".to_string(), s.clone());
            add_digest_claims(claims, &extra, &format!("{}@{}", artifact.name, artifact.version), category)?;
        }
    }
    Ok(())
}

pub fn create_empty_inventory() -> Inventory {
    Inventory { projects: Vec::new(), artifacts: Vec::new(), metadata_only: Vec::new() }
}

fn project_sort_key(p: &ProjectRecord) -> (String, String, String, String, String) {
    let lockfile_version = match &p.lockfile_version {
        LockfileVersion::Npm(n) => n.to_string(),
        LockfileVersion::Pnpm(s) => s.clone(),
    };
    (
        p.id.clone(),
        p.lockfile_path.clone(),
        p.manager.as_str().to_string(),
        lockfile_version,
        p.targets.join(","),
    )
}

pub fn parse_inventory(text: &str) -> Result<Inventory> {
    let value: deser_value::Value = deser_json::from_str(text)
        .map_err(|_| Error::Corrupt("inventory is not valid JSON".to_string()))?;
    let record = require_record(&value, "inventory", Category::Corrupt)?;
    let version = record.get("schemaVersion").and_then(|v| v.as_u64()).unwrap_or(0);
    match version {
        SCHEMA_VERSION => exact_keys(record, &["schemaVersion", "projects", "artifacts"], "inventory", Category::Corrupt)?,
        METADATA_ONLY_SCHEMA_VERSION => exact_keys(
            record,
            &["schemaVersion", "projects", "artifacts", "metadataOnly"],
            "inventory",
            Category::Corrupt,
        )?,
        _ => return Err(err(Category::Corrupt, format!("unsupported inventory schemaVersion: {version}"))),
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let projects_val = record.get("projects").unwrap_or(&null);
    let artifacts_val = record.get("artifacts").unwrap_or(&null);
    let (Some(projects_seq), Some(artifacts_seq)) = (projects_val.as_seq(), artifacts_val.as_seq()) else {
        return Err(err(Category::Corrupt, "inventory projects and artifacts must be arrays".to_string()));
    };
    let mut projects = Vec::new();
    for (i, p) in projects_seq.iter().enumerate() {
        projects.push(normalize_project(p, &format!("inventory.projects[{i}]"), Category::Corrupt)?);
    }
    projects.sort_by_key(project_sort_key);
    let mut artifacts = Vec::new();
    for (i, a) in artifacts_seq.iter().enumerate() {
        artifacts.push(normalize_artifact_record(a, &format!("inventory.artifacts[{i}]"), Category::Corrupt)?);
    }
    artifacts.sort_by_key(|a| (a.name.clone(), a.version.clone()));
    let identities: Vec<String> = artifacts.iter().map(|a| format!("{}\0{}", a.name, a.version)).collect();
    let mut dedup = identities.clone();
    dedup.sort();
    dedup.dedup();
    if dedup.len() != identities.len() {
        return Err(err(Category::Corrupt, "inventory contains duplicate verified artifact identities".to_string()));
    }
    validate_digest_conflicts(&projects, &artifacts, Category::Corrupt)?;
    let mut metadata_only = Vec::new();
    if let Some(seq) = record.get("metadataOnly").and_then(|v| v.as_seq()) {
        for (i, r) in seq.iter().enumerate() {
            metadata_only.push(normalize_metadata_only_record(r, &format!("inventory.metadataOnly[{i}]"), Category::Corrupt)?);
        }
    } else if version == METADATA_ONLY_SCHEMA_VERSION {
        return Err(err(Category::Corrupt, "inventory metadataOnly must be an array".to_string()));
    }
    metadata_only.sort_by_key(|r| (r.name.clone(), r.version.clone()));
    validate_metadata_only(&projects, &artifacts, &metadata_only, Category::Corrupt)?;
    let stable: Vec<String> = projects.iter().map(project_stable_string).collect::<Result<Vec<_>>>()?;
    let mut dedup2 = stable.clone();
    dedup2.sort();
    dedup2.dedup();
    if dedup2.len() != stable.len() {
        return Err(err(Category::Corrupt, "inventory contains duplicate project imports".to_string()));
    }
    Ok(Inventory { projects, artifacts, metadata_only })
}

fn project_stable_string(p: &ProjectRecord) -> Result<String> {
    let mut map = BTreeMap::new();
    map.insert("id".to_string(), p.id.clone());
    map.insert("lockfilePath".to_string(), p.lockfile_path.clone());
    map.insert("manager".to_string(), p.manager.as_str().to_string());
    let lv = match &p.lockfile_version {
        LockfileVersion::Npm(n) => n.to_string(),
        LockfileVersion::Pnpm(s) => s.clone(),
    };
    map.insert("lockfileVersion".to_string(), lv);
    map.insert("targets".to_string(), p.targets.join(","));
    map.insert("importers".to_string(), p.importers.join(","));
    map.insert(
        "roots".to_string(),
        p.roots.iter().map(|r| format!("{}|{}|{}|{}|{}|{}|{}", r.importer, r.requested_name, r.specifier, r.package_name, r.package_version.clone().unwrap_or_default(), r.group, r.package_key.clone().unwrap_or_default())).collect::<Vec<_>>().join(";"),
    );
    map.insert(
        "packages".to_string(),
        p.packages
            .iter()
            .map(|pkg| {
                Ok(format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}|{}",
                    pkg.key,
                    pkg.name,
                    pkg.version.clone().unwrap_or_default(),
                    pkg.integrity.clone().unwrap_or_default(),
                    pkg.resolved.clone().unwrap_or_default(),
                    pkg.source_kind.as_str(),
                    canonical_string(&platform_to_value(&pkg.platform))?,
                    package_flags_stable(&pkg.flags),
                    pkg.aliases.join("\u{1f}"),
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(";"),
    );
    map.insert("snapshot".to_string(), snapshot_digest(p)?);
    map.insert("pin".to_string(), p.package_manager_pin.clone().unwrap_or_default());
    Ok(format!("{map:?}"))
}

fn package_flags_stable(flags: &PackageFlags) -> String {
    format!(
        "{},{},{},{},{}",
        flags.dev, flags.optional, flags.peer, flags.peer_optional, flags.link
    )
}

fn snapshot_digest(p: &ProjectRecord) -> Result<String> {
    let canonical = canonical_string(&snapshot_to_value(&p.snapshot)?)?;
    let mut digest = Sha256::new();
    digest.update(canonical.as_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

fn inventory_to_value(inv: &Inventory) -> Result<deser_value::Value> {
    let mut root = deser_value::Map::new();
    let version = if inv.metadata_only.is_empty() { SCHEMA_VERSION } else { METADATA_ONLY_SCHEMA_VERSION };
    root.insert("schemaVersion".to_string(), deser_value::Value::from(version));
    let mut projects = deser_value::Seq::new();
    for p in &inv.projects {
        projects.push(project_to_value(p)?);
    }
    root.insert("projects".to_string(), deser_value::Value::from(projects));
    let mut artifacts = deser_value::Seq::new();
    for a in &inv.artifacts {
        artifacts.push(artifact_to_value(a)?);
    }
    root.insert("artifacts".to_string(), deser_value::Value::from(artifacts));
    if !inv.metadata_only.is_empty() {
        let mut stubs = deser_value::Seq::new();
        for r in &inv.metadata_only {
            stubs.push(metadata_only_to_value(r));
        }
        root.insert("metadataOnly".to_string(), deser_value::Value::from(stubs));
    }
    let mut out = deser_value::Value::from(root);
    sort_canonical_value(&mut out);
    Ok(out)
}

pub fn project_to_value(p: &ProjectRecord) -> Result<deser_value::Value> {
    let mut m = deser_value::Map::new();
    m.insert("id".to_string(), deser_value::Value::from(p.id.clone()));
    m.insert("manager".to_string(), deser_value::Value::from(p.manager.as_str().to_string()));
    m.insert("packageManagerPin".to_string(), match &p.package_manager_pin {
        Some(s) => deser_value::Value::from(s.clone()),
        None => deser_value::Value::from(deser_value::Kind::Null),
    });
    m.insert("lockfilePath".to_string(), deser_value::Value::from(p.lockfile_path.clone()));
    m.insert("lockfileVersion".to_string(), match &p.lockfile_version {
        LockfileVersion::Npm(n) => deser_value::Value::from(*n),
        LockfileVersion::Pnpm(s) => deser_value::Value::from(s.clone()),
    });
    let mut targets = deser_value::Seq::new();
    for t in &p.targets {
        targets.push(deser_value::Value::from(t.clone()));
    }
    m.insert("targets".to_string(), deser_value::Value::from(targets));
    let mut importers = deser_value::Seq::new();
    for t in &p.importers {
        importers.push(deser_value::Value::from(t.clone()));
    }
    m.insert("importers".to_string(), deser_value::Value::from(importers));
    let mut roots = deser_value::Seq::new();
    for r in &p.roots {
        let mut rm = deser_value::Map::new();
        rm.insert("importer".to_string(), deser_value::Value::from(r.importer.clone()));
        rm.insert("requestedName".to_string(), deser_value::Value::from(r.requested_name.clone()));
        rm.insert("specifier".to_string(), deser_value::Value::from(r.specifier.clone()));
        rm.insert("packageName".to_string(), deser_value::Value::from(r.package_name.clone()));
        rm.insert("packageVersion".to_string(), match &r.package_version {
            Some(s) => deser_value::Value::from(s.clone()),
            None => deser_value::Value::from(deser_value::Kind::Null),
        });
        rm.insert("group".to_string(), deser_value::Value::from(r.group.clone()));
        rm.insert("packageKey".to_string(), match &r.package_key {
            Some(s) => deser_value::Value::from(s.clone()),
            None => deser_value::Value::from(deser_value::Kind::Null),
        });
        roots.push(deser_value::Value::from(rm));
    }
    m.insert("roots".to_string(), deser_value::Value::from(roots));
    let mut packages = deser_value::Seq::new();
    for pkg in &p.packages {
        let mut pm = deser_value::Map::new();
        pm.insert("key".to_string(), deser_value::Value::from(pkg.key.clone()));
        pm.insert("name".to_string(), deser_value::Value::from(pkg.name.clone()));
        pm.insert("version".to_string(), match &pkg.version {
            Some(s) => deser_value::Value::from(s.clone()),
            None => deser_value::Value::from(deser_value::Kind::Null),
        });
        pm.insert("integrity".to_string(), match &pkg.integrity {
            Some(s) => deser_value::Value::from(s.clone()),
            None => deser_value::Value::from(deser_value::Kind::Null),
        });
        pm.insert("resolved".to_string(), match &pkg.resolved {
            Some(s) => deser_value::Value::from(s.clone()),
            None => deser_value::Value::from(deser_value::Kind::Null),
        });
        pm.insert("sourceKind".to_string(), deser_value::Value::from(pkg.source_kind.as_str().to_string()));
        let mut aliases = deser_value::Seq::new();
        for a in &pkg.aliases {
            aliases.push(deser_value::Value::from(a.clone()));
        }
        pm.insert("aliases".to_string(), deser_value::Value::from(aliases));
        let mut flags = deser_value::Map::new();
        flags.insert("dev".to_string(), deser_value::Value::from(pkg.flags.dev));
        flags.insert("optional".to_string(), deser_value::Value::from(pkg.flags.optional));
        flags.insert("peer".to_string(), deser_value::Value::from(pkg.flags.peer));
        flags.insert("peerOptional".to_string(), deser_value::Value::from(pkg.flags.peer_optional));
        flags.insert("link".to_string(), deser_value::Value::from(pkg.flags.link));
        pm.insert("flags".to_string(), deser_value::Value::from(flags));
        let platform = platform_to_value(&pkg.platform);
        pm.insert("platform".to_string(), platform);
        let mut targets = deser_value::Seq::new();
        for t in &pkg.targets {
            targets.push(deser_value::Value::from(t.clone()));
        }
        pm.insert("targets".to_string(), deser_value::Value::from(targets));
        packages.push(deser_value::Value::from(pm));
    }
    m.insert("packages".to_string(), deser_value::Value::from(packages));
    m.insert("snapshot".to_string(), snapshot_to_value(&p.snapshot)?);
    Ok(deser_value::Value::from(m))
}

fn platform_to_value(platform: &PlatformConstraints) -> deser_value::Value {
    let mut map = deser_value::Map::new();
    for (key, list) in [("os", &platform.os), ("cpu", &platform.cpu), ("libc", &platform.libc)] {
        let value = match list {
            Some(list) => {
                let mut seq = deser_value::Seq::new();
                for s in list {
                    seq.push(deser_value::Value::from(s.clone()));
                }
                deser_value::Value::from(seq)
            }
            None => deser_value::Value::from(deser_value::Kind::Null),
        };
        map.insert(key.to_string(), value);
    }
    deser_value::Value::from(map)
}

fn snapshot_to_value(s: &Option<ProjectSnapshot>) -> Result<deser_value::Value> {
    match s {
        None => Ok(deser_value::Value::from(deser_value::Kind::Null)),
        Some(snap) => {
            let mut m = deser_value::Map::new();
            m.insert("manifest".to_string(), match &snap.manifest {
                Some(s) => deser_value::Value::from(s.clone()),
                None => deser_value::Value::from(deser_value::Kind::Null),
            });
            m.insert("lockfile".to_string(), deser_value::Value::from(snap.lockfile.clone()));
            let mut files = deser_value::Seq::new();
            for f in &snap.workspace_files {
                let mut fm = deser_value::Map::new();
                fm.insert("path".to_string(), deser_value::Value::from(f.path.clone()));
                fm.insert("contents".to_string(), deser_value::Value::from(f.contents.clone()));
                files.push(deser_value::Value::from(fm));
            }
            m.insert("workspaceFiles".to_string(), deser_value::Value::from(files));
            let mut opts = deser_value::Map::new();
            opts.insert("legacyPeerDeps".to_string(), match snap.npm_install_options.legacy_peer_deps {
                Some(b) => deser_value::Value::from(b),
                None => deser_value::Value::from(deser_value::Kind::Null),
            });
            opts.insert("installLinks".to_string(), match snap.npm_install_options.install_links {
                Some(b) => deser_value::Value::from(b),
                None => deser_value::Value::from(deser_value::Kind::Null),
            });
            m.insert("npmInstallOptions".to_string(), deser_value::Value::from(opts));
            Ok(deser_value::Value::from(m))
        }
    }
}

pub fn artifact_to_value(a: &ArtifactRecord) -> Result<deser_value::Value> {
    let mut m = deser_value::Map::new();
    m.insert("name".to_string(), deser_value::Value::from(a.name.clone()));
    m.insert("version".to_string(), deser_value::Value::from(a.version.clone()));
    m.insert("packument".to_string(), a.packument.clone());
    m.insert("metadata".to_string(), a.metadata.clone());
    let mut am = deser_value::Map::new();
    am.insert("sha512".to_string(), deser_value::Value::from(a.artifact.sha512.clone()));
    am.insert("integrity".to_string(), deser_value::Value::from(a.artifact.integrity.clone()));
    am.insert("shasum".to_string(), match &a.artifact.shasum {
        Some(s) => deser_value::Value::from(s.clone()),
        None => deser_value::Value::from(deser_value::Kind::Null),
    });
    am.insert("sourceUrl".to_string(), deser_value::Value::from(a.artifact.source_url.clone()));
    am.insert("legacySha1Only".to_string(), deser_value::Value::from(a.artifact.legacy_sha1_only));
    if a.artifact.first_party {
        am.insert("firstParty".to_string(), deser_value::Value::from(true));
    }
    m.insert("artifact".to_string(), deser_value::Value::from(am));
    Ok(deser_value::Value::from(m))
}

pub fn metadata_only_to_value(r: &MetadataOnlyRecord) -> deser_value::Value {
    let mut m = deser_value::Map::new();
    m.insert("name".to_string(), deser_value::Value::from(r.name.clone()));
    m.insert("version".to_string(), deser_value::Value::from(r.version.clone()));
    m.insert("integrity".to_string(), deser_value::Value::from(r.integrity.clone()));
    m.insert("publishedAt".to_string(), deser_value::Value::from(r.published_at.clone()));
    m.insert("platform".to_string(), platform_to_value(&r.platform));
    deser_value::Value::from(m)
}

pub fn serialize_inventory(inv: &Inventory) -> Result<String> {
    let value = inventory_to_value(inv)?;
    let _reparsed: Inventory = parse_inventory(&deser_json::to_string(&value).map_err(|e| Error::Operation(e.to_string()))?)?;
    canonical_json(&value)
}

pub fn canonical_json(value: &deser_value::Value) -> Result<String> {
    Ok(format!("{}\n", render_pretty(value)))
}

pub fn render_pretty(value: &deser_value::Value) -> String {
    let mut owned = value.clone();
    sort_canonical_value(&mut owned);
    let mut out = String::new();
    write_pretty(&owned, &mut out, 0);
    out
}

fn write_pretty(v: &deser_value::Value, out: &mut String, indent: usize) {
    let pad = "  ".repeat(indent);
    let pad_in = "  ".repeat(indent + 1);
    match &**v {
        deser_value::Kind::Null => out.push_str("null"),
        deser_value::Kind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        deser_value::Kind::U64(n) => out.push_str(&n.to_string()),
        deser_value::Kind::I64(n) => out.push_str(&n.to_string()),
        deser_value::Kind::F64(f) => out.push_str(&format!("{f:?}")),
        deser_value::Kind::Str(s) | deser_value::Kind::Lexical(s) => {
            out.push_str(&deser_json::to_string(v).unwrap_or_else(|_| format!("{s:?}")));
        }
        _ if v.as_seq().is_some() => {
            let seq = v.as_seq().expect("seq");
            if seq.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, item) in seq.iter().enumerate() {
                out.push_str(&pad_in);
                write_pretty(item, out, indent + 1);
                if i + 1 < seq.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
        }
        _ if v.as_map().is_some() => {
            let map = v.as_map().expect("map");
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for i in 0..map.len() {
                let Some((k, item)) = map.get_index(i) else { continue };
                out.push_str(&pad_in);
                let key = k.as_str().unwrap_or("");
                out.push_str(&deser_json::to_string(&deser_value::Value::from(key.to_string())).unwrap_or_else(|_| format!("{key:?}")));
                out.push_str(": ");
                write_pretty(item, out, indent + 1);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
        _ => out.push_str(&deser_json::to_string(v).unwrap_or("null".to_string())),
    }
}

pub fn adopt_mirror_tarballs(project: &mut ProjectRecord, public_base_url: &str) {
    for record in &mut project.packages {
        let mirrored = record.source_kind == SourceKind::ExternalUrl
            && record
                .resolved
                .as_deref()
                .is_some_and(|url| is_mirror_tarball(url, record.integrity.as_deref(), public_base_url));
        if mirrored {
            record.source_kind = SourceKind::Registry;
            record.resolved = None;
        }
    }
}

fn is_mirror_tarball(url: &str, integrity: Option<&str>, public_base_url: &str) -> bool {
    let Some(sha512) = integrity
        .and_then(|text| crate::value::check_integrity_string(text, "integrity").ok())
        .and_then(|claims| claims.get("sha512").cloned())
    else {
        return false;
    };
    let Some(path) = url.strip_prefix(public_base_url.trim_end_matches('/')) else {
        return false;
    };
    let Some(mount) = path.strip_suffix(&format!("/-/tarballs/{sha512}.tgz")) else {
        return false;
    };
    match mount.strip_prefix('/') {
        None => mount.is_empty(),
        Some(segment) => {
            !segment.is_empty()
                && segment.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }
    }
}

pub fn merge_project(inv: &Inventory, project: ProjectRecord) -> Result<Inventory> {
    let reparsed = parse_inventory(&inventory_roundtrip(inv)?)?;
    let checked = normalize_project(&project_to_value(&project)?, "imported project", Category::Input)?;
    let existing: Vec<String> = reparsed.projects.iter().map(project_stable_string).collect::<Result<Vec<_>>>()?;
    if existing.contains(&project_stable_string(&checked)?) {
        return Ok(reparsed);
    }
    let mut projects = reparsed.projects.clone();
    projects.push(checked);
    validate_digest_conflicts(&projects, &reparsed.artifacts, Category::Conflict)?;
    validate_metadata_only(&projects, &reparsed.artifacts, &reparsed.metadata_only, Category::Conflict)?;
    parse_inventory(&inventory_roundtrip(&Inventory {
        projects,
        artifacts: reparsed.artifacts,
        metadata_only: reparsed.metadata_only,
    })?)
}

pub fn record_verified_artifacts(inv: &Inventory, values: &[deser_value::Value]) -> Result<Inventory> {
    let reparsed = parse_inventory(&inventory_roundtrip(inv)?)?;
    let mut artifacts = reparsed.artifacts;
    let mut canonical_by_identity: std::collections::HashMap<String, String> = std::collections::HashMap::with_capacity(artifacts.len() + values.len());
    for artifact in &artifacts {
        canonical_by_identity.insert(format!("{}\0{}", artifact.name, artifact.version), canonical_string(&artifact_to_value(artifact)?)?);
    }
    for value in values {
        let checked = normalize_artifact_record(value, "verified artifact", Category::Input)?;
        let identity = format!("{}\0{}", checked.name, checked.version);
        let canonical = canonical_string(&artifact_to_value(&checked)?)?;
        match canonical_by_identity.get(&identity) {
            Some(prev) if *prev != canonical => {
                return Err(Error::Conflict(format!("verified artifact conflicts for {}@{}", checked.name, checked.version)));
            }
            Some(_) => {}
            None => {
                canonical_by_identity.insert(identity, canonical);
                artifacts.push(checked);
            }
        }
    }
    validate_digest_conflicts(&reparsed.projects, &artifacts, Category::Conflict)?;
    let mut metadata_only = reparsed.metadata_only;
    for artifact in &artifacts {
        if let Some(i) = metadata_only.iter().position(|r| r.name == artifact.name && r.version == artifact.version) {
            if metadata_only[i].integrity != artifact.artifact.integrity {
                return Err(Error::Conflict(format!(
                    "hosted artifact conflicts with metadata-only record for {}@{}",
                    artifact.name, artifact.version
                )));
            }
            metadata_only.remove(i);
        }
    }
    parse_inventory(&inventory_roundtrip(&Inventory { projects: reparsed.projects, artifacts, metadata_only })?)
}

pub fn record_metadata_only(inv: &Inventory, records: &[MetadataOnlyRecord]) -> Result<Inventory> {
    let reparsed = parse_inventory(&inventory_roundtrip(inv)?)?;
    let mut metadata_only = reparsed.metadata_only;
    for record in records {
        let checked = normalize_metadata_only_record(&metadata_only_to_value(record), "metadata-only record", Category::Input)?;
        match metadata_only.iter().find(|r| r.name == checked.name && r.version == checked.version) {
            Some(existing) if *existing != checked => {
                return Err(Error::Conflict(format!(
                    "metadata-only record conflicts for {}@{}",
                    checked.name, checked.version
                )));
            }
            Some(_) => {}
            None => metadata_only.push(checked),
        }
    }
    validate_metadata_only(&reparsed.projects, &reparsed.artifacts, &metadata_only, Category::Conflict)?;
    parse_inventory(&inventory_roundtrip(&Inventory {
        projects: reparsed.projects,
        artifacts: reparsed.artifacts,
        metadata_only,
    })?)
}

fn inventory_roundtrip(inv: &Inventory) -> Result<String> {
    let value = inventory_to_value(inv)?;
    deser_json::to_string(&value).map_err(|e| Error::Corrupt(format!("inventory is not JSON serializable: {e}")))
}

pub fn package_identities(inv: &Inventory) -> Vec<(String, String, Vec<String>, Vec<String>)> {
    let mut out: BTreeMap<(String, String), (Vec<SourceKind>, Vec<String>)> = BTreeMap::new();
    for p in &inv.projects {
        for pkg in &p.packages {
            if let Some(ref ver) = pkg.version {
                let entry = out.entry((pkg.name.clone(), ver.clone())).or_default();
                entry.0.push(pkg.source_kind.clone());
                entry.1.extend(pkg.targets.iter().cloned());
            }
        }
    }
    out.into_iter()
        .map(|((n, v), (kinds, mut targets))| {
            let mut kinds: Vec<String> = kinds.into_iter().map(|k| k.as_str().to_string()).collect();
            kinds.sort();
            kinds.dedup();
            targets.sort();
            targets.dedup();
            (
                n,
                v,
                kinds,
                targets,
            )
        })
        .collect()
}
