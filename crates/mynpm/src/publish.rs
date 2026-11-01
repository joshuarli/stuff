#![forbid(unsafe_code)]

use crate::acquire::verify_inventory_snapshot;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::inventory::{
    canonical_json, metadata_only_to_value, normalize_metadata_only_record, validate_artifact_record, ArtifactRecord,
    Category, MetadataOnlyRecord,
};
use crate::inventory_store::load_inventory;
use crate::packument::{render_packument_files, PackumentFile, RENDERER_VERSION};
use sha2::{Digest, Sha256, Sha512};
use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct PublishSummary {
    pub generation_id: String,
    pub package_count: usize,
    pub version_count: usize,
    pub metadata_only_count: usize,
    pub no_op: bool,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[derive(Clone, Copy)]
enum CatalogSeal<'a> {
    Unsealed,
    Current,
    Stored(&'a str),
}

fn generation_payload(
    public_base_url: &str,
    artifacts: &[ArtifactRecord],
    packuments: &[PackumentFile],
    renderer: u32,
    catalog_seal: CatalogSeal<'_>,
) -> Result<deser_value::Value> {
    let mut artifact_entries = Vec::new();
    for record in artifacts {
        let mut digests = deser_value::Map::new();
        digests.insert("sha512".to_string(), deser_value::Value::from(record.artifact.sha512.clone()));
        digests.insert("integrity".to_string(), deser_value::Value::from(record.artifact.integrity.clone()));
        digests.insert("shasum".to_string(), match &record.artifact.shasum {
            Some(s) => deser_value::Value::from(s.clone()),
            None => deser_value::Value::from(deser_value::Kind::Null),
        });
        let mut entry = deser_value::Map::new();
        entry.insert("name".to_string(), deser_value::Value::from(record.name.clone()));
        entry.insert("version".to_string(), deser_value::Value::from(record.version.clone()));
        entry.insert("digests".to_string(), deser_value::Value::from(digests));
        artifact_entries.push(deser_value::Value::from(entry));
    }
    let mut packument_entries = Vec::new();
    for file in packuments {
        let mut entry = deser_value::Map::new();
        entry.insert("path".to_string(), deser_value::Value::from(file.path.clone()));
        let contents = String::from_utf8(file.bytes.clone())
            .map_err(|_| Error::Operation("packument is not valid UTF-8".to_string()))?;
        entry.insert("contents".to_string(), deser_value::Value::from(contents));
        packument_entries.push(deser_value::Value::from(entry));
    }
    let mut payload = deser_value::Map::new();
    payload.insert("schemaVersion".to_string(), deser_value::Value::from(1u64));
    payload.insert("rendererVersion".to_string(), deser_value::Value::from(u64::from(renderer)));
    payload.insert("publicBaseUrl".to_string(), deser_value::Value::from(public_base_url.to_string()));
    let mut artifacts_seq = deser_value::Seq::new();
    for entry in artifact_entries {
        artifacts_seq.push(entry);
    }
    payload.insert("artifacts".to_string(), deser_value::Value::from(artifacts_seq));
    let mut packuments_seq = deser_value::Seq::new();
    for entry in packument_entries {
        packuments_seq.push(entry);
    }
    payload.insert("packuments".to_string(), deser_value::Value::from(packuments_seq));
    payload.insert(
        "publicationLayout".to_string(),
        deser_value::Value::from("current-dash-tarballs-v1".to_string()),
    );
    let fingerprint = match catalog_seal {
        CatalogSeal::Unsealed => None,
        CatalogSeal::Stored(fingerprint) => Some(fingerprint.to_string()),
        CatalogSeal::Current => {
            let catalog = catalog_for(stuff_catalog::FINGERPRINT_GENERATION, artifacts, packuments)
                .map_err(Error::Operation)?;
            let origin = if renderer >= 7 {
                public_base_url
            } else {
                stuff_catalog::DEFAULT_NPM_ORIGIN
            };
            Some(stuff_catalog::catalog_fingerprint(&catalog, origin))
        }
    };
    if let Some(fingerprint) = fingerprint {
        payload.insert("catalogFingerprint".to_string(), deser_value::Value::from(fingerprint));
    }
    Ok(deser_value::Value::from(payload))
}

pub fn generation_id_for(public_base_url: &str, artifacts: &[ArtifactRecord], packuments: &[PackumentFile]) -> Result<String> {
    generation_id_for_renderer(public_base_url, artifacts, packuments, RENDERER_VERSION, CatalogSeal::Current)
}

fn generation_id_for_renderer(
    public_base_url: &str,
    artifacts: &[ArtifactRecord],
    packuments: &[PackumentFile],
    renderer: u32,
    catalog_seal: CatalogSeal<'_>,
) -> Result<String> {
    let payload = generation_payload(public_base_url, artifacts, packuments, renderer, catalog_seal)?;
    let text = canonical_json(&payload)?;
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn catalog_dependencies(version_metadata: &deser_value::Value) -> stuff_catalog::Dependencies {
    let names = |field: &str| -> Vec<String> {
        let Some(group) = version_metadata
            .as_map()
            .and_then(|map| map.get(field))
            .and_then(|value| value.as_map())
        else {
            return Vec::new();
        };
        (0..group.len())
            .filter_map(|i| group.get_index(i))
            .filter_map(|(name, _)| name.as_str().map(str::to_string))
            .collect()
    };
    stuff_catalog::Dependencies {
        runtime: names("dependencies"),
        peer: names("peerDependencies"),
        build: Vec::new(),
        dev: names("devDependencies"),
    }
}

fn catalog_for(
    generation_id: &str,
    artifacts: &[ArtifactRecord],
    files: &[PackumentFile],
) -> std::result::Result<stuff_catalog::Catalog, String> {
    let mut dist_latest: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for file in files {
        let text = String::from_utf8(file.bytes.clone()).map_err(|_| "packument is not valid UTF-8".to_string())?;
        let value: deser_value::Value =
            deser_json::from_str(&text).map_err(|_| "packument is not valid JSON".to_string())?;
        if let Some(map) = value.as_map()
            && let Some(tags) = map.get("dist-tags")
            && let Some(tags_map) = tags.as_map()
            && let Some(latest) = tags_map.get("latest")
            && let Some(latest_str) = latest.as_str() {
                dist_latest.insert(file.name.clone(), latest_str.to_string());
            }
    }
    let inputs = artifacts
        .iter()
        .map(|r| stuff_catalog::NpmInput {
            name: r.name.clone(),
            version: r.version.clone(),
            dependencies: catalog_dependencies(&r.metadata),
        })
        .collect();
    stuff_catalog::npm_catalog(generation_id, inputs, &dist_latest)
}

fn manifest_value(
    public_base_url: &str,
    generation_id: &str,
    artifacts: &[ArtifactRecord],
    metadata_only: &[MetadataOnlyRecord],
) -> Result<deser_value::Value> {
    let mut manifest = deser_value::Map::new();
    manifest.insert("schemaVersion".to_string(), deser_value::Value::from(1u64));
    manifest.insert("rendererVersion".to_string(), deser_value::Value::from(u64::from(RENDERER_VERSION)));
    manifest.insert("publicBaseUrl".to_string(), deser_value::Value::from(public_base_url.to_string()));
    manifest.insert("generationId".to_string(), deser_value::Value::from(generation_id.to_string()));
    let mut seq = deser_value::Seq::new();
    for record in artifacts {
        seq.push(crate::inventory::artifact_to_value(record)?);
    }
    manifest.insert("artifacts".to_string(), deser_value::Value::from(seq));
    let mut stubs = deser_value::Seq::new();
    for record in metadata_only {
        stubs.push(metadata_only_to_value(record));
    }
    manifest.insert("metadataOnly".to_string(), deser_value::Value::from(stubs));
    Ok(deser_value::Value::from(manifest))
}

fn sorted_artifacts(records: &[deser_value::Value]) -> Result<Vec<ArtifactRecord>> {
    let mut artifacts = Vec::with_capacity(records.len());
    for record in records {
        artifacts.push(validate_artifact_record(record)?);
    }
    artifacts.sort_by_key(|r| (r.name.clone(), r.version.clone()));
    Ok(artifacts)
}

pub struct GenerationManifest {
    pub renderer_version: u32,
    pub public_base_url: String,
    pub generation_id: String,
    pub artifacts: Vec<ArtifactRecord>,
    pub metadata_only: Vec<MetadataOnlyRecord>,
}

fn parse_manifest(text: &str, path: &Path) -> Result<GenerationManifest> {
    let parsed: deser_value::Value = deser_json::from_str(text)
        .map_err(|_| Error::Corrupt(format!("generation manifest is not valid JSON: {}", path.display())))?;
    let record = parsed.as_map().ok_or_else(|| {
        Error::Corrupt(format!("generation manifest must be an object: {}", path.display()))
    })?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let renderer = record.get("rendererVersion").unwrap_or(&null).as_u64().unwrap_or(0);
    let mut keys = vec!["schemaVersion", "rendererVersion", "publicBaseUrl", "generationId", "artifacts"];
    if renderer >= 6 {
        keys.push("metadataOnly");
    }
    for i in 0..record.len() {
        if let Some((k, _)) = record.get_index(i) {
            let name = k.as_str().unwrap_or("");
            if !keys.contains(&name) {
                return Err(Error::Corrupt(format!(
                    "generation manifest has an unsupported shape: {}",
                    path.display()
                )));
            }
        }
    }
    for key in keys {
        if record.get(key).is_none() {
            return Err(Error::Corrupt(format!(
                "generation manifest has an unsupported shape: {}",
                path.display()
            )));
        }
    }
    let schema_ok = record.get("schemaVersion").unwrap_or(&null).as_u64() == Some(1);
    let renderer_ok = (1..=7).contains(&renderer);
    let base_ok = record.get("publicBaseUrl").unwrap_or(&null).as_str().is_some();
    let gen_id = record.get("generationId").unwrap_or(&null).as_str().unwrap_or("").to_string();
    let artifacts_ok = record.get("artifacts").unwrap_or(&null).as_seq().is_some();
    if !schema_ok || !renderer_ok || !base_ok || !is_hex64(&gen_id) || !artifacts_ok {
        return Err(Error::Corrupt(format!(
            "generation manifest has invalid fields: {}",
            path.display()
        )));
    }
    let seq = record.get("artifacts").expect("artifacts").as_seq().expect("seq");
    let mut raw = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        raw.push(item.clone());
    }
    let artifacts = sorted_artifacts(&raw)?;
    let mut metadata_only = Vec::new();
    if renderer >= 6 {
        let stubs = record.get("metadataOnly").and_then(|v| v.as_seq()).ok_or_else(|| {
            Error::Corrupt(format!("generation manifest has invalid fields: {}", path.display()))
        })?;
        for item in stubs.iter() {
            metadata_only.push(normalize_metadata_only_record(item, "generation metadataOnly", Category::Corrupt)?);
        }
        metadata_only.sort_by_key(|r| (r.name.clone(), r.version.clone()));
    }
    let identities: Vec<String> = artifacts.iter().map(|r| format!("{}\0{}", r.name, r.version)).collect();
    let mut dedup = identities.clone();
    dedup.sort();
    dedup.dedup();
    if dedup.len() != identities.len() {
        return Err(Error::Corrupt(format!(
            "generation manifest has duplicate package identities: {}",
            path.display()
        )));
    }
    Ok(GenerationManifest {
        renderer_version: renderer as u32,
        public_base_url: record.get("publicBaseUrl").expect("base").as_str().unwrap_or("").to_string(),
        generation_id: gen_id,
        artifacts,
        metadata_only,
    })
}

fn generation_files(directory: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|_| {
        Error::Corrupt(format!("could not read generation directory {}", directory.display()))
    })? {
        let entry = entry.map_err(|_| {
            Error::Corrupt(format!("could not read generation directory {}", directory.display()))
        })?;
        let file_type = entry.file_type().map_err(|_| {
            Error::Corrupt(format!("could not read generation directory {}", directory.display()))
        })?;
        let path = entry.path();
        if file_type.is_symlink() || (!file_type.is_dir() && !file_type.is_file()) {
            return Err(Error::Corrupt(format!("generation contains an unsafe entry: {}", path.display())));
        }
        if file_type.is_dir() {
            let meta = std::fs::symlink_metadata(&path).map_err(|_| {
                Error::Corrupt(format!("could not inspect generation directory {}", path.display()))
            })?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(Error::Corrupt(format!(
                    "generation contains an unsafe directory: {}",
                    path.display()
                )));
            }
            for sub in generation_files(&path)? {
                found.push(format!("{}/{}", entry.file_name().to_string_lossy(), sub));
            }
        } else {
            found.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    Ok(found)
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| {
        Error::Corrupt(format!("managed path is not a regular file: {}", path.display()))
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::Corrupt(format!("managed path is not a regular file: {}", path.display())));
    }
    std::fs::read(path).map_err(|_| Error::Corrupt(format!("could not read regular file {}", path.display())))
}

