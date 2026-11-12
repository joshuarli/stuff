#![forbid(unsafe_code)]

use crate::catalog::{Catalog, PackageEntry};
use crate::config::ProjectSettings;
use crate::error::{Error, Kind, Result, Stage};
use crate::models::{PackageVersion, Target, ValidatedArtifact, WheelIdentity};
use crate::planner::validate_catalog_targets;
use crate::platforms::ordered_target_tags;
use crate::validate::{ExceptionKind, ValidationException, ValidationPolicy};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

pub struct PublishOptions<'a> {
    pub settings: &'a ProjectSettings,
    pub output: &'a Path,
    pub state_dir: &'a Path,
    pub catalog: &'a Catalog,
    pub platforms: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishSummary {
    pub generation: PathBuf,
    pub generation_id: String,
    pub new_wheels: usize,
    pub no_imports: usize,
    pub no_op: bool,
}

pub struct RepublishOptions<'a> {
    pub output: &'a Path,
    pub public_url: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepublishSummary {
    pub generation: PathBuf,
    pub generation_id: String,
    pub no_op: bool,
}

pub fn publish(options: &PublishOptions<'_>) -> Result<PublishSummary> {
    let first_party: FxHashSet<String> = crate::store::published_artifacts(options.output)
        .map_err(|e| Error::validation(e.detail.clone(), Stage::Validation))?
        .into_iter()
        .filter(|artifact| artifact.first_party)
        .map(|artifact| artifact.wheel.package.name)
        .collect();
    if let Some(entry) = options.catalog.entries.iter().find(|entry| first_party.contains(&entry.package.name)) {
        return Err(Error::conflict(
            format!(
                "{} is not a first-party version; {} is a first-party package name",
                entry.package, entry.package.name
            ),
            Stage::Publication,
        )
        .with_package(&entry.package));
    }
    let set = collect_validated(options.settings, options.output, options.state_dir, options.platforms, options.catalog)?;
    let entries = options.catalog.by_package();
    let mut yanked: FxHashMap<String, Option<String>> = FxHashMap::default();
    for artifact in crate::store::published_artifacts(options.output).map_err(|e| {
        Error::new(Kind::Integrity, e.detail.clone(), Stage::Publication)
    })? {
        if let Some(entry) = entries.get(&artifact.wheel.package) {
            if entry.clear_yanked {
                yanked.insert(artifact.wheel.filename.clone(), None);
            } else if let Some(reason) = &entry.yanked_reason {
                yanked.insert(artifact.wheel.filename.clone(), Some(reason.clone()));
            }
        }
    }
    for artifact in &set.artifacts {
        if let Some(entry) = entries.get(&artifact.wheel.package) {
            if entry.clear_yanked {
                yanked.insert(artifact.wheel.filename.clone(), None);
            } else if let Some(reason) = &entry.yanked_reason {
                yanked.insert(artifact.wheel.filename.clone(), Some(reason.clone()));
            }
        }
    }
    let (current, activation) = crate::store::publish(&set.artifacts, options.output, &yanked, &options.settings.public_base_url)?;
    Ok(PublishSummary {
        no_op: activation.is_noop,
        generation: current,
        generation_id: activation.generation_id,
        new_wheels: set.artifacts.len(),
        no_imports: set.no_imports,
    })
}

pub struct FirstPartyOptions<'a> {
    pub settings: &'a ProjectSettings,
    pub output: &'a Path,
    pub artifacts: &'a [ValidatedArtifact],
}

