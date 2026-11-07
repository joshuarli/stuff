#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::adopt::{NativeRecord, Record, UpstreamRecord, View, reconstruct_index};
use super::render::{mirror_config_json, native_config_json};
use crate::publish::config::Config;
use crate::publish::r2::pull::{PART_SIZE, is_generation_id, object_etag, safe_relative};
use crate::publish::snapshot::Snapshot;
use crate::publish::PublishError;

const LIVE_INDEX_VIEWS: [View; 2] = [View::Native, View::Upstream];

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexFile {
    view: View,
    relative: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveSnapshot {
    generation: Option<String>,
    native: Vec<NativeRecord>,
    upstream: Vec<UpstreamRecord>,
    blobs: BTreeSet<String>,
    index: Vec<IndexFile>,
}

impl LiveSnapshot {
    pub fn native(&self) -> &[NativeRecord] {
        &self.native
    }

    pub fn upstream(&self) -> &[UpstreamRecord] {
        &self.upstream
    }

    pub fn has_blob(&self, checksum: &str) -> bool {
        self.blobs.contains(checksum)
    }

    pub fn seed_server_site(
        &self,
        site_dir: &Path,
        public_base_url: &str,
        auth_required: bool,
    ) -> Result<(), PublishError> {
        reset_directory(site_dir)?;
        let Some(generation) = &self.generation else {
            return Ok(());
        };
        let generation_dir = site_dir.join("generations").join(generation);
        write_file(
            &generation_dir.join("registry").join("config.json"),
            native_config_json(public_base_url, auth_required).as_bytes(),
        )?;
        write_file(
            &generation_dir.join("crates-io").join("config.json"),
            mirror_config_json(public_base_url).as_bytes(),
        )?;
        for file in &self.index {
            let relative = safe_relative(&file.relative)?;
            write_file(&generation_dir.join(view_dir(file.view)).join(relative), &file.bytes)?;
        }
        let current = site_dir.join("current");
        std::os::unix::fs::symlink(format!("generations/{generation}"), &current).map_err(
            |error| {
                PublishError::Config(format!("could not link {}: {error}", current.display()))
            },
        )
    }
}

pub fn load(snapshot: &Snapshot, config: &Config) -> Result<LiveSnapshot, PublishError> {
    let bucket = config.cargo.bucket.as_str();
    let metadata = snapshot.metadata();
    let blobs = shared_blob_checksums(snapshot)?;
    let Some(_) = metadata.get("current") else {
        return Ok(LiveSnapshot {
            blobs,
            ..LiveSnapshot::default()
        });
    };
    let marker = fetch(snapshot, bucket, "current")?;
    let generation = String::from_utf8_lossy(&marker).trim().to_string();
    if !is_generation_id(&generation) {
        return Err(PublishError::Integrity(format!(
            "bad generation id in the live current marker: {generation:?}"
        )));
    }
    let mut live = LiveSnapshot {
        generation: Some(generation.clone()),
        blobs,
        ..LiveSnapshot::default()
    };
    for view in LIVE_INDEX_VIEWS {
        let prefix = format!("generations/{generation}/{}/", view_dir(view));
        let mut keys: Vec<&String> = metadata
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .collect();
        keys.sort();
        for key in keys {
            let relative = key[prefix.len()..].to_string();
            if relative == "config.json" {
                continue;
            }
            let bytes = fetch(snapshot, bucket, key)?;
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                PublishError::Integrity(format!("{key} is not UTF-8: {error}"))
            })?;
            for line in reconstruct_index(view, &relative, text).map_err(|error| {
                PublishError::Integrity(error.detail)
            })? {
                match line.record {
                    Record::Native(record) => live.native.push(record),
                    Record::Upstream(record) => live.upstream.push(record),
                }
            }
            live.index.push(IndexFile {
                view,
                relative,
                bytes,
            });
        }
    }
    live.native.sort_by(|left, right| {
        (left.entry.name.as_str(), left.entry.vers.as_str())
            .cmp(&(right.entry.name.as_str(), right.entry.vers.as_str()))
    });
    live.upstream.sort_by(|left, right| {
        (left.name.as_str(), left.vers.as_str()).cmp(&(right.name.as_str(), right.vers.as_str()))
    });
    check_unique_native(&live.native)?;
    check_unique_upstream(&live.upstream)?;
    for record in &live.native {
        require_blob(&live.blobs, &record.entry.name, &record.entry.vers, &record.entry.cksum)?;
    }
    for record in &live.upstream {
        require_blob(&live.blobs, &record.name, &record.vers, &record.checksum)?;
    }
    Ok(live)
}