fn hash_file(path: &Path, expected: &str) -> Result<()> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| {
        Error::Corrupt(format!("managed path is not a regular file: {}", path.display()))
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::Corrupt(format!("managed path is not a regular file: {}", path.display())));
    }
    let actual = stuff_store::stream_file_hash(path, stuff_store::HashAlg::Sha512)
        .map_err(|e| Error::Corrupt(format!("could not read published artifact {}: {e}", path.display())))?;
    if actual != expected {
        return Err(Error::Corrupt(format!("SHA-512 mismatch for published artifact {}", path.display())));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seal {
    Current,
    Adopted,
}

fn stored_catalog_fingerprint(generation_dir: &Path, id: &str) -> Option<String> {
    let read = |name: &str| String::from_utf8(read_regular_file(&generation_dir.join(name)).ok()?).ok();
    Some(stuff_catalog::stored_catalog_fingerprint(
        id,
        &read("catalog.json")?,
        &read("index.html")?,
        &read("llms.txt")?,
    ))
}

fn validate_generation(
    generations_dir: &Path,
    id: &str,
    mirror_root: &Path,
    seal: Seal,
) -> Result<Option<GenerationManifest>> {
    Ok(validate_generation_sealed(generations_dir, id, mirror_root, seal)?.map(|(manifest, _, _)| manifest))
}

fn validate_generation_sealed(
    generations_dir: &Path,
    id: &str,
    mirror_root: &Path,
    seal: Seal,
) -> Result<Option<(GenerationManifest, bool, usize)>> {
    let generation_dir = generations_dir.join(id);
    match std::fs::symlink_metadata(&generation_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::Operation(format!("could not inspect generation {id}: {e}")));
        }
        Ok(meta) => {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err(Error::Corrupt(format!(
                    "generation is not a real directory: {}",
                    generation_dir.display()
                )));
            }
        }
    }
    let manifest_path = generation_dir.join("manifest.json");
    let manifest_bytes = read_regular_file(&manifest_path).map_err(|_| {
        Error::Corrupt(format!("generation manifest is missing: {}", manifest_path.display()))
    })?;
    let manifest_text = String::from_utf8(manifest_bytes)
        .map_err(|_| Error::Corrupt(format!("generation manifest is not valid JSON: {}", manifest_path.display())))?;
    let manifest = parse_manifest(&manifest_text, &manifest_path)?;
    let artifact_values: Vec<deser_value::Value> = manifest
        .artifacts
        .iter()
        .map(crate::inventory::artifact_to_value)
        .collect::<Result<Vec<_>>>()?;
    let files = render_packument_files(
        &artifact_values,
        &manifest.metadata_only,
        &manifest.public_base_url,
        Some(manifest.renderer_version),
    )
        .map_err(|_| Error::Corrupt(format!("could not render generation packuments: {id}")))?;
    let corrupt_render = || Error::Corrupt(format!("could not render generation packuments: {id}"));
    let identity_for = |seal: CatalogSeal<'_>| {
        generation_id_for_renderer(&manifest.public_base_url, &manifest.artifacts, &files, manifest.renderer_version, seal)
    };
    let sealed_by_current_renderer = identity_for(CatalogSeal::Current).map_err(|_| corrupt_render())? == id;
    let sealed_before_catalog = seal == Seal::Adopted
        && identity_for(CatalogSeal::Unsealed).map_err(|_| corrupt_render())? == id;
    let sealed_by_older_renderer = !sealed_by_current_renderer
        && !sealed_before_catalog
        && match stored_catalog_fingerprint(&generation_dir, id) {
            Some(fingerprint) => identity_for(CatalogSeal::Stored(&fingerprint)).map_err(|_| corrupt_render())? == id,
            None => false,
        };
    if !(sealed_by_current_renderer || sealed_by_older_renderer || sealed_before_catalog) || manifest.generation_id != id {
        return Err(Error::Corrupt(format!("generation identity does not match its contents: {id}")));
    }
    let mut digests: Vec<String> = manifest.artifacts.iter().map(|r| r.artifact.sha512.clone()).collect();
    digests.sort();
    digests.dedup();
    let mut expected: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    match manifest.renderer_version {
        1 => {}
        2 => {
            for digest in &digests {
                expected.push(format!("tarballs/{digest}.tgz"));
            }
        }
        _ => {
            for digest in &digests {
                expected.push(format!("tarballs/{digest}.tgz"));
                expected.push(format!("-/tarballs/{digest}.tgz"));
            }
        }
    }
    if manifest.renderer_version >= 4 {
        expected.push("catalog.json".to_string());
        expected.push("index.html".to_string());
    }
    if manifest.renderer_version >= 5 {
        expected.push("llms.txt".to_string());
    }
    expected.push("manifest.json".to_string());
    expected.sort();
    let mut actual = generation_files(&generation_dir)?;
    actual.sort();
    if expected != actual {
        return Err(Error::Corrupt(format!("generation file set does not match its manifest: {id}")));
    }
    for file in &files {
        let actual_bytes = read_regular_file(&generation_dir.join(&file.path))?;
        if actual_bytes != file.bytes {
            return Err(Error::Corrupt(format!(
                "generation packument does not match its manifest: {}",
                file.path
            )));
        }
    }
    if manifest.renderer_version >= 4 && seal == Seal::Current && sealed_by_current_renderer {
        let catalog = catalog_for(id, &manifest.artifacts, &files)
            .map_err(|_| Error::Corrupt(format!("could not render generation catalog: {id}")))?;
        let expected_json = stuff_catalog::render_catalog_json(&catalog);
        let actual_json = read_regular_file(&generation_dir.join("catalog.json"))?;
        if actual_json != expected_json.as_bytes() {
            return Err(Error::Corrupt(format!("generation catalog does not match its manifest: {id}")));
        }
        let expected_html = stuff_catalog::render_html(&catalog);
        let actual_html = read_regular_file(&generation_dir.join("index.html"))?;
        if actual_html != expected_html.as_bytes() {
            return Err(Error::Corrupt(format!("generation catalog page does not match its manifest: {id}")));
        }
    }
    if manifest.renderer_version >= 5 && seal == Seal::Current && sealed_by_current_renderer {
        let catalog = catalog_for(id, &manifest.artifacts, &files)
            .map_err(|_| Error::Corrupt(format!("could not render generation catalog: {id}")))?;
        let origin = if manifest.renderer_version >= 7 {
            manifest.public_base_url.as_str()
        } else {
            stuff_catalog::DEFAULT_NPM_ORIGIN
        };
        let expected_llms = stuff_catalog::render_llms_txt(&catalog, origin);
        let actual_llms = read_regular_file(&generation_dir.join("llms.txt"))?;
        if actual_llms != expected_llms.as_bytes() {
            return Err(Error::Corrupt(format!("generation llms.txt does not match its manifest: {id}")));
        }
    }
    let mut verified: HashMap<(u64, u64), String> = HashMap::new();
    for record in &manifest.artifacts {
        let digest = record.artifact.sha512.as_str();
        let mut paths = vec![mirror_root.join("tarballs").join(format!("{digest}.tgz"))];
        if manifest.renderer_version != 1 {
            paths.push(generation_dir.join("tarballs").join(format!("{digest}.tgz")));
        }
        if manifest.renderer_version >= 3 {
            paths.push(generation_dir.join("-").join("tarballs").join(format!("{digest}.tgz")));
        }
        for path in paths {
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
                Error::Corrupt(format!("could not inspect published artifact {}", path.display()))
            })?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(Error::Corrupt(format!(
                    "managed path is not a regular file: {}",
                    path.display()
                )));
            }
            let key = (metadata.dev(), metadata.ino());
            if let Some(previous) = verified.get(&key) {
                if previous != digest {
                    return Err(Error::Corrupt(format!(
                        "published artifacts share a file with different digests: {}",
                        path.display()
                    )));
                }
                continue;
            }
            if seal == Seal::Adopted {
                hash_file(&path, digest)?;
            }
            verified.insert(key, digest.to_string());
        }
    }
    Ok(Some((manifest, sealed_by_current_renderer, files.len())))
}

