#![forbid(unsafe_code)]

use rustc_hash::FxHashSet;
use std::path::{Path, PathBuf};

use deser_value::Value;
use mynpm::acquire::{fetch_inventory, Exchange};
use mynpm::first_party::{first_party_artifact, FirstPartyTimes};
use mynpm::inventory::{
    artifact_to_value, merge_project, normalize_metadata_only_record, record_metadata_only,
    record_verified_artifacts, validate_artifact_record, ArtifactRecord, Category, Inventory,
    MetadataOnlyRecord, SourceKind,
};
use mynpm::inventory_store::{load_inventory, save_inventory};
use mynpm::lock_npm::{parse_npm_lock, NpmOptions};
use mynpm::lock_pnpm::{parse_pnpm_lock, PnpmOptions};

use crate::publish::r2::parallel::{REQUEST_CONCURRENCY, map_bounded};
use crate::publish::r2::pull::{is_generation_id, safe_relative};
use crate::publish::snapshot::{SharedStore, Snapshot};
use crate::publish::PublishError;

pub fn library_error(error: mynpm::error::Error) -> PublishError {
    use mynpm::error::Error as LibraryError;
    match error {
        LibraryError::Usage(message) | LibraryError::Input(message) => PublishError::Usage(message),
        LibraryError::Operation(message) | LibraryError::Corrupt(message) | LibraryError::Limit(message) => {
            PublishError::Integrity(message)
        }
        LibraryError::Conflict(message) => PublishError::Conflict(message),
        LibraryError::Network(message) => PublishError::Transport(message),
    }
}

#[derive(Debug)]
struct GenerationManifest {
    artifacts: Vec<ArtifactRecord>,
    metadata_only: Vec<MetadataOnlyRecord>,
}

fn parse_generation_manifest(text: &str, generation: &str) -> Result<GenerationManifest, PublishError> {
    let value: Value = deser_json::from_str(text).map_err(|error| {
        PublishError::Integrity(format!("generation {generation} manifest is not valid JSON: {error}"))
    })?;
    let record = value.as_map().ok_or_else(|| {
        PublishError::Integrity(format!("generation {generation} manifest must be an object"))
    })?;
    let artifacts_value = record.get("artifacts").and_then(|value| value.as_seq()).ok_or_else(|| {
        PublishError::Integrity(format!("generation {generation} manifest has no artifact list"))
    })?;
    let mut artifacts = Vec::with_capacity(artifacts_value.len());
    for item in artifacts_value.iter() {
        artifacts.push(validate_artifact_record(item).map_err(library_error)?);
    }
    artifacts.sort_by_key(|artifact| (artifact.name.clone(), artifact.version.clone()));
    let mut metadata_only = Vec::new();
    if let Some(stubs) = record.get("metadataOnly").and_then(|value| value.as_seq()) {
        for item in stubs.iter() {
            metadata_only.push(
                normalize_metadata_only_record(item, "generation metadataOnly", Category::Corrupt)
                    .map_err(library_error)?,
            );
        }
        metadata_only.sort_by_key(|record| (record.name.clone(), record.version.clone()));
    }
    Ok(GenerationManifest { artifacts, metadata_only })
}

fn read_generation_manifest(mirror: &Path, generation: &str) -> Result<GenerationManifest, PublishError> {
    let path = mirror
        .join("generations")
        .join(generation)
        .join("manifest.json");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        PublishError::Integrity(format!(
            "generation {generation} has no readable manifest at {}: {error}",
            path.display()
        ))
    })?;
    parse_generation_manifest(&text, generation)
}