pub fn merge_native(
    local: Vec<NativeRecord>,
    live: Vec<NativeRecord>,
) -> Result<Vec<NativeRecord>, PublishError> {
    let mut merged: BTreeMap<(String, String), NativeRecord> = BTreeMap::new();
    for record in local {
        let key = (record.entry.name.clone(), record.entry.vers.clone());
        if let Some(existing) = merged.get(&key)
            && existing.entry.cksum != record.entry.cksum
        {
            return Err(PublishError::Conflict(format!(
                "{} {} exists twice in the local state with different checksums",
                record.entry.name, record.entry.vers
            )));
        }
        merged.insert(key, record);
    }
    for record in live {
        let key = (record.entry.name.clone(), record.entry.vers.clone());
        match merged.get(&key) {
            None => {
                merged.insert(key, record);
            }
            Some(existing) if existing.entry.cksum == record.entry.cksum => {}
            Some(existing) => {
                return Err(PublishError::Conflict(format!(
                    "{} {} differs between the local state ({}) and the live generation ({})",
                    record.entry.name, record.entry.vers, existing.entry.cksum, record.entry.cksum
                )));
            }
        }
    }
    Ok(merged.into_values().collect())
}

pub fn merge_upstream(
    local: Vec<UpstreamRecord>,
    live: Vec<UpstreamRecord>,
) -> Result<Vec<UpstreamRecord>, PublishError> {
    let mut merged: BTreeMap<(String, String), UpstreamRecord> = BTreeMap::new();
    for record in local {
        let key = (record.name.clone(), record.vers.clone());
        if let Some(existing) = merged.get(&key)
            && existing.checksum != record.checksum
        {
            return Err(PublishError::Conflict(format!(
                "{} {} exists twice in the local state with different checksums",
                record.name, record.vers
            )));
        }
        merged.insert(key, record);
    }
    for record in live {
        let key = (record.name.clone(), record.vers.clone());
        match merged.get(&key) {
            None => {
                merged.insert(key, record);
            }
            Some(existing) if existing.checksum == record.checksum => {}
            Some(existing) => {
                return Err(PublishError::Conflict(format!(
                    "{} {} differs between the local state ({}) and the live generation ({})",
                    record.name, record.vers, existing.checksum, record.checksum
                )));
            }
        }
    }
    Ok(merged.into_values().collect())
}

fn view_dir(view: View) -> &'static str {
    match view {
        View::Native => "registry",
        View::Upstream => "crates-io",
    }
}

fn fetch(snapshot: &Snapshot, bucket: &str, key: &str) -> Result<Vec<u8>, PublishError> {
    let bytes = snapshot.fetch(bucket, key)?;
    let listed = snapshot.metadata().get(key).ok_or_else(|| {
        PublishError::Integrity(format!("{bucket}/{key} is not in the live listing"))
    })?;
    if bytes.len() as u64 != listed.size
        || object_etag(&bytes, &listed.etag, PART_SIZE) != listed.etag
    {
        return Err(PublishError::Integrity(format!(
            "{bucket}/{key} differs from the live listing"
        )));
    }
    Ok(bytes)
}