pub fn first_party_artifacts(settings: &ProjectSettings, wheels: &[PathBuf]) -> Result<Vec<ValidatedArtifact>> {
    let targets = settings.targets();
    let mut artifacts: Vec<ValidatedArtifact> = Vec::new();
    for path in wheels {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !std::fs::metadata(path).is_ok_and(|md| md.is_file()) {
            return Err(fail(format!("wheel is not a readable file: {}", path.display())));
        }
        let identity = WheelIdentity::parse(name)
            .map_err(|e| fail(format!("invalid wheel filename {name}: {}", e.detail)))?;
        let pure = crate::platforms::wheel_tag_triples(name)?
            .iter()
            .all(|triple| triple.abi == "none" && triple.platform == "any");
        if !pure {
            return Err(fail(format!(
                "{name} is not a pure-Python none-any wheel; first-party publication supports only none-any wheels"
            ))
            .with_package(&identity.package));
        }
        for target in &targets {
            let inspected = crate::validate::inspect_wheel(path, target, &settings.linux_repair_policy)?;
            if !inspected.native_architectures.is_empty() {
                return Err(fail(format!("{name} contains native code; first-party publication supports only pure-Python wheels"))
                    .with_package(&identity.package));
            }
        }
        let sha256 = sha256_of(path)?;
        if let Some(previous) = artifacts.iter().find(|other| {
            other.wheel.package == identity.package
        }) {
            if previous.wheel.filename == identity.filename && previous.sha256 == sha256 {
                continue;
            }
            let detail = if previous.wheel.filename == identity.filename {
                format!("{name} is given twice with different bytes")
            } else {
                format!("{} is given as both {} and {name}; first-party versions are immutable", identity.package, previous.wheel.filename)
            };
            return Err(Error::conflict(detail, Stage::Publication).with_package(&identity.package));
        }
        artifacts.push(ValidatedArtifact::new(identity, path.clone(), sha256, targets.clone())?.into_first_party());
    }
    Ok(artifacts)
}

pub fn publish_first_party(options: &FirstPartyOptions<'_>) -> Result<PublishSummary> {
    let (current, activation) = crate::store::publish(
        options.artifacts,
        options.output,
        &FxHashMap::default(),
        &options.settings.public_base_url,
    )?;
    Ok(PublishSummary {
        no_op: activation.is_noop,
        generation: current,
        generation_id: activation.generation_id,
        new_wheels: options.artifacts.len(),
        no_imports: 0,
    })
}

pub fn republish(options: &RepublishOptions<'_>) -> Result<RepublishSummary> {
    let (current, activation) = crate::store::republish(options.output, options.public_url)?;
    Ok(RepublishSummary {
        no_op: activation.is_noop,
        generation: current,
        generation_id: activation.generation_id,
    })
}

fn fail(detail: impl Into<String>) -> Error {
    Error::new(Kind::InvalidInput, detail, Stage::Command)
}

pub(crate) fn selected_targets(settings: &ProjectSettings, requested: &[String]) -> Result<Vec<Target>> {
    if requested.is_empty() {
        return Ok(settings.targets());
    }
    {
        let mut seen = FxHashSet::default();
        for p in requested {
            if !seen.insert(p.clone()) {
                return Err(fail("--platform values must be unique"));
            }
        }
    }
    let configured: FxHashMap<&str, &Target> = FxHashMap::default();
    let targets = settings.targets();
    let mut map: FxHashMap<&str, Target> = FxHashMap::default();
    for t in &targets {
        map.insert(t.platform.as_str(), t.clone());
    }
    let _ = configured;
    let mut out = Vec::new();
    for name in requested {
        match map.get(name.as_str()) {
            Some(t) => out.push(t.clone()),
            None => return Err(fail(format!("unconfigured platform: {name}"))),
        }
    }
    Ok(out)
}

pub(crate) fn real_directory(path: &Path, create: bool) -> Result<bool> {
    let md = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(false);
            }
            std::fs::create_dir_all(path).map_err(|_| {
                Error::io(format!("could not create directory {}", path.display()), Stage::Command)
            })?;
            std::fs::symlink_metadata(path).map_err(|_| {
                Error::io(format!("could not inspect directory {}", path.display()), Stage::Command)
            })?
        }
        Err(_) => {
            return Err(Error::io(format!("could not inspect directory {}", path.display()), Stage::Command));
        }
    };
    if md.file_type().is_symlink() || !md.file_type().is_dir() {
        return Err(Error::invalid_input(format!("path must be a real directory: {}", path.display()), Stage::Command));
    }
    Ok(true)
}

