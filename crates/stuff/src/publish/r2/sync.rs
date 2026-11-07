#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use md5::{Digest as _, Md5};

use crate::publish::PublishError;

use super::client::{ObjectMeta, Precondition, R2Client};
use super::manifest;
use super::multipart;
use super::pull::is_generation_id;
use super::put::{self, PutAction, UploadOptions};

pub const MANIFEST_DIRECTORY: &str = "manifests/";
pub const BLOB_DIRECTORY: &str = "blobs/sha256";
pub const BLOB_KEY_PREFIX: &str = "sha256/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub size: u64,
    pub etag: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalManifest {
    pub body: Vec<u8>,
    pub size: u64,
    pub etag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSite {
    pub generation: String,
    pub files: BTreeMap<String, LocalFile>,
    pub manifests: BTreeMap<String, LocalManifest>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    File(PathBuf),
    Memory(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub key: String,
    pub size: u64,
    pub action: PutAction,
    pub payload: Payload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePlan {
    pub generation: String,
    pub entries: Vec<PlanEntry>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub generation: String,
    pub uploaded: Vec<String>,
    pub multipart: Vec<String>,
    pub skipped: Vec<String>,
    pub uploaded_bytes: u64,
    pub aliases: Vec<String>,
}

impl SyncReport {
    pub fn uploaded_objects(&self) -> usize {
        self.uploaded.len()
    }

    pub fn skipped_objects(&self) -> usize {
        self.skipped.len()
    }
}

impl SitePlan {
    pub fn report(&self) -> SyncReport {
        let mut report = SyncReport {
            generation: self.generation.clone(),
            aliases: self.aliases.clone(),
            ..SyncReport::default()
        };
        for entry in &self.entries {
            match entry.action {
                PutAction::Skip => report.skipped.push(entry.key.clone()),
                PutAction::Put => {
                    report.uploaded.push(entry.key.clone());
                    report.uploaded_bytes += entry.size;
                }
                PutAction::Multipart => {
                    report.multipart.push(entry.key.clone());
                    report.uploaded.push(entry.key.clone());
                    report.uploaded_bytes += entry.size;
                }
                PutAction::Conflict => {}
            }
        }
        report
    }
}

pub fn active_generation(site: &Path) -> Result<String, PublishError> {
    let current = site.join("current");
    let target = std::fs::read_link(&current).map_err(|error| {
        PublishError::Usage(format!("{} is not a symlink: {error}", current.display()))
    })?;
    let text = target.to_string_lossy();
    let generation = text.strip_prefix("generations/").unwrap_or(&text);
    if !is_generation_id(generation) {
        return Err(PublishError::Usage(format!(
            "bad generation id: {generation}"
        )));
    }
    Ok(generation.to_string())
}

pub fn collect(
    site: &Path,
    options: &UploadOptions,
    excludes: &[&str],
) -> Result<LocalSite, PublishError> {
    collect_with_known(site, options, excludes, &BTreeMap::new())
}

pub fn collect_with_known(
    site: &Path,
    options: &UploadOptions,
    excludes: &[&str],
    known: &BTreeMap<String, ObjectMeta>,
) -> Result<LocalSite, PublishError> {
    let generation = active_generation(site)?;
    let mut files = BTreeMap::new();
    let mut aliases = Vec::new();
    walk(
        site,
        Path::new(""),
        options,
        excludes,
        &mut files,
        &mut aliases,
    )?;
    for (key, object) in known {
        if excludes.iter().any(|prefix| key.starts_with(prefix)) {
            continue;
        }
        files.entry(key.clone()).or_insert_with(|| LocalFile {
            size: object.size,
            etag: object.etag.clone(),
            path: PathBuf::new(),
        });
    }
    let mut manifests = BTreeMap::new();
    for directory in generations(site)? {
        let prefix = format!("generations/{directory}/");
        let keys: Vec<String> = files
            .keys()
            .filter(|key| key.starts_with(&prefix) || !key.starts_with("generations/"))
            .cloned()
            .collect();
        let built = manifest::build(site, &keys, options, known)?;
        let key = format!("{MANIFEST_DIRECTORY}{directory}.json");
        let body = manifest::render(&built).into_bytes();
        let (size, etag) = match built.objects.iter().find(|object| object.key == key) {
            Some(object) => (object.size, object.etag.clone()),
            None => (body.len() as u64, multipart::md5_hex(&body)),
        };
        manifests.insert(
            key,
            LocalManifest {
                body,
                size,
                etag,
            },
        );
    }
    Ok(LocalSite {
        generation,
        files,
        manifests,
        aliases,
    })
}

pub fn plan(
    local: &LocalSite,
    remote: &BTreeMap<String, ObjectMeta>,
    options: &UploadOptions,
) -> Result<SitePlan, PublishError> {
    let active_manifest = format!("{MANIFEST_DIRECTORY}{}.json", local.generation);
    let mut entries = Vec::with_capacity(local.files.len() + local.manifests.len());
    let mut conflicts = Vec::new();
    for (key, file) in &local.files {
        let mut action = put::plan_key(
            file.size,
            &file.etag,
            remote.get(key),
            options.single_put_max,
        );
        if action == PutAction::Conflict && options.repair {
            action = if file.size > options.single_put_max {
                PutAction::Multipart
            } else {
                PutAction::Put
            };
        }
        if action == PutAction::Conflict {
            conflicts.push(key.clone());
        }
        entries.push(PlanEntry {
            key: key.clone(),
            size: file.size,
            action,
            payload: Payload::File(file.path.clone()),
        });
    }
    for (key, manifest) in &local.manifests {
        let mut action = put::plan_key(
            manifest.size,
            &manifest.etag,
            remote.get(key),
            options.single_put_max,
        );
        if action == PutAction::Conflict {
            if key != &active_manifest {
                action = PutAction::Skip;
            } else if options.repair {
                action = if manifest.size > options.single_put_max {
                    PutAction::Multipart
                } else {
                    PutAction::Put
                };
            } else {
                conflicts.push(key.clone());
            }
        }
        entries.push(PlanEntry {
            key: key.clone(),
            size: manifest.size,
            action,
            payload: Payload::Memory(manifest.body.clone()),
        });
    }
    if !conflicts.is_empty() {
        return Err(PublishError::Conflict(format!(
            "refusing to replace immutable content: {}",
            conflicts.join(", ")
        )));
    }
    entries.sort_by(|left, right| left.key.cmp(&right.key));
    Ok(SitePlan {
        generation: local.generation.clone(),
        entries,
        aliases: local.aliases.clone(),
    })
}

pub fn plan_blobs(
    site: &Path,
    remote: &BTreeMap<String, ObjectMeta>,
    options: &UploadOptions,
) -> Result<SitePlan, PublishError> {
    let directory = site.join(BLOB_DIRECTORY);
    let mut entries = Vec::new();
    let mut conflicts = Vec::new();
    if directory.is_dir() {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&directory).map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", directory.display()))
        })? {
            let entry = entry.map_err(|error| {
                PublishError::Config(format!("could not read {}: {error}", directory.display()))
            })?;
            if !entry
                .file_type()
                .map_err(|error| {
                    PublishError::Config(format!(
                        "could not inspect {}: {error}",
                        entry.path().display()
                    ))
                })?
                .is_file()
            {
                continue;
            }
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            if name.len() != 64
                || !name
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(PublishError::Integrity(format!(
                    "blob name is not a sha256 hex: {name}"
                )));
            }
            let path = directory.join(&name);
            let size = std::fs::metadata(&path)
                .map_err(|error| {
                    PublishError::Integrity(format!("could not stat {}: {error}", path.display()))
                })?
                .len();
            let etag = multipart::etag_file(&path, options.single_put_max, options.part_size)?;
            let key = format!("{BLOB_KEY_PREFIX}{name}");
            let action = put::plan_key(size, &etag, remote.get(&key), options.single_put_max);
            if action == PutAction::Conflict {
                conflicts.push(key.clone());
            }
            entries.push(PlanEntry {
                key,
                size,
                action,
                payload: Payload::File(PathBuf::from(BLOB_DIRECTORY).join(name)),
            });
        }
    }
    if !conflicts.is_empty() {
        return Err(PublishError::Conflict(format!(
            "refusing to replace content-addressed blobs: {}",
            conflicts.join(", ")
        )));
    }
    Ok(SitePlan {
        generation: String::new(),
        entries,
        aliases: Vec::new(),
    })
}