fn read_current(
    mirror_root: &Path,
    generations_dir: &Path,
) -> Result<(Option<String>, Vec<ArtifactRecord>, Vec<MetadataOnlyRecord>)> {
    let site = stuff_store::Site::new(mirror_root.to_path_buf());
    let Some(id) = site
        .current_generation()
        .map_err(|e| Error::Corrupt(e.to_string()))?
    else {
        return Ok((None, Vec::new(), Vec::new()));
    };
    let manifest = validate_generation(generations_dir, &id, mirror_root, Seal::Current)?.ok_or_else(|| {
        Error::Corrupt(format!("current publication points to a missing generation: {id}"))
    })?;
    Ok((Some(format!("generations/{id}")), manifest.artifacts, manifest.metadata_only))
}

fn merge_artifacts(retained: &[ArtifactRecord], incoming: &[ArtifactRecord]) -> Result<Vec<ArtifactRecord>> {
    let mut merged: BTreeMap<String, ArtifactRecord> = BTreeMap::new();
    for record in retained {
        merged.insert(format!("{}\0{}", record.name, record.version), record.clone());
    }
    for record in incoming {
        let key = format!("{}\0{}", record.name, record.version);
        if let Some(existing) = merged.get(&key)
            && existing.artifact.sha512 != record.artifact.sha512 {
                return Err(Error::Conflict(format!(
                    "published digest conflicts for {}@{}",
                    record.name, record.version
                )));
            }
        merged.insert(key, record.clone());
    }
    let mut out: Vec<ArtifactRecord> = merged.into_values().collect();
    out.sort_by_key(|r| (r.name.clone(), r.version.clone()));
    Ok(out)
}