pub(crate) fn wheel_paths(state_dir: &Path) -> Result<Vec<PathBuf>> {
    if !real_directory(state_dir, false)? {
        return Ok(Vec::new());
    }
    let wheel_dir = state_dir.join("wheels");
    if !real_directory(&wheel_dir, false)? {
        return Ok(Vec::new());
    }
    let mut entries: Vec<_> = std::fs::read_dir(&wheel_dir)
        .map_err(|_| Error::io(format!("could not inspect wheel {}", wheel_dir.display()), Stage::Command))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("whl"))
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in &entries {
        let md = std::fs::symlink_metadata(path).map_err(|_| {
            Error::io(format!("could not inspect wheel {}", path.display()), Stage::Command)
        })?;
        if !md.file_type().is_file() {
            return Err(Error::invalid_input(
                format!("wheel path must be a regular file: {}", path.display()),
                Stage::Command,
            ));
        }
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if WheelIdentity::parse(name).is_err() {
            return Err(Error::integrity(format!("invalid wheel filename {name}"), Stage::Command));
        }
    }
    Ok(entries)
}

pub(crate) fn sha256_of(path: &Path) -> Result<String> {
    crate::acquire::sha256_file(path).map_err(|_| {
        Error::io(format!("could not read wheel {}", path.display()), Stage::Command)
    })
}

fn validation_policy(entry: &PackageEntry) -> Result<ValidationPolicy> {
    let extras: Vec<(String, String)> = entry.validation.validate_extras.clone();
    if extras.len() > 1 {
        return Err(Error::configuration(format!("{} may configure one validation extra set", entry.package))
        .with_package(&entry.package));
    }
    let mut exceptions = Vec::new();
    if let Some((value, reason)) = extras.first() {
        exceptions.push(ValidationException::new(ExceptionKind::Extras, value, reason)?);
    }
    for (value, reason) in &entry.validation.validate_incorrect_missing_deps {
        exceptions.push(ValidationException::new(ExceptionKind::MissingDependency, value, reason)?);
    }
    for (value, reason) in &entry.validation.validate_skip_imports {
        exceptions.push(ValidationException::new(ExceptionKind::SkipImport, value, reason)?);
    }
    ValidationPolicy::new(exceptions)
}

pub(crate) struct TempScope {
    pub path: PathBuf,
}

impl TempScope {
    pub fn create(prefix: &str) -> Result<Self> {
        for counter in 0..100u32 {
            let name = format!("{prefix}{}-{}-{counter}", std::process::id(), rand_hex());
            let path = std::env::temp_dir().join(name);
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(Error::io(format!("could not create temporary directory: {e}"), Stage::Validation));
                }
            }
        }
        Err(Error::io("could not create temporary directory", Stage::Validation))
    }
}

impl Drop for TempScope {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn rand_hex() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (nanos as u64) ^ ((std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15))
}

pub(crate) struct ValidatedSet {
    pub artifacts: Vec<ValidatedArtifact>,
    pub no_imports: usize,
    pub _scope: TempScope,
}