pub fn execute(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    plan: &SitePlan,
    options: &UploadOptions,
) -> Result<SyncReport, PublishError> {
    let mut report = SyncReport {
        generation: plan.generation.clone(),
        aliases: plan.aliases.clone(),
        ..SyncReport::default()
    };
    for entry in &plan.entries {
        if entry.action == PutAction::Skip {
            report.skipped.push(entry.key.clone());
        }
    }
    for entry in upload_order(&plan.entries) {
        match entry.action {
            PutAction::Put => {
                upload_single(client, site, bucket, entry)?;
                report.uploaded.push(entry.key.clone());
                report.uploaded_bytes += entry.size;
            }
            PutAction::Multipart => {
                upload_multipart(client, site, bucket, entry, options.part_size)?;
                report.multipart.push(entry.key.clone());
                report.uploaded.push(entry.key.clone());
                report.uploaded_bytes += entry.size;
            }
            PutAction::Skip | PutAction::Conflict => {}
        }
    }
    Ok(report)
}

pub fn sync(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    options: &UploadOptions,
    excludes: &[&str],
    dry_run: bool,
) -> Result<SyncReport, PublishError> {
    let local = collect(site, options, excludes)?;
    let remote = remote_index(client, bucket)?;
    let planned = plan(&local, &remote, options)?;
    if dry_run {
        return Ok(planned.report());
    }
    execute(client, site, bucket, &planned, options)
}