pub fn pull_live_generation(
    snapshot: &Snapshot,
    bucket: &str,
    mirror: &Path,
) -> Result<Option<String>, PublishError> {
    let Some(_) = snapshot.metadata().get("current") else {
        return Ok(None);
    };
    let current = snapshot.fetch(bucket, "current")?;
    let generation = String::from_utf8_lossy(&current).trim().to_string();
    if !is_generation_id(&generation) {
        return Err(PublishError::Integrity(format!(
            "bad generation id in the current marker: {generation:?}"
        )));
    }
    let prefix = format!("generations/{generation}/");
    let mut keys: Vec<String> = snapshot
        .metadata()
        .keys()
        .filter(|key| key.starts_with(&prefix))
        .cloned()
        .collect();
    keys.sort();
    if keys.is_empty() {
        return Err(PublishError::Integrity(format!(
            "generation {generation} has no objects in {bucket}"
        )));
    }
    std::fs::create_dir_all(mirror).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", mirror.display()))
    })?;
    map_bounded(&keys, REQUEST_CONCURRENCY, |key| {
        let bytes = snapshot.fetch(bucket, key)?;
        let path = mirror.join(safe_relative(key)?);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                PublishError::Config(format!("could not create {}: {error}", parent.display()))
            })?;
        }
        std::fs::write(&path, &bytes).map_err(|error| {
            PublishError::Config(format!("could not write {}: {error}", path.display()))
        })
    })?;
    let manifest_key = format!("manifests/{generation}.json");
    if let Some(object) = snapshot.metadata().get(&manifest_key) {
        let bytes = snapshot.fetch(bucket, &object.key)?;
        let path = mirror.join(safe_relative(&manifest_key)?);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                PublishError::Config(format!("could not create {}: {error}", parent.display()))
            })?;
        }
        std::fs::write(&path, &bytes).map_err(|error| {
            PublishError::Config(format!("could not write {}: {error}", path.display()))
        })?;
    }
    let current_link = mirror.join("current");
    if current_link.symlink_metadata().is_ok() {
        std::fs::remove_file(&current_link).map_err(|error| {
            PublishError::Config(format!("could not replace {}: {error}", current_link.display()))
        })?;
    }
    std::os::unix::fs::symlink(format!("generations/{generation}"), &current_link).map_err(|error| {
        PublishError::Config(format!("could not link {}: {error}", current_link.display()))
    })?;
    if !mirror
        .join("generations")
        .join(&generation)
        .join("manifest.json")
        .is_file()
    {
        return Err(PublishError::Integrity(format!(
            "generation {generation} has no manifest object"
        )));
    }
    Ok(Some(generation))
}

pub fn import_lockfile(
    inventory: Inventory,
    lockfile: &Path,
    project_id: &str,
    library: &mynpm::config::Config,
) -> Result<Inventory, PublishError> {
    let text = std::fs::read_to_string(lockfile).map_err(|error| {
        PublishError::Usage(format!("could not read {}: {error}", lockfile.display()))
    })?;
    let name = lockfile
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| lockfile.to_string_lossy().into_owned());
    let targets: Vec<String> = library.targets.iter().map(|target| target.id().to_string()).collect();
    let extension = lockfile
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    let mut project = if extension == "yaml" || extension == "yml" {
        parse_pnpm_lock(
            &text,
            PnpmOptions {
                project_id: project_id.to_string(),
                package_manager_pin: None,
                lockfile_path: name,
                target_ids: targets,
                snapshot: None,
            },
        )
        .map_err(library_error)?
    } else {
        parse_npm_lock(
            &text,
            NpmOptions {
                project_id: project_id.to_string(),
                package_manager_pin: None,
                lockfile_path: name,
                target_ids: targets,
                snapshot: None,
            },
        )
        .map_err(library_error)?
    };
    mynpm::inventory::adopt_mirror_tarballs(&mut project, &library.public_base_url);
    merge_project(&inventory, project).map_err(library_error)
}

fn project_has_registry_package(inventory: &Inventory, name: &str, version: &str) -> bool {
    inventory.projects.iter().any(|project| {
        project.packages.iter().any(|package| {
            package.name == name
                && package.version.as_deref() == Some(version)
                && package.source_kind == SourceKind::Registry
        })
    })
}

fn link_or_copy(source: &Path, target: &Path) -> Result<(), PublishError> {
    match std::fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(_) => std::fs::copy(source, target).map(|_| ()).map_err(|error| {
            PublishError::Config(format!("could not publish {}: {error}", target.display()))
        }),
    }
}