pub(crate) fn collect_validated(
    settings: &ProjectSettings,
    output: &Path,
    state_dir: &Path,
    requested_platforms: &[String],
    catalog: &Catalog,
) -> Result<ValidatedSet> {
    let targets = selected_targets(settings, requested_platforms)?;
    validate_catalog_targets(catalog, &settings.targets())?;
    let entries = catalog.by_package();
    let published = crate::store::published_artifacts(output).map_err(|e| {
        Error::validation(e.detail.clone(), Stage::Validation)
    })?;
    let published_by_filename: FxHashMap<&str, &ValidatedArtifact> =
        published.iter().map(|a| (a.wheel.filename.as_str(), a)).collect();
    let mut staged: FxHashMap<String, (WheelIdentity, PathBuf, String, Vec<Target>)> = FxHashMap::default();
    for path in wheel_paths(state_dir)? {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let identity = WheelIdentity::parse(&name).map_err(|e| {
            Error::integrity(format!("invalid wheel filename {name}: {}", e.detail), Stage::Validation)
        })?;
        let entry = entries.get(&identity.package).ok_or_else(|| {
            Error::invalid_input(
                format!("wheel is not listed at this version in the catalog: {}", identity.package),
                Stage::Validation,
            )
            .with_package(&identity.package)
        })?;
        let mut applicable = Vec::new();
        for target in &targets {
            if let Some(platforms) = entry.platforms.as_ref()
                && !platforms.contains(&target.platform)
            {
                continue;
            }
            if entry.excluded_platforms.iter().any(|(p, _)| p == &target.platform) {
                continue;
            }
            let wheel_tags: FxHashSet<String> = crate::platforms::wheel_tag_triples(&name)
                .map_err(|e| Error::integrity(format!("invalid wheel {name}: {}", e.detail), Stage::Validation))?
                .iter()
                .map(|t| t.as_string())
                .collect();
            let supported: FxHashSet<String> = ordered_target_tags(target, &settings.linux_repair_policy)
                .map_err(|e| Error::integrity(e.detail, Stage::Validation))?
                .iter()
                .map(|t| t.as_string())
                .collect();
            if wheel_tags.intersection(&supported).next().is_none() {
                continue;
            }
            applicable.push(target.clone());
        }
        if applicable.is_empty() {
            continue;
        }
        let checksum = sha256_of(&path)?;
        if let Some(existing) = published_by_filename.get(name.as_str())
            && existing.sha256 != checksum {
                return Err(Error::conflict(
                    format!("wheel filename differs from published bytes: {name}"),
                    Stage::Validation,
                )
                .with_package(&identity.package));
            }
        staged.insert(name.clone(), (identity, path, checksum, applicable));
    }
    let mut obligations: Vec<(&PackageEntry, &Target)> = Vec::new();
    for entry in &catalog.entries {
        for t in &targets {
            let in_scope = entry.platforms.is_none()
                || entry.platforms.as_ref().is_some_and(|p| p.contains(&t.platform));
            if in_scope && !entry.excluded_platforms.iter().any(|(p, _)| p == &t.platform) {
                obligations.push((entry, t));
            }
        }
    }
    for (entry, target) in &obligations {
        let covered = published.iter().any(|a| a.wheel.package == entry.package && a.targets.contains(target));
        let staged_candidate = staged.values().any(|(id, _, _, applicable)| id.package == entry.package && applicable.contains(target));
        if !covered && !staged_candidate {
            return Err(Error::missing_artifact(
                format!("catalog package has no candidate wheel for {}", target.platform),
                Stage::Validation,
            )
            .with_package(&entry.package)
            .with_target(&target.python_version.to_string(), &target.platform));
        }
    }
    let mut targets_to_validate: FxHashMap<String, Vec<Target>> = FxHashMap::default();
    for (filename, (_, _, _, applicable)) in &staged {
        let mut needed = Vec::new();
        for target in applicable {
            let covered = published_by_filename
                .get(filename.as_str())
                .is_some_and(|a| a.targets.contains(target));
            if !covered {
                needed.push(target.clone());
            }
        }
        if !needed.is_empty() {
            targets_to_validate.insert(filename.clone(), needed);
        }
    }
    let local = crate::validate::local_platform();
    let mut policies: FxHashMap<PackageVersion, ValidationPolicy> = FxHashMap::default();
    let mut entry_pairs: Vec<_> = entries.iter().collect();
    entry_pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (package, entry) in entry_pairs {
        policies.insert((*package).clone(), validation_policy(entry)?);
    }
    let scope = TempScope::create("mypypi-candidate-")?;
    let snapshot_root = scope.path.clone();
    let mut validated_targets: FxHashMap<String, Vec<Target>> = FxHashMap::default();
    let mut copied_paths: FxHashMap<String, PathBuf> = FxHashMap::default();
    let mut no_imports = 0usize;
    for target in &targets {
        let mut candidate_sources: FxHashMap<String, (PathBuf, String)> = FxHashMap::default();
        for artifact in &published {
            if artifact.targets.contains(target) {
                candidate_sources.insert(artifact.wheel.filename.clone(), (artifact.path.clone(), artifact.sha256.clone()));
            }
        }
        for (filename, (_, path, checksum, applicable)) in &staged {
            if !applicable.contains(target) {
                continue;
            }
            if published_by_filename.get(filename.as_str()).is_some_and(|a| a.targets.contains(target)) {
                continue;
            }
            candidate_sources.insert(filename.clone(), (path.clone(), checksum.clone()));
        }
        if candidate_sources.is_empty() {
            continue;
        }
        let wheel_set = snapshot_root.join(&target.platform);
        std::fs::create_dir(&wheel_set).map_err(|e| {
            Error::io(format!("could not create validation snapshot: {e}"), Stage::Validation)
        })?;
        let mut target_paths: FxHashMap<String, PathBuf> = FxHashMap::default();
        let mut sources: Vec<_> = candidate_sources.iter().collect();
        sources.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (filename, (source, expected_hash)) in sources {
            let wheel_path = wheel_set.join(filename);
            std::fs::copy(source, &wheel_path).map_err(|e| {
                Error::io(format!("could not copy validation snapshot: {e}"), Stage::Validation)
            })?;
            if sha256_of(&wheel_path)? != *expected_hash {
                return Err(Error::integrity(
                    format!("wheel changed while making candidate snapshot: {filename}"),
                    Stage::Validation,
                ));
            }
            target_paths.insert(filename.clone(), wheel_path.clone());
            copied_paths.entry(filename.clone()).or_insert(wheel_path);
        }
        let native = local.as_deref() == Some(target.platform.as_str());
        let mut pending = Vec::new();
        let mut validation_order: Vec<_> = targets_to_validate.iter().collect();
        validation_order.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (filename, required) in validation_order {
            if !required.contains(target) {
                continue;
            }
            let wheel_path = target_paths.get(filename).ok_or_else(|| {
                Error::integrity(format!("candidate wheel is absent from target snapshot: {filename}"), Stage::Validation)
            })?;
            let meta = crate::validate::inspect_wheel(wheel_path, target, &settings.linux_repair_policy)?;
            if native {
                pending.push(wheel_path.clone());
            } else {
                validated_targets.entry(meta.identity.filename.clone()).or_default().push(target.clone());
                if meta.imports.is_empty() {
                    no_imports += 1;
                }
            }
        }
        if !pending.is_empty() {
            let metadata = crate::validate::validate_wheels(&pending, std::slice::from_ref(target), &wheel_set, &policies, &settings.linux_repair_policy)?;
            for (item, t) in metadata {
                validated_targets.entry(item.identity.filename.clone()).or_default().push(t);
                if item.imports.is_empty() {
                    no_imports += 1;
                }
            }
        }
        let mut copied: Vec<_> = target_paths.iter().collect();
        copied.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (filename, wheel_path) in copied {
            if sha256_of(wheel_path)? != candidate_sources[filename].1 {
                return Err(Error::integrity(
                    format!("candidate wheel changed during validation: {filename}"),
                    Stage::Validation,
                ));
            }
        }
    }
    let mut new_artifacts = Vec::new();
    let mut staged_names: Vec<String> = staged.keys().cloned().collect();
    staged_names.sort();
    for filename in staged_names {
        let (identity, _, checksum, _) = &staged[&filename];
        if let Some(covered) = validated_targets.get(&filename) {
            let path = copied_paths[&filename].clone();
            new_artifacts.push(ValidatedArtifact::new(identity.clone(), path, checksum.clone(), covered.clone())?);
        }
    }
    let mut coverage: FxHashSet<(PackageVersion, Target)> = FxHashSet::default();
    for artifact in &published {
        for target in &artifact.targets {
            coverage.insert((artifact.wheel.package.clone(), target.clone()));
        }
    }
    for (filename, covered) in &validated_targets {
        let identity = WheelIdentity::parse(filename).expect("staged filename parses");
        for target in covered {
            coverage.insert((identity.package.clone(), target.clone()));
        }
    }
    for (entry, target) in &obligations {
        if !coverage.contains(&(entry.package.clone(), (*target).clone())) {
            return Err(Error::missing_artifact(
                format!("catalog package has no validated wheel for {}", target.platform),
                Stage::Validation,
            )
            .with_package(&entry.package)
            .with_target(&target.python_version.to_string(), &target.platform));
        }
    }
    Ok(ValidatedSet { artifacts: new_artifacts, no_imports, _scope: scope })
}