pub fn sync_blobs(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    options: &UploadOptions,
    dry_run: bool,
) -> Result<SyncReport, PublishError> {
    let remote = remote_index(client, bucket)?;
    let plan = plan_blobs(site, &remote, options)?;
    if dry_run {
        return Ok(plan.report());
    }
    execute(client, site, bucket, &plan, options)
}

pub fn remote_index(
    client: &R2Client,
    bucket: &str,
) -> Result<BTreeMap<String, ObjectMeta>, PublishError> {
    Ok(client
        .list_objects_v2(bucket)?
        .into_iter()
        .map(|object| {
            (
                object.key,
                ObjectMeta {
                    size: object.size,
                    etag: object.etag,
                    content_type: None,
                },
            )
        })
        .collect())
}

fn walk(
    root: &Path,
    relative: &Path,
    options: &UploadOptions,
    excludes: &[&str],
    files: &mut BTreeMap<String, LocalFile>,
    aliases: &mut Vec<String>,
) -> Result<(), PublishError> {
    let directory = root.join(relative);
    let mut entries: Vec<_> = std::fs::read_dir(&directory)
        .map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", directory.display()))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", directory.display()))
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if relative.as_os_str().is_empty() && name.to_string_lossy().starts_with('.') {
            continue;
        }
        let child = relative.join(&name);
        let key = relative_key(&child);
        let file_type = entry.file_type().map_err(|error| {
            PublishError::Config(format!(
                "could not inspect {}: {error}",
                entry.path().display()
            ))
        })?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk(root, &child, options, excludes, files, aliases)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if excludes.iter().any(|prefix| key.starts_with(prefix)) {
            continue;
        }
        if is_shared_alias(&key) {
            aliases.push(key);
            continue;
        }
        if is_manifest_key(&key) || is_generation_stuff_manifest(&key) {
            continue;
        }
        let path = root.join(&child);
        let size = entry
            .metadata()
            .map_err(|error| {
                PublishError::Integrity(format!("could not stat {}: {error}", path.display()))
            })?
            .len();
        let etag = multipart::etag_file(&path, options.single_put_max, options.part_size)?;
        files.insert(
            key,
            LocalFile {
                size,
                etag,
                path: child,
            },
        );
    }
    Ok(())
}

fn generations(site: &Path) -> Result<Vec<String>, PublishError> {
    let directory = site.join("generations");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(PublishError::Config(format!(
                "could not read {}: {error}",
                directory.display()
            )));
        }
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", directory.display()))
        })?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_generation_id(&name) {
            found.push(name);
        }
    }
    found.sort();
    Ok(found)
}