fn ensure_objects(
    snapshot: &Snapshot,
    store: &SharedStore,
    state_dir: &Path,
    artifacts: impl IntoIterator<Item = ArtifactRecord>,
) -> Result<(), PublishError> {
    let objects = state_dir.join("objects").join("sha512");
    std::fs::create_dir_all(&objects).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", objects.display()))
    })?;
    let mut digests = FxHashSet::default();
    for artifact in artifacts {
        digests.insert(artifact.artifact.sha512);
    }
    let mut digests: Vec<_> = digests.into_iter().collect();
    digests.sort();
    for digest in digests {
        let object = objects.join(format!("{digest}.tgz"));
        if object.is_file() {
            continue;
        }
        let key = format!("{}{digest}.tgz", store.prefix);
        let bytes = snapshot.fetch_shared(store, &key)?;
        std::fs::write(&object, &bytes).map_err(|error| {
            PublishError::Config(format!("could not write {}: {error}", object.display()))
        })?;
    }
    Ok(())
}

fn materialize(
    snapshot: &Snapshot,
    store: &SharedStore,
    mirror: &Path,
    state_dir: &Path,
    generation: &str,
    retained: &[ArtifactRecord],
    incoming: &[ArtifactRecord],
) -> Result<(), PublishError> {
    let objects = state_dir.join("objects").join("sha512");
    std::fs::create_dir_all(&objects).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", objects.display()))
    })?;
    let tarballs = mirror.join("tarballs");
    std::fs::create_dir_all(&tarballs).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", tarballs.display()))
    })?;
    let mut digests = FxHashSet::default();
    for artifact in retained.iter().chain(incoming) {
        digests.insert(artifact.artifact.sha512.clone());
    }
    let mut live_digests = FxHashSet::default();
    for artifact in retained {
        live_digests.insert(artifact.artifact.sha512.clone());
    }
    let mut digests: Vec<_> = digests.into_iter().collect();
    digests.sort();
    for digest in digests {
        let object = objects.join(format!("{digest}.tgz"));
        if !object.is_file() {
            let key = format!("{}{digest}.tgz", store.prefix);
            let bytes = snapshot.fetch_shared(store, &key)?;
            std::fs::write(&object, &bytes).map_err(|error| {
                PublishError::Config(format!("could not write {}: {error}", object.display()))
            })?;
        }
        let shared = tarballs.join(format!("{digest}.tgz"));
        if shared.symlink_metadata().is_err() {
            link_or_copy(&object, &shared)?;
        }
        if !live_digests.contains(&digest) {
            continue;
        }
        for alias in ["tarballs", "-/tarballs"] {
            let directory = mirror.join("generations").join(generation).join(alias);
            std::fs::create_dir_all(&directory).map_err(|error| {
                PublishError::Config(format!("could not create {}: {error}", directory.display()))
            })?;
            let path = directory.join(format!("{digest}.tgz"));
            if path.symlink_metadata().is_err() {
                link_or_copy(&shared, &path)?;
            }
        }
    }
    Ok(())
}

fn refuse_first_party_name_clashes(
    inventory: &Inventory,
    project_id: &str,
    manifest: &GenerationManifest,
) -> Result<(), PublishError> {
    for project in inventory.projects.iter().filter(|project| project.id == project_id) {
        for package in &project.packages {
            if package.source_kind != SourceKind::Registry {
                continue;
            }
            let Some(version) = package.version.as_deref() else { continue };
            let first_party_name = manifest
                .artifacts
                .iter()
                .any(|artifact| artifact.artifact.first_party && artifact.name == package.name);
            let first_party_identity = manifest.artifacts.iter().any(|artifact| {
                artifact.artifact.first_party && artifact.name == package.name && artifact.version == version
            });
            if first_party_name && !first_party_identity {
                return Err(PublishError::Conflict(format!(
                    "{}@{version} is not a first-party version; {} is a first-party package name",
                    package.name, package.name
                )));
            }
        }
    }
    Ok(())
}