fn shared_blob_checksums(snapshot: &Snapshot) -> Result<BTreeSet<String>, PublishError> {
    let store = snapshot
        .shared_stores()
        .first()
        .ok_or_else(|| PublishError::Config("cargo has no shared blob store".to_string()))?;
    let mut checksums = BTreeSet::new();
    for object in snapshot.shared_objects(store) {
        let checksum = object
            .key
            .strip_prefix(store.prefix)
            .and_then(|name| name.strip_suffix(".crate"))
            .filter(|name| is_sha256_hex(name))
            .ok_or_else(|| {
                PublishError::Integrity(format!("unexpected shared store key {}", object.key))
            })?;
        checksums.insert(checksum.to_string());
    }
    Ok(checksums)
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn require_blob(
    blobs: &BTreeSet<String>,
    name: &str,
    vers: &str,
    checksum: &str,
) -> Result<(), PublishError> {
    if !is_sha256_hex(checksum) {
        return Err(PublishError::Integrity(format!(
            "live index checksum for {name} {vers} is not lowercase SHA-256 hex: {checksum}"
        )));
    }
    if !blobs.contains(checksum) {
        return Err(PublishError::Integrity(format!(
            "live index for {name} {vers} references missing blob blobs/{checksum}.crate"
        )));
    }
    Ok(())
}

fn check_unique_native(records: &[NativeRecord]) -> Result<(), PublishError> {
    let mut seen: BTreeMap<(&str, &str), &str> = BTreeMap::new();
    for record in records {
        let key = (record.entry.name.as_str(), record.entry.vers.as_str());
        match seen.get(&key) {
            None => {
                seen.insert(key, record.entry.cksum.as_str());
            }
            Some(checksum) if *checksum == record.entry.cksum.as_str() => {}
            Some(_) => {
                return Err(PublishError::Integrity(format!(
                    "live index publishes {} {} twice with different checksums",
                    record.entry.name, record.entry.vers
                )));
            }
        }
    }
    Ok(())
}

fn check_unique_upstream(records: &[UpstreamRecord]) -> Result<(), PublishError> {
    let mut seen: BTreeMap<(&str, &str), &str> = BTreeMap::new();
    for record in records {
        let key = (record.name.as_str(), record.vers.as_str());
        match seen.get(&key) {
            None => {
                seen.insert(key, record.checksum.as_str());
            }
            Some(checksum) if *checksum == record.checksum.as_str() => {}
            Some(_) => {
                return Err(PublishError::Integrity(format!(
                    "live index publishes {} {} twice with different checksums",
                    record.name, record.vers
                )));
            }
        }
    }
    Ok(())
}

fn reset_directory(directory: &Path) -> Result<(), PublishError> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(directory).map_err(|error| {
                PublishError::Config(format!(
                    "could not remove {}: {error}",
                    directory.display()
                ))
            })?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(directory).map_err(|error| {
                PublishError::Config(format!(
                    "could not remove {}: {error}",
                    directory.display()
                ))
            })?;
        }
        Ok(_) => {
            fs::remove_file(directory).map_err(|error| {
                PublishError::Config(format!(
                    "could not remove {}: {error}",
                    directory.display()
                ))
            })?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(PublishError::Config(format!(
                "could not inspect {}: {error}",
                directory.display()
            )));
        }
    }
    fs::create_dir_all(directory).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            directory.display()
        ))
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", parent.display()))
        })?;
    }
    fs::write(path, bytes).map_err(|error| {
        PublishError::Config(format!("could not write {}: {error}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::cargo::model::{Features, IndexEntry};

    const CHECKSUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn native(name: &str, vers: &str, checksum: &str) -> NativeRecord {
        NativeRecord {
            entry: IndexEntry {
                name: name.to_string(),
                vers: vers.to_string(),
                deps: Vec::new(),
                cksum: checksum.to_string(),
                features: Features::empty(),
                yanked: false,
                links: None,
                rust_version: None,
                v: Some(1),
                extra: Vec::new(),
            },
        }
    }

    fn upstream(name: &str, vers: &str, checksum: &str) -> UpstreamRecord {
        UpstreamRecord {
            name: name.to_string(),
            vers: vers.to_string(),
            checksum: checksum.to_string(),
            raw_index_line: format!("{{\"name\":\"{name}\",\"vers\":\"{vers}\"}}"),
            acquired_at: 7,
        }
    }

    #[test]
    fn merging_keeps_local_records_and_fills_in_the_live_ones() {
        let merged = merge_native(
            vec![native("alpha", "0.1.0", CHECKSUM)],
            vec![
                native("alpha", "0.1.0", CHECKSUM),
                native("beta", "1.0.0", OTHER),
            ],
        )
        .unwrap();
        let names: Vec<(&str, &str)> = merged
            .iter()
            .map(|record| (record.entry.name.as_str(), record.entry.vers.as_str()))
            .collect();
        assert_eq!(names, vec![("alpha", "0.1.0"), ("beta", "1.0.0")]);
        assert_eq!(merged[0].entry.cksum, CHECKSUM);
    }

    #[test]
    fn a_local_and_live_checksum_disagreement_is_a_conflict() {
        let error = merge_native(
            vec![native("alpha", "0.1.0", CHECKSUM)],
            vec![native("alpha", "0.1.0", OTHER)],
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("alpha 0.1.0"), "{error}");
    }

    #[test]
    fn merging_upstream_keeps_local_provenance_and_rejects_conflicts() {
        let merged = merge_upstream(
            vec![upstream("serde", "1.0.0", CHECKSUM)],
            vec![
                UpstreamRecord {
                    name: "serde".to_string(),
                    vers: "1.0.0".to_string(),
                    checksum: CHECKSUM.to_string(),
                    raw_index_line: "{\"name\":\"serde\"}".to_string(),
                    acquired_at: 0,
                },
                upstream("serde_json", "1.0.0", OTHER),
            ],
        )
        .unwrap();
        assert_eq!(merged.len(), 2);
        let serde = merged
            .iter()
            .find(|record| record.name == "serde")
            .unwrap();
        assert_eq!(serde.acquired_at, 7);
        assert_eq!(serde.raw_index_line, "{\"name\":\"serde\",\"vers\":\"1.0.0\"}");
        let error = merge_upstream(
            vec![upstream("serde", "1.0.0", CHECKSUM)],
            vec![upstream("serde", "1.0.0", OTHER)],
        )
        .unwrap_err();
        assert_eq!(error.exit_code(), 3);
    }

    #[test]
    fn live_index_views_use_the_published_directory_names() {
        assert_eq!(view_dir(View::Native), "registry");
        assert_eq!(view_dir(View::Upstream), "crates-io");
    }

    #[test]
    fn shared_blob_checksums_reject_malformed_keys() {
        assert!(is_sha256_hex(CHECKSUM));
        assert!(!is_sha256_hex(&CHECKSUM[..63]));
        assert!(!is_sha256_hex(&CHECKSUM.to_uppercase()));
        assert!(!is_sha256_hex("not-hex"));
    }
}