fn merge_metadata_only(
    retained: &[MetadataOnlyRecord],
    incoming: &[MetadataOnlyRecord],
    artifacts: &[ArtifactRecord],
) -> Result<Vec<MetadataOnlyRecord>> {
    let mut merged: BTreeMap<String, MetadataOnlyRecord> = BTreeMap::new();
    for record in retained.iter().chain(incoming) {
        let key = format!("{}\0{}", record.name, record.version);
        if let Some(existing) = merged.get(&key)
            && existing != record
        {
            return Err(Error::Conflict(format!(
                "published metadata-only record conflicts for {}@{}",
                record.name, record.version
            )));
        }
        merged.insert(key, record.clone());
    }
    let mut out = Vec::with_capacity(merged.len());
    for record in merged.into_values() {
        match artifacts.iter().find(|a| a.name == record.name && a.version == record.version) {
            Some(artifact) if artifact.artifact.integrity != record.integrity => {
                return Err(Error::Conflict(format!(
                    "hosted artifact conflicts with metadata-only record for {}@{}",
                    record.name, record.version
                )));
            }
            Some(_) => {}
            None => out.push(record),
        }
    }
    out.sort_by_key(|r| (r.name.clone(), r.version.clone()));
    Ok(out)
}

fn ensure_real_dir(path: &Path) -> Result<()> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(Error::Operation(format!("could not create directory {}: {e}", path.display())));
        }
    }
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::Operation(format!("could not inspect directory {}", path.display())))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::Operation(format!("managed path is not a real directory: {}", path.display())));
    }
    Ok(())
}

fn ensure_dir_tree(target: &Path) -> Result<PathBuf> {
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::Operation(format!("could not resolve site directory: {e}")))?
            .join(target)
    };
    if absolute.parent().is_none() {
        return Err(Error::Operation(format!(
            "publish site cannot be a filesystem root: {}",
            absolute.display()
        )));
    }
    let mut existing = absolute.clone();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(meta) => {
                if existing == absolute && meta.file_type().is_symlink() {
                    return Err(Error::Operation(format!(
                        "managed path is not a real directory: {}",
                        existing.display()
                    )));
                }
                if !meta.is_dir() && !meta.file_type().is_symlink() {
                    return Err(Error::Operation(format!(
                        "managed path is not a real directory: {}",
                        existing.display()
                    )));
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(Error::Operation(format!(
                        "could not resolve site directory: {}",
                        absolute.display()
                    )));
                };
                missing.push(name.to_os_string());
                if !existing.pop() {
                    return Err(Error::Operation(format!(
                        "could not resolve site directory: {}",
                        absolute.display()
                    )));
                }
            }
            Err(e) => {
                return Err(Error::Operation(format!(
                    "could not inspect directory {}: {e}",
                    existing.display()
                )));
            }
        }
    }
    let mut current = std::fs::canonicalize(&existing)
        .map_err(|e| Error::Operation(format!("could not resolve {}: {e}", existing.display())))?;
    for name in missing.iter().rev() {
        current.push(name);
        ensure_real_dir(&current)?;
    }
    Ok(current)
}

fn write_durable_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(path)
        .map_err(|_| Error::Operation(format!("could not create file {}", path.display())))?;
    file.write_all(bytes)
        .map_err(|_| Error::Operation(format!("could not write file {}", path.display())))?;
    file.sync_all()
        .map_err(|_| Error::Operation(format!("could not synchronize file {}", path.display())))?;
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
        .map_err(|_| Error::Operation(format!("could not set file permissions: {}", path.display())))?;
    Ok(())
}