pub struct FirstPartyOptions<'a> {
    pub bucket: &'a str,
    pub store: &'a SharedStore,
    pub site_dir: &'a Path,
    pub state_dir: &'a Path,
    pub tarballs: &'a [PathBuf],
    pub library: &'a mynpm::config::Config,
    pub published_at: &'a str,
}

fn existing_creation_time(manifest: &GenerationManifest, name: &str) -> Option<String> {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.artifact.first_party && artifact.name == name)
        .filter_map(|artifact| {
            artifact
                .packument
                .as_map()?
                .get("time")?
                .as_map()?
                .get("created")?
                .as_str()
                .map(str::to_string)
        })
        .min()
}

fn plan_first_party<'a>(
    manifest: &GenerationManifest,
    options: &FirstPartyOptions<'a>,
) -> Result<Vec<(&'a Path, ArtifactRecord)>, PublishError> {
    let mut incoming: Vec<(&'a Path, ArtifactRecord)> = Vec::new();
    for tarball in options.tarballs {
        let now = FirstPartyTimes { created: options.published_at, published: options.published_at };
        let mut record =
            first_party_artifact(tarball, &options.library.public_base_url, &now).map_err(library_error)?;
        if let Some(created) = existing_creation_time(manifest, &record.name) {
            let times = FirstPartyTimes { created: &created, published: options.published_at };
            record = first_party_artifact(tarball, &options.library.public_base_url, &times)
                .map_err(library_error)?;
        }
        let identity = format!("{}@{}", record.name, record.version);
        if let Some(previous) = incoming
            .iter()
            .map(|(_, other)| other)
            .find(|other| other.name == record.name && other.version == record.version)
        {
            if previous.artifact.sha512 != record.artifact.sha512 {
                return Err(PublishError::Conflict(format!(
                    "{identity} is given twice with different bytes"
                )));
            }
            continue;
        }
        if manifest
            .artifacts
            .iter()
            .any(|live| live.name == record.name && !live.artifact.first_party)
            || manifest.metadata_only.iter().any(|live| live.name == record.name)
        {
            return Err(PublishError::Conflict(format!(
                "{} is hosted by the upstream mirror; a first-party package cannot reuse its name",
                record.name
            )));
        }
        if let Some(live) = manifest
            .artifacts
            .iter()
            .find(|live| live.name == record.name && live.version == record.version)
        {
            if live.artifact.sha512 != record.artifact.sha512 {
                return Err(PublishError::Conflict(format!(
                    "{identity} is already published with different bytes; versions are immutable"
                )));
            }
            continue;
        }
        incoming.push((tarball.as_path(), record));
    }
    Ok(incoming)
}

pub fn prepare_first_party(
    snapshot: &Snapshot,
    options: &FirstPartyOptions<'_>,
) -> Result<Vec<ArtifactRecord>, PublishError> {
    let site_dir = options.site_dir;
    if site_dir.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(site_dir).map_err(|error| {
            PublishError::Config(format!("could not clear {}: {error}", site_dir.display()))
        })?;
    }
    let mirror = site_dir.join("mirror");
    std::fs::create_dir_all(&mirror).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", mirror.display()))
    })?;
    let generation = pull_live_generation(snapshot, options.bucket, &mirror)?;
    let (manifest, generation_id) = match generation {
        Some(generation) => (read_generation_manifest(&mirror, &generation)?, generation),
        None => (
            GenerationManifest {
                artifacts: Vec::new(),
                metadata_only: Vec::new(),
            },
            String::new(),
        ),
    };
    let incoming = plan_first_party(&manifest, options)?;
    let records: Vec<ArtifactRecord> = incoming.iter().map(|(_, record)| record.clone()).collect();

    let objects = options.state_dir.join("objects").join("sha512");
    std::fs::create_dir_all(&objects).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", objects.display()))
    })?;
    for (tarball, record) in &incoming {
        let object = objects.join(format!("{}.tgz", record.artifact.sha512));
        if !object.is_file() {
            std::fs::copy(tarball, &object).map_err(|error| {
                PublishError::Config(format!("could not write {}: {error}", object.display()))
            })?;
        }
    }
    materialize(
        snapshot,
        options.store,
        &mirror,
        options.state_dir,
        &generation_id,
        &manifest.artifacts,
        &records,
    )?;
    Ok(records)
}