fn relative_key(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_shared_alias(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("generations/") else {
        return false;
    };
    let Some((_generation, remainder)) = rest.split_once('/') else {
        return false;
    };
    remainder.starts_with("wheels/")
        || remainder.starts_with("tarballs/")
        || remainder.starts_with("-/tarballs/")
}

fn is_manifest_key(key: &str) -> bool {
    key.starts_with(MANIFEST_DIRECTORY) && key.ends_with(".json")
}

fn is_generation_stuff_manifest(key: &str) -> bool {
    key.starts_with("generations/") && key.ends_with("/stuff-manifest.json")
}

fn upload_order(entries: &[PlanEntry]) -> Vec<&PlanEntry> {
    let mut ordered = Vec::new();
    for entry in entries {
        if entry.action == PutAction::Put && !is_manifest_key(&entry.key) {
            ordered.push(entry);
        }
    }
    for entry in entries {
        if entry.action == PutAction::Multipart && !is_manifest_key(&entry.key) {
            ordered.push(entry);
        }
    }
    for entry in entries {
        if is_manifest_key(&entry.key) && entry.action != PutAction::Skip {
            ordered.push(entry);
        }
    }
    ordered
}

fn upload_single(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    entry: &PlanEntry,
) -> Result<(), PublishError> {
    let kind = put::content_type(&entry.key);
    match &entry.payload {
        Payload::File(relative) => {
            let path = site.join(relative);
            let body = std::fs::read(&path).map_err(|error| {
                PublishError::Integrity(format!("could not read {}: {error}", path.display()))
            })?;
            client.put_object(bucket, &entry.key, &body, kind, Precondition::None)?;
        }
        Payload::Memory(body) => {
            client.put_object(bucket, &entry.key, body, kind, Precondition::None)?;
        }
    }
    Ok(())
}

fn upload_multipart(
    client: &R2Client,
    site: &Path,
    bucket: &str,
    entry: &PlanEntry,
    part_size: u64,
) -> Result<(), PublishError> {
    let kind = put::content_type(&entry.key);
    match &entry.payload {
        Payload::File(relative) => {
            let path = site.join(relative);
            multipart::upload_file(client, bucket, &entry.key, &path, kind, part_size)?;
        }
        Payload::Memory(body) => {
            upload_memory_multipart(client, bucket, &entry.key, body, kind, part_size)?;
        }
    }
    Ok(())
}

fn upload_memory_multipart(
    client: &R2Client,
    bucket: &str,
    key: &str,
    body: &[u8],
    content_type: &str,
    part_size: u64,
) -> Result<(), PublishError> {
    if part_size == 0 {
        return Err(PublishError::Config(
            "multipart part size must be positive".to_string(),
        ));
    }
    let upload_id = multipart::initiate(client, bucket, key)?;
    let mut etags = Vec::new();
    for (index, chunk) in body.chunks(part_size as usize).enumerate() {
        let number = index + 1;
        match multipart::upload_part(client, bucket, key, number, &upload_id, chunk) {
            Ok(etag) => etags.push((number, etag)),
            Err(error) => {
                multipart::abort(client, bucket, key, &upload_id);
                return Err(error);
            }
        }
    }
    multipart::complete(client, bucket, key, &upload_id, &etags, content_type)?;
    let digests: Vec<Vec<u8>> = body
        .chunks(part_size as usize)
        .map(|chunk| Md5::digest(chunk).to_vec())
        .collect();
    let expected = multipart::combined_etag(&digests);
    let metadata = client.head_object(bucket, key)?;
    if metadata.etag != expected {
        return Err(PublishError::Integrity(format!(
            "multipart {key}: expected ETag {expected}, got {:?}",
            metadata.etag
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ExitCode;

    fn write_site(files: &[(&str, &[u8])], generation: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (key, body) in files {
            let path = dir.path().join(key);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, body).unwrap();
        }
        let target = dir.path().join("generations").join(generation);
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(
            format!("generations/{generation}"),
            dir.path().join("current"),
        )
        .unwrap();
        dir
    }

    fn object(size: u64, etag: &str) -> ObjectMeta {
        ObjectMeta {
            size,
            etag: etag.to_string(),
            content_type: None,
        }
    }

    #[test]
    fn collecting_a_site_skips_aliases_manifests_and_stuff_manifests() {
        let site = write_site(
            &[
                ("generations/g1/catalog.json", b"{}"),
                ("generations/g1/stuff-manifest.json", b"{}"),
                ("generations/g1/tarballs/alias.tgz", b"alias"),
                ("generations/g1/-/tarballs/alias.tgz", b"alias"),
                ("generations/g1/wheels/a.whl", b"wheel"),
                ("manifests/g1.json", b"{}"),
                ("tarballs/pkg.tgz", b"pkg"),
                (".hidden", b"hidden"),
            ],
            "g1",
        );
        let local = collect(site.path(), &UploadOptions::default(), &[]).unwrap();
        assert_eq!(local.generation, "g1");
        assert_eq!(
            local.files.keys().cloned().collect::<Vec<_>>(),
            vec!["generations/g1/catalog.json", "tarballs/pkg.tgz"]
        );
        assert_eq!(
            local.aliases,
            vec![
                "generations/g1/-/tarballs/alias.tgz",
                "generations/g1/tarballs/alias.tgz",
                "generations/g1/wheels/a.whl",
            ]
        );
        assert_eq!(
            local.manifests.keys().cloned().collect::<Vec<_>>(),
            vec!["manifests/g1.json"]
        );
        let manifest =
            String::from_utf8(local.manifests["manifests/g1.json"].body.clone()).unwrap();
        assert!(manifest.contains("\"key\": \"generations/g1/catalog.json\""), "{manifest}");
        assert!(manifest.contains("\"key\": \"tarballs/pkg.tgz\""), "{manifest}");
        assert!(!manifest.contains("alias"), "{manifest}");
    }

    #[test]
    fn exclusions_keep_the_oci_blob_store_out_of_the_metadata_site() {
        let site = write_site(
            &[
                ("blobs/sha256/a", b"blob"),
                ("generations/g1/catalog.json", b"{}"),
            ],
            "g1",
        );
        let local = collect(site.path(), &UploadOptions::default(), &["blobs/"]).unwrap();
        assert_eq!(
            local.files.keys().cloned().collect::<Vec<_>>(),
            vec!["generations/g1/catalog.json"]
        );
    }

    #[test]
    fn planning_skips_equal_content_and_refuses_different_bytes() {
        let site = write_site(
            &[
                ("generations/g1/catalog.json", b"{}"),
                ("tarballs/pkg.tgz", b"pkg"),
            ],
            "g1",
        );
        let options = UploadOptions::default();
        let local = collect(site.path(), &options, &[]).unwrap();
        let manifest = &local.manifests["manifests/g1.json"];
        let mut remote = BTreeMap::new();
        remote.insert(
            "tarballs/pkg.tgz".to_string(),
            object(3, &multipart::md5_hex(b"pkg")),
        );
        remote.insert(
            "generations/g1/catalog.json".to_string(),
            object(2, &multipart::md5_hex(b"{}")),
        );
        remote.insert("manifests/g1.json".to_string(), object(manifest.size, &manifest.etag));
        let planned = plan(&local, &remote, &options).unwrap();
        assert!(
            planned.entries
                .iter()
                .all(|entry| entry.action == PutAction::Skip),
            "{planned:?}"
        );

        remote.insert(
            "tarballs/pkg.tgz".to_string(),
            object(2, &multipart::md5_hex(b"no")),
        );
        let error = plan(&local, &remote, &options).unwrap_err();
        assert_eq!(error.classification(), ExitCode::Conflict);
        assert!(error.to_string().contains("tarballs/pkg.tgz"), "{error}");
    }

    #[test]
    fn manifests_for_older_generations_keep_the_remote_copy_but_the_active_one_conflicts() {
        let site = write_site(&[("generations/g1/catalog.json", b"{}")], "g1");
        std::fs::create_dir_all(site.path().join("generations/g0")).unwrap();
        std::fs::write(site.path().join("generations/g0/catalog.json"), b"old").unwrap();
        let options = UploadOptions::default();
        let local = collect(site.path(), &options, &[]).unwrap();
        let mut remote = BTreeMap::new();
        remote.insert(
            "manifests/g0.json".to_string(),
            object(9, "00000000000000000000000000000000"),
        );
        let planned = plan(&local, &remote, &options).unwrap();
        let old = planned
            .entries
            .iter()
            .find(|entry| entry.key == "manifests/g0.json")
            .unwrap();
        assert_eq!(old.action, PutAction::Skip);
        let active = planned
            .entries
            .iter()
            .find(|entry| entry.key == "manifests/g1.json")
            .unwrap();
        assert_eq!(active.action, PutAction::Put);

        remote.insert(
            "manifests/g1.json".to_string(),
            object(9, "00000000000000000000000000000000"),
        );
        let error = plan(&local, &remote, &options).unwrap_err();
        assert_eq!(error.exit_code(), 3, "{error}");
        assert!(error.to_string().contains("manifests/g1.json"), "{error}");
    }

    #[test]
    fn repair_rewrites_a_conflicting_active_manifest() {
        let site = write_site(&[("generations/g1/catalog.json", b"{}")], "g1");
        let options = UploadOptions {
            repair: true,
            ..UploadOptions::default()
        };
        let local = collect(site.path(), &options, &[]).unwrap();
        let mut remote = BTreeMap::new();
        for key in local.manifests.keys() {
            remote.insert(key.clone(), object(9, "00000000000000000000000000000000"));
        }
        let planned = plan(&local, &remote, &options).unwrap();
        let active = planned
            .entries
            .iter()
            .find(|entry| entry.key == "manifests/g1.json")
            .unwrap();
        assert_eq!(active.action, PutAction::Put);
    }

    #[test]
    fn manifests_upload_after_every_object_they_list() {
        let mut files = Vec::new();
        for index in 0..120 {
            files.push((format!("tarballs/pkg-{index}.tgz"), b"object\n".to_vec()));
        }
        let dir = tempfile::tempdir().unwrap();
        for (key, body) in &files {
            let path = dir.path().join(key);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
        }
        let generation = dir.path().join("generations/g1");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("catalog.json"), b"{}").unwrap();
        std::os::unix::fs::symlink("generations/g1", dir.path().join("current")).unwrap();
        let options = UploadOptions::default();
        let local = collect(dir.path(), &options, &[]).unwrap();
        let planned = plan(&local, &BTreeMap::new(), &options).unwrap();
        let ordered = upload_order(&planned.entries);
        assert_eq!(ordered.len(), 122);
        assert_eq!(ordered.last().unwrap().key, "manifests/g1.json");
        assert!(
            ordered[..121]
                .iter()
                .all(|entry| !entry.key.starts_with(MANIFEST_DIRECTORY))
        );
        let report = planned.report();
        assert_eq!(report.uploaded_objects(), 122);
        assert_eq!(report.skipped_objects(), 0);
    }

    #[test]
    fn blob_plans_validate_sha256_names_and_refuse_different_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = dir.path().join(BLOB_DIRECTORY);
        std::fs::create_dir_all(&blobs).unwrap();
        let hex = "a".repeat(64);
        std::fs::write(blobs.join(&hex), b"large layer").unwrap();
        let options = UploadOptions {
            single_put_max: 2,
            part_size: 4,
            repair: false,
        };
        let planned = plan_blobs(dir.path(), &BTreeMap::new(), &options).unwrap();
        assert_eq!(planned.entries.len(), 1);
        assert_eq!(planned.entries[0].key, format!("sha256/{hex}"));
        assert_eq!(planned.entries[0].action, PutAction::Multipart);

        let mut remote = BTreeMap::new();
        remote.insert(format!("sha256/{hex}"), object(11, "00000000000000000000000000000000"));
        let error = plan_blobs(dir.path(), &remote, &options).unwrap_err();
        assert_eq!(error.exit_code(), 3, "{error}");
        assert!(error.to_string().contains("content-addressed blobs"), "{error}");

        std::fs::write(blobs.join(format!("{hex}0")), b"x").unwrap();
        let error = plan_blobs(dir.path(), &BTreeMap::new(), &options).unwrap_err();
        assert_eq!(error.exit_code(), 4, "{error}");
        assert!(error.to_string().contains("sha256 hex"), "{error}");
    }
}