fn copy_artifact(record: &ArtifactRecord, state_dir: &Path, tarballs_dir: &Path) -> Result<()> {
    use std::io::Read;
    let digest = &record.artifact.sha512;
    let destination = tarballs_dir.join(format!("{digest}.tgz"));
    let source = state_dir.join("objects").join("sha512").join(format!("{digest}.tgz"));
    let source_meta = std::fs::symlink_metadata(&source).map_err(|_| {
        Error::Corrupt(format!("managed path is not a regular file: {}", source.display()))
    })?;
    if !source_meta.is_file() || source_meta.file_type().is_symlink() {
        return Err(Error::Corrupt(format!("managed path is not a regular file: {}", source.display())));
    }
    match std::fs::symlink_metadata(&destination) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(Error::Corrupt(format!(
                    "managed path is not a regular file: {}",
                    destination.display()
                )));
            }
            if meta.len() == source_meta.len() {
                return Ok(());
            }
            return Err(Error::Corrupt(format!(
                "published artifact size mismatch for {}@{}",
                record.name, record.version
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Error::Operation(format!(
                "could not inspect published artifact {}: {e}",
                destination.display()
            )));
        }
    }
    let tmp = stuff_store::tmp_sibling(&destination)?;
    let result = (|| -> Result<()> {
        let mut reader = std::fs::File::open(&source)
            .map_err(|_| Error::Operation(format!("could not read state artifact {}", source.display())))?;
        let mut writer = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o644)
            .open(&tmp)
            .map_err(|_| Error::Operation(format!("could not create temporary artifact {}", tmp.display())))?;
        let mut hasher = Sha512::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = reader.read(&mut buf).map_err(|_| {
                Error::Operation(format!("could not read state artifact {}", source.display()))
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            writer.write_all(&buf[..n]).map_err(|_| {
                Error::Operation(format!("could not publish artifact {}@{}", record.name, record.version))
            })?;
        }
        writer.sync_all()
            .map_err(|_| Error::Operation(format!("could not synchronize artifact {}", tmp.display())))?;
        drop(writer);
        if format!("{:x}", hasher.finalize()) != *digest {
            return Err(Error::Corrupt(format!(
                "state artifact digest mismatch for {}@{}",
                record.name, record.version
            )));
        }
        match std::fs::hard_link(&tmp, &destination) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let meta = std::fs::symlink_metadata(&destination).map_err(|_| {
                    Error::Corrupt(format!("managed path is not a regular file: {}", destination.display()))
                })?;
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != source_meta.len() {
                    return Err(Error::Corrupt(format!(
                        "published artifact size mismatch for {}@{}",
                        record.name, record.version
                    )));
                }
            }
            Err(_) => {
                return Err(Error::Operation(format!(
                    "could not publish artifact {}@{}",
                    record.name, record.version
                )));
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    result?;
    stuff_store::sync_dir(tarballs_dir)?;
    Ok(())
}

fn write_tarball_aliases(directory: &Path, artifacts: &[ArtifactRecord], mirror_root: &Path) -> Result<()> {
    let mut digests: Vec<String> = artifacts.iter().map(|r| r.artifact.sha512.clone()).collect();
    digests.sort();
    digests.dedup();
    if digests.is_empty() {
        return Ok(());
    }
    for relative in ["tarballs", "-/tarballs"] {
        let directory_path = directory.join(relative);
        std::fs::create_dir_all(&directory_path).map_err(|_| {
            Error::Operation(format!(
                "could not create generation tarballs directory {}",
                directory_path.display()
            ))
        })?;
        for digest in &digests {
            let alias = directory_path.join(format!("{digest}.tgz"));
            if std::fs::symlink_metadata(&alias).is_err() {
                std::fs::hard_link(mirror_root.join("tarballs").join(format!("{digest}.tgz")), &alias).map_err(|_| {
                    Error::Operation(format!("could not expose published artifact {digest}"))
                })?;
            }
        }
    }
    Ok(())
}

fn build_generation(
    staging: &Path,
    manifest: &GenerationManifest,
    files: &[PackumentFile],
    mirror_root: &Path,
) -> Result<()> {
    for file in files {
        let path = staging.join(&file.path);
        if let Some(parent) = path.parent()
            && parent != staging
        {
            std::fs::create_dir_all(parent).map_err(|_| {
                Error::Operation(format!("could not create generation directory {}", parent.display()))
            })?;
        }
        write_durable_file(&path, &file.bytes)?;
    }
    write_tarball_aliases(staging, &manifest.artifacts, mirror_root)?;
    if manifest.renderer_version >= 4 {
        let catalog = catalog_for(&manifest.generation_id, &manifest.artifacts, files)
            .map_err(|e| Error::Operation(format!("could not render generation catalog: {e}")))?;
        write_durable_file(
            &staging.join("catalog.json"),
            stuff_catalog::render_catalog_json(&catalog).as_bytes(),
        )?;
        write_durable_file(
            &staging.join("index.html"),
            stuff_catalog::render_html(&catalog).as_bytes(),
        )?;
        if manifest.renderer_version >= 5 {
            write_durable_file(
                &staging.join("llms.txt"),
                stuff_catalog::render_llms_txt(&catalog, &manifest.public_base_url).as_bytes(),
            )?;
        }
    }
    let manifest_text = canonical_json(&manifest_value(
        &manifest.public_base_url,
        &manifest.generation_id,
        &manifest.artifacts,
        &manifest.metadata_only,
    )?)?;
    write_durable_file(&staging.join("manifest.json"), manifest_text.as_bytes())?;
    Ok(())
}

fn merge_publish_targets(
    retained: &[ArtifactRecord],
    incoming: &[ArtifactRecord],
) -> Result<Vec<ArtifactRecord>> {
    merge_artifacts(retained, incoming)
}

pub struct PublishOptions<'a> {
    pub state_directory: &'a Path,
    pub site_directory: &'a Path,
    pub config: &'a Config,
    pub render_base_url: Option<String>,
}

struct Mirror {
    site: stuff_store::Site,
    root: PathBuf,
    generations: PathBuf,
    tarballs: PathBuf,
    _lock: stuff_store::FileLock,
}