pub struct PrepareOptions<'a> {
    pub bucket: &'a str,
    pub store: &'a SharedStore,
    pub site_dir: &'a Path,
    pub state_dir: &'a Path,
    pub lockfile: &'a Path,
    pub project_id: &'a str,
    pub library: &'a mynpm::config::Config,
    pub exchange: &'a dyn Exchange,
}

pub fn prepare(snapshot: &Snapshot, options: &PrepareOptions<'_>) -> Result<(), PublishError> {
    let site_dir = options.site_dir;
    if site_dir.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(site_dir).map_err(|error| {
            PublishError::Config(format!("could not clear {}: {error}", site_dir.display()))
        })?;
    }
    let mirror = site_dir.join("mirror");
    std::fs::create_dir_all(&mirror).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", mirror.display()))
    })?;
    let generation = pull_live_generation(snapshot, options.bucket, &mirror)?;
    let (manifest, generation_id) = match generation {
        Some(generation) => (read_generation_manifest(&mirror, &generation)?, generation),
        None => (
            GenerationManifest {
                artifacts: Vec::new(),
                metadata_only: Vec::new(),
            },
            String::new(),
        ),
    };

    let state_dir = options.state_dir;
    let mut inventory = load_inventory(state_dir).map_err(library_error)?;
    inventory = import_lockfile(inventory, options.lockfile, options.project_id, options.library)?;

    refuse_first_party_name_clashes(&inventory, options.project_id, &manifest)?;
    let values: Vec<Value> = manifest
        .artifacts
        .iter()
        .filter(|artifact| project_has_registry_package(&inventory, &artifact.name, &artifact.version))
        .map(artifact_to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(library_error)?;
    if !values.is_empty() {
        inventory = record_verified_artifacts(&inventory, &values).map_err(library_error)?;
    }
    let metadata_only: Vec<MetadataOnlyRecord> = manifest
        .metadata_only
        .iter()
        .filter(|record| project_has_registry_package(&inventory, &record.name, &record.version))
        .cloned()
        .collect();
    if !metadata_only.is_empty() {
        inventory = record_metadata_only(&inventory, &metadata_only).map_err(library_error)?;
    }
    save_inventory(state_dir, &inventory).map_err(library_error)?;
    ensure_objects(snapshot, options.store, state_dir, inventory.artifacts.clone())?;
    fetch_inventory(state_dir, options.library, options.exchange).map_err(library_error)?;
    let inventory = load_inventory(state_dir).map_err(library_error)?;
    materialize(
        snapshot,
        options.store,
        &mirror,
        state_dir,
        &generation_id,
        &manifest.artifacts,
        &inventory.artifacts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use mynpm::inventory::{
        parse_inventory, serialize_inventory, LockfileVersion, Manager, PackageFlags,
        PackageRecord, PlatformConstraints, ProjectRecord,
    };
    use sha2::{Digest, Sha512};

    fn artifact_json(name: &str, version: &str, tarball: &[u8]) -> String {
        let digest = crate::publish::lower_hex(Sha512::digest(tarball));
        let raw: Vec<u8> = (0..digest.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&digest[index..index + 2], 16).unwrap())
            .collect();
        let integrity = base64::engine::general_purpose::STANDARD.encode(&raw);
        let source = format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz");
        format!(
            "{{\"name\":\"{name}\",\"version\":\"{version}\",\"packument\":{{\"name\":\"{name}\",\"dist-tags\":{{\"latest\":\"{version}\"}}}},\"metadata\":{{\"name\":\"{name}\",\"version\":\"{version}\",\"dist\":{{\"tarball\":\"{source}\"}}}},\"artifact\":{{\"sha512\":\"{digest}\",\"integrity\":\"sha512-{integrity}\",\"shasum\":null,\"sourceUrl\":\"{source}\",\"legacySha1Only\":false}}}}"
        )
    }

    fn manifest_text(artifacts: &[String], metadata_only: &str) -> String {
        format!(
            "{{\"schemaVersion\":1,\"rendererVersion\":7,\"publicBaseUrl\":\"https://npm.example.test/\",\"generationId\":\"{}\",\"artifacts\":[{}],\"metadataOnly\":[{}]}}",
            "0".repeat(64),
            artifacts.join(","),
            metadata_only
        )
    }

    #[test]
    fn the_generation_manifest_yields_validated_artifacts() {
        let text = manifest_text(&[artifact_json("flatdemo", "1.2.3", b"tarball")], "");
        let manifest = parse_generation_manifest(&text, "g1").unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(manifest.artifacts[0].name, "flatdemo");
        assert!(manifest.metadata_only.is_empty());
    }

    #[test]
    fn a_manifest_without_artifacts_is_corrupt() {
        let text = manifest_text(&[], "");
        assert!(parse_generation_manifest(&text, "g1").is_ok());
        let error = parse_generation_manifest("{}", "g1").unwrap_err();
        assert_eq!(error.exit_code(), 4);
    }

    fn package(name: &str, version: &str, integrity: &str) -> PackageRecord {
        PackageRecord {
            key: format!("{name}@{version}"),
            name: name.to_string(),
            version: Some(version.to_string()),
            integrity: Some(integrity.to_string()),
            resolved: None,
            source_kind: SourceKind::Registry,
            aliases: Vec::new(),
            flags: PackageFlags {
                dev: false,
                optional: false,
                peer: false,
                peer_optional: false,
                link: false,
            },
            platform: PlatformConstraints { os: None, cpu: None, libc: None },
            targets: vec!["mac-arm64".to_string()],
        }
    }

    fn project(packages: Vec<PackageRecord>) -> ProjectRecord {
        ProjectRecord {
            id: "demo".to_string(),
            manager: Manager::Pnpm,
            package_manager_pin: None,
            lockfile_path: "pnpm-lock.yaml".to_string(),
            lockfile_version: LockfileVersion::Pnpm("9.0".to_string()),
            targets: vec!["mac-arm64".to_string()],
            importers: vec![".".to_string()],
            roots: Vec::new(),
            packages,
            snapshot: None,
        }
    }

    #[test]
    fn inventory_schema_two_and_three_stay_readable() {
        let empty = Inventory { projects: Vec::new(), artifacts: Vec::new(), metadata_only: Vec::new() };
        let text = serialize_inventory(&empty).unwrap();
        assert!(text.contains("\"schemaVersion\": 2"), "{text}");
        assert!(parse_inventory(&text).unwrap().metadata_only.is_empty());

        let tarball = b"tarball";
        let digest = crate::publish::lower_hex(Sha512::digest(tarball));
        let raw: Vec<u8> = (0..digest.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&digest[index..index + 2], 16).unwrap())
            .collect();
        let integrity = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(&raw)
        );
        let inventory = Inventory {
            projects: vec![project(vec![package("native", "1.0.0", &integrity)])],
            artifacts: Vec::new(),
            metadata_only: vec![MetadataOnlyRecord {
                name: "native".to_string(),
                version: "1.0.0".to_string(),
                integrity,
                published_at: "2024-01-02T03:04:05.678Z".to_string(),
                platform: PlatformConstraints { os: None, cpu: None, libc: None },
            }],
        };
        let text = serialize_inventory(&inventory).unwrap();
        assert!(text.contains("\"schemaVersion\": 3"), "{text}");
        let reparsed = parse_inventory(&text).unwrap();
        assert_eq!(reparsed.metadata_only.len(), 1);
        assert_eq!(reparsed.projects.len(), 1);
    }
}