fn open_mirror(site_directory: &Path) -> Result<Mirror> {
    let site_root = ensure_dir_tree(site_directory)?;
    let root = site_root.join("mirror");
    ensure_real_dir(&root)?;
    let site = stuff_store::Site::new(root.clone());
    site.ensure_layout()?;
    let generations = site.generations_dir();
    let tarballs = root.join("tarballs");
    ensure_real_dir(&generations)?;
    ensure_real_dir(&tarballs)?;
    let lock = site.lock(std::time::Duration::ZERO).map_err(|e| match e {
        stuff_store::Error::Conflict(_) => {
            Error::Conflict("another mynpm operation is updating publication".to_string())
        }
        other => other.into(),
    })?;
    site.recover_staging()?;
    remove_tarball_temps(&tarballs)?;
    site.check_no_symlinks_in_path()
        .map_err(|e| Error::Corrupt(e.to_string()))?;
    Ok(Mirror { site, root, generations, tarballs, _lock: lock })
}

fn remove_tarball_temps(tarballs: &Path) -> Result<()> {
    for entry in std::fs::read_dir(tarballs)
        .map_err(|_| Error::Operation(format!("could not read {}", tarballs.display())))?
    {
        let entry = entry
            .map_err(|_| Error::Operation(format!("could not read {}", tarballs.display())))?;
        if entry.file_name().to_string_lossy().starts_with(".tmp.") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn activate(
    mirror: &Mirror,
    artifacts: &[ArtifactRecord],
    metadata_only: &[MetadataOnlyRecord],
    public_base_url: &str,
    state_directory: Option<&Path>,
) -> Result<PublishSummary> {
    let artifact_values: Vec<deser_value::Value> = artifacts
        .iter()
        .map(crate::inventory::artifact_to_value)
        .collect::<Result<Vec<_>>>()?;
    let files = render_packument_files(&artifact_values, metadata_only, public_base_url, None)
        .map_err(|e| Error::Operation(format!("could not render publication packuments: {e}")))?;
    let id = generation_id_for(public_base_url, artifacts, &files)
        .map_err(|e| Error::Operation(format!("could not identify publication generation: {e}")))?;
    let manifest_text = canonical_json(&manifest_value(public_base_url, &id, artifacts, metadata_only)?)?;
    let manifest = parse_manifest(&manifest_text, Path::new("candidate manifest"))?;
    for record in artifacts {
        match state_directory {
            Some(state) => copy_artifact(record, state, &mirror.tarballs)?,
            None => hash_file(
                &mirror.tarballs.join(format!("{}.tgz", record.artifact.sha512)),
                &record.artifact.sha512,
            )?,
        }
    }
    let build = |staging: &Path| -> stuff_store::Result<()> {
        build_generation(staging, &manifest, &files, &mirror.root).map_err(stuff_store::Error::from)
    };
    let verify = |_generation_dir: &Path| -> stuff_store::Result<bool> {
        validate_generation(&mirror.generations, &id, &mirror.root, Seal::Current)
            .map(|found| found.is_some())
            .map_err(stuff_store::Error::from)
    };
    let activation = mirror.site.publish(&id, &build, &verify)?;
    Ok(PublishSummary {
        generation_id: activation.generation_id,
        package_count: files.len(),
        version_count: artifacts.len(),
        metadata_only_count: metadata_only.len(),
        no_op: activation.is_noop,
    })
}

pub fn publish_site(options: &PublishOptions) -> Result<PublishSummary> {
    let mirror = open_mirror(options.site_directory)?;
    let inventory = load_inventory(options.state_directory)?;
    verify_inventory_snapshot(options.state_directory, options.config, &inventory)?;
    let (_, retained, retained_metadata_only) = read_current(&mirror.root, &mirror.generations)?;
    let artifacts = merge_publish_targets(&retained, &inventory.artifacts)?;
    let metadata_only = merge_metadata_only(&retained_metadata_only, &inventory.metadata_only, &artifacts)?;
    let public_base_url = options
        .render_base_url
        .clone()
        .unwrap_or_else(|| options.config.public_base_url.clone());
    activate(
        &mirror,
        &artifacts,
        &metadata_only,
        &public_base_url,
        Some(options.state_directory),
    )
}

pub fn adopt_site(site_directory: &Path) -> Result<PublishSummary> {
    let mirror = open_mirror(site_directory)?;
    let id = mirror
        .site
        .current_generation()
        .map_err(|e| Error::Corrupt(e.to_string()))?
        .ok_or_else(|| Error::Input(format!("no current publication under {}", mirror.root.display())))?;
    let generation_dir = mirror.generations.join(&id);
    let manifest_path = generation_dir.join("manifest.json");
    let manifest_text = String::from_utf8(read_regular_file(&manifest_path)?)
        .map_err(|_| Error::Corrupt(format!("generation manifest is not valid JSON: {}", manifest_path.display())))?;
    let listed = parse_manifest(&manifest_text, &manifest_path)?;
    if listed.renderer_version < 3 {
        return Err(Error::Input(format!(
            "adopt supports renderer 3 and later, generation {id} uses renderer {}",
            listed.renderer_version
        )));
    }
    write_tarball_aliases(&generation_dir, &listed.artifacts, &mirror.root)?;
    let (adopted, sealed, package_count) =
        validate_generation_sealed(&mirror.generations, &id, &mirror.root, Seal::Adopted)?
            .ok_or_else(|| Error::Corrupt(format!("current publication points to a missing generation: {id}")))?;
    if sealed {
        return Ok(PublishSummary {
            generation_id: id,
            package_count,
            version_count: adopted.artifacts.len(),
            metadata_only_count: adopted.metadata_only.len(),
            no_op: true,
        });
    }
    activate(
        &mirror,
        &adopted.artifacts,
        &adopted.metadata_only,
        &adopted.public_base_url,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_dependencies_keep_runtime_peer_and_dev_names() {
        let metadata: deser_value::Value = deser_json::from_str(
            r#"{"name":"a","version":"1.0.0","dependencies":{"b":"^1","@s/c":"2"},"devDependencies":{"d":"1"},"peerDependencies":{"e":"1"},"optionalDependencies":{"f":"1"}}"#,
        )
        .unwrap();
        let mut dependencies = catalog_dependencies(&metadata);
        dependencies.runtime.sort();
        assert_eq!(dependencies.runtime, ["@s/c", "b"]);
        assert_eq!(dependencies.peer, ["e"]);
        assert_eq!(dependencies.dev, ["d"]);
        assert!(dependencies.build.is_empty());
        let bare: deser_value::Value = deser_json::from_str(r#"{"name":"a","version":"1.0.0"}"#).unwrap();
        assert!(catalog_dependencies(&bare).is_empty());
    }

    #[test]
    fn directory_tree_accepts_a_symlinked_parent() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let target = link.join("site");
        let resolved = ensure_dir_tree(&target).unwrap();
        assert!(resolved.is_dir());
        assert!(std::fs::symlink_metadata(&target).unwrap().is_dir());
    }

    #[test]
    fn generation_writes_catalog_and_landing_page() {
        use base64::Engine;
        use sha2::{Digest, Sha512};
        let tgz = b"fake-npm-tarball";
        let mut h = Sha512::new();
        h.update(tgz);
        let digest = format!("{:x}", h.finalize());
        let raw: Vec<u8> = (0..digest.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&digest[i..i + 2], 16).unwrap())
            .collect();
        let integrity = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(&raw));
        let tarball = "https://registry.npmjs.org/flatdemo/-/flatdemo-1.2.3.tgz";
        let text = format!(
            "{{\"name\":\"flatdemo\",\"version\":\"1.2.3\",\"packument\":{{\"name\":\"flatdemo\",\"dist-tags\":{{\"latest\":\"1.2.3\"}}}},\"metadata\":{{\"name\":\"flatdemo\",\"version\":\"1.2.3\",\"dist\":{{\"tarball\":\"{tarball}\"}}}},\"artifact\":{{\"sha512\":\"{digest}\",\"integrity\":\"{integrity}\",\"shasum\":null,\"sourceUrl\":\"{tarball}\",\"legacySha1Only\":false}}}}"
        );
        let value: deser_value::Value = deser_json::from_str(&text).unwrap();
        let record = validate_artifact_record(&value).unwrap();
        let base = "http://127.0.0.1:8080/mirror/";
        let files = render_packument_files(
            &[crate::inventory::artifact_to_value(&record).unwrap()],
            &[],
            base,
            None,
        )
        .unwrap();
        let id = generation_id_for(base, std::slice::from_ref(&record), &files).unwrap();

        let catalog = catalog_for(&id, std::slice::from_ref(&record), &files).unwrap();
        assert_eq!(catalog.generation, id);
        assert_eq!(catalog.packages.len(), 1);
        assert_eq!(catalog.packages[0].latest, "1.2.3");
        let parsed = stuff_catalog::parse_catalog_json(&stuff_catalog::render_catalog_json(&catalog)).unwrap();
        assert_eq!(parsed, catalog);

        let dir = tempfile::tempdir().unwrap();
        let mirror_root = dir.path().join("mirror");
        std::fs::create_dir_all(mirror_root.join("tarballs")).unwrap();
        std::fs::write(mirror_root.join("tarballs").join(format!("{digest}.tgz")), tgz).unwrap();
        let manifest_text = canonical_json(&manifest_value(base, &id, std::slice::from_ref(&record), &[]).unwrap()).unwrap();
        let manifest = parse_manifest(&manifest_text, Path::new("candidate manifest")).unwrap();
        let site = stuff_store::Site::new(mirror_root.clone());
        site.publish(
            &id,
            &|staging: &Path| {
                build_generation(staging, &manifest, &files, &mirror_root).map_err(stuff_store::Error::from)
            },
            &|_generation_dir: &Path| {
                validate_generation(&mirror_root.join("generations"), &id, &mirror_root, Seal::Current)
                    .map(|found| found.is_some())
                    .map_err(stuff_store::Error::from)
            },
        )
        .unwrap();
        let gen_dir = mirror_root.join("generations").join(&id);
        let stored = stuff_catalog::parse_catalog_json(
            &std::fs::read_to_string(gen_dir.join("catalog.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored, catalog);
        let landing = std::fs::read_to_string(gen_dir.join("index.html")).unwrap();
        assert!(landing.contains("flatdemo"));
        let llms = std::fs::read_to_string(gen_dir.join("llms.txt")).unwrap();
        assert!(llms.starts_with("# npm registry\n"));
        assert!(llms.contains("flatdemo"));
        assert!(validate_generation(&mirror_root.join("generations"), &id, &mirror_root, Seal::Current).unwrap().is_some());
    }

    fn fixture_record(tgz: &[u8]) -> (ArtifactRecord, String) {
        use base64::Engine;
        let mut h = Sha512::new();
        h.update(tgz);
        let digest = format!("{:x}", h.finalize());
        let raw: Vec<u8> = (0..digest.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&digest[i..i + 2], 16).unwrap())
            .collect();
        let integrity = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(&raw));
        let tarball = "https://registry.npmjs.org/flatdemo/-/flatdemo-1.2.3.tgz";
        let text = format!(
            "{{\"name\":\"flatdemo\",\"version\":\"1.2.3\",\"packument\":{{\"name\":\"flatdemo\",\"dist-tags\":{{\"latest\":\"1.2.3\"}}}},\"metadata\":{{\"name\":\"flatdemo\",\"version\":\"1.2.3\",\"dist\":{{\"tarball\":\"{tarball}\"}}}},\"artifact\":{{\"sha512\":\"{digest}\",\"integrity\":\"{integrity}\",\"shasum\":null,\"sourceUrl\":\"{tarball}\",\"legacySha1Only\":false}}}}"
        );
        let value: deser_value::Value = deser_json::from_str(&text).unwrap();
        (validate_artifact_record(&value).unwrap(), digest)
    }

    fn write_legacy_generation(mirror_root: &Path, record: &ArtifactRecord, base: &str) -> String {
        let files = render_packument_files(
            &[crate::inventory::artifact_to_value(record).unwrap()],
            &[],
            base,
            Some(5),
        )
        .unwrap();
        let id = generation_id_for_renderer(base, std::slice::from_ref(record), &files, 5, CatalogSeal::Unsealed).unwrap();
        let dir = mirror_root.join("generations").join(&id);
        for file in &files {
            let path = dir.join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &file.bytes).unwrap();
        }
        for name in ["catalog.json", "index.html", "llms.txt"] {
            std::fs::write(dir.join(name), "rendered by an older catalog renderer").unwrap();
        }
        let mut manifest = manifest_value(base, &id, std::slice::from_ref(record), &[]).unwrap();
        let map = manifest.as_map_mut().unwrap();
        map.insert("rendererVersion".to_string(), deser_value::Value::from(5u64));
        map.remove("metadataOnly");
        std::fs::write(dir.join("manifest.json"), canonical_json(&manifest).unwrap()).unwrap();
        std::os::unix::fs::symlink(format!("generations/{id}"), mirror_root.join("current")).unwrap();
        id
    }

    fn legacy_site(tgz: &[u8]) -> (tempfile::TempDir, String, ArtifactRecord) {
        let (record, digest) = fixture_record(tgz);
        let dir = tempfile::tempdir().unwrap();
        let mirror_root = dir.path().join("mirror");
        std::fs::create_dir_all(mirror_root.join("tarballs")).unwrap();
        std::fs::create_dir_all(mirror_root.join("generations")).unwrap();
        std::fs::write(mirror_root.join("tarballs").join(format!("{digest}.tgz")), tgz).unwrap();
        let id = write_legacy_generation(&mirror_root, &record, "https://npm.example.test/");
        (dir, id, record)
    }

    #[test]
    fn adopt_reseals_a_generation_published_before_catalog_fingerprints() {
        let (dir, legacy_id, record) = legacy_site(b"fake-npm-tarball");
        let mirror_root = dir.path().join("mirror");
        let generations = mirror_root.join("generations");
        assert!(read_current(&mirror_root, &generations).is_err());

        let summary = adopt_site(dir.path()).unwrap();
        assert!(!summary.no_op);
        assert_ne!(summary.generation_id, legacy_id);
        assert_eq!(summary.version_count, 1);
        let (target, retained, _) = read_current(&mirror_root, &generations).unwrap();
        assert_eq!(target.unwrap(), format!("generations/{}", summary.generation_id));
        assert_eq!(retained, vec![record]);
        assert_eq!(
            std::fs::read(generations.join(&legacy_id).join("flatdemo")).unwrap(),
            std::fs::read(generations.join(&summary.generation_id).join("flatdemo")).unwrap()
        );
        assert!(adopt_site(dir.path()).unwrap().no_op);
    }

    fn write_generation_sealed_by_an_older_catalog_renderer(
        mirror_root: &Path,
        record: &ArtifactRecord,
        base: &str,
    ) -> String {
        let files = render_packument_files(
            &[crate::inventory::artifact_to_value(record).unwrap()],
            &[],
            base,
            None,
        )
        .unwrap();
        let catalog_files = [
            ("catalog.json", "catalog from an older renderer"),
            ("index.html", "landing page from an older renderer"),
            ("llms.txt", "llms from an older renderer"),
        ];
        let fingerprint = stuff_catalog::stored_catalog_fingerprint(
            "unused",
            catalog_files[0].1,
            catalog_files[1].1,
            catalog_files[2].1,
        );
        let id = generation_id_for_renderer(base, std::slice::from_ref(record), &files, RENDERER_VERSION, CatalogSeal::Stored(&fingerprint)).unwrap();
        let dir = mirror_root.join("generations").join(&id);
        for file in &files {
            let path = dir.join(&file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &file.bytes).unwrap();
        }
        for (name, contents) in catalog_files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        let digest = &record.artifact.sha512;
        for tarballs in [dir.join("tarballs"), dir.join("-").join("tarballs")] {
            std::fs::create_dir_all(&tarballs).unwrap();
            std::fs::copy(
                mirror_root.join("tarballs").join(format!("{digest}.tgz")),
                tarballs.join(format!("{digest}.tgz")),
            )
            .unwrap();
        }
        let manifest = manifest_value(base, &id, std::slice::from_ref(record), &[]).unwrap();
        std::fs::write(dir.join("manifest.json"), canonical_json(&manifest).unwrap()).unwrap();
        std::os::unix::fs::symlink(format!("generations/{id}"), mirror_root.join("current")).unwrap();
        id
    }

    #[test]
    fn a_generation_sealed_by_an_older_catalog_renderer_stays_readable() {
        let (record, digest) = fixture_record(b"fake-npm-tarball");
        let dir = tempfile::tempdir().unwrap();
        let mirror_root = dir.path().join("mirror");
        std::fs::create_dir_all(mirror_root.join("tarballs")).unwrap();
        std::fs::create_dir_all(mirror_root.join("generations")).unwrap();
        std::fs::write(mirror_root.join("tarballs").join(format!("{digest}.tgz")), b"fake-npm-tarball").unwrap();
        let id = write_generation_sealed_by_an_older_catalog_renderer(&mirror_root, &record, "https://npm.example.test/");
        let generations = mirror_root.join("generations");
        for seal in [Seal::Current, Seal::Adopted] {
            assert!(validate_generation(&generations, &id, &mirror_root, seal).unwrap().is_some());
        }
        assert!(read_current(&mirror_root, &generations).is_ok());

        std::fs::write(generations.join(&id).join("index.html"), "tampered").unwrap();
        assert!(matches!(
            validate_generation(&generations, &id, &mirror_root, Seal::Current),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn adopt_rejects_a_tampered_tarball_or_packument() {
        let (dir, legacy_id, record) = legacy_site(b"fake-npm-tarball");
        let tarball = dir.path().join("mirror/tarballs").join(format!("{}.tgz", record.artifact.sha512));
        std::fs::write(&tarball, b"tampered").unwrap();
        assert!(matches!(adopt_site(dir.path()).unwrap_err(), Error::Corrupt(_)));

        let (dir, legacy_id_2, _) = legacy_site(b"fake-npm-tarball");
        assert_eq!(legacy_id, legacy_id_2);
        std::fs::write(dir.path().join("mirror/generations").join(&legacy_id).join("flatdemo"), b"{}").unwrap();
        assert!(matches!(adopt_site(dir.path()).unwrap_err(), Error::Corrupt(_)));
    }

    #[test]
    fn generation_ids_are_stable() {
        use base64::Engine;
        let zeros = "0".repeat(128);
        let raw: Vec<u8> = (0..zeros.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&zeros[i..i + 2], 16).unwrap())
            .collect();
        let integrity = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(&raw));
        let text = format!(
            "{{\"name\":\"flatdemo\",\"version\":\"1.2.3\",\"packument\":{{\"name\":\"flatdemo\",\"dist-tags\":{{\"latest\":\"1.2.3\"}}}},\"metadata\":{{\"name\":\"flatdemo\",\"version\":\"1.2.3\",\"dist\":{{\"tarball\":\"https://registry.npmjs.org/x\"}}}},\"artifact\":{{\"sha512\":\"{zeros}\",\"integrity\":\"{integrity}\",\"shasum\":null,\"sourceUrl\":\"https://registry.npmjs.org/x\",\"legacySha1Only\":false}}}}"
        );
        let value: deser_value::Value = deser_json::from_str(&text).unwrap();
        let record = validate_artifact_record(&value).unwrap();
        let files = render_packument_files(
            &[crate::inventory::artifact_to_value(&record).unwrap()],
            &[],
            "https://example.test/mirror/",
            None,
        )
        .unwrap();
        let first = generation_id_for("https://example.test/mirror/", std::slice::from_ref(&record), &files).unwrap();
        let second = generation_id_for("https://example.test/mirror/", &[record], &files).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }
}
