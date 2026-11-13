#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::http::{registry_base, RegistryHttp, MANIFEST_ACCEPT};
use crate::mirror::inventory::Inventory;
use crate::mirror::lock::MirrorLock;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;
use stuff_oci::{parse_image, Descriptor, Digest, Image};

pub const MAX_MANIFEST_BYTES: u64 = stuff_oci::MAX_MANIFEST_BYTES as u64;
pub const MAX_GRAPH_DEPTH: u32 = 32;
pub const MAX_GRAPH_DESCRIPTORS: u64 = 100_000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AcquireCounts {
    pub manifests: usize,
    pub blobs_fetched: u64,
    pub bytes_fetched: u64,
    pub objects: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquiredEntry {
    pub id: String,
    pub manifests: Vec<Digest>,
    pub blobs: Vec<Digest>,
    pub blobs_fetched: u64,
    pub bytes_fetched: u64,
    pub objects: u64,
}

pub struct Acquisition {
    pub entries: Vec<AcquiredEntry>,
    pub blobs_fetched: u64,
    pub bytes_fetched: u64,
}

const MIRROR_TEMP_GRACE: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

fn sweep_stale_upload_temps(state: &crate::state::State) {
    let dir = state.uploads_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - MIRROR_TEMP_GRACE;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !(name.starts_with("mirror-") && name.ends_with(".tmp")) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn acquire(
    state: &crate::state::State,
    inventory: &Inventory,
    lock: &MirrorLock,
    http: &RegistryHttp,
    offline: bool,
) -> Result<Acquisition> {
    state.init()?;
    sweep_stale_upload_temps(state);
    let mut present_blobs = state.read_blobs()?;
    let mut entries = Vec::with_capacity(lock.entries.len());
    let mut totals = AcquireCounts::default();
    for entry in &lock.entries {
        if inventory.entry(&entry.id).is_none() {
            return Err(Error::Input(format!("lock entry {:?} is not in the inventory", entry.id)));
        }
        let base = registry_base(&source_registry(entry)?, &source_repository(entry)?);
        let Acquired { entry: acquired, .. } = acquire_entry(state, &base, &source_repository(entry)?, entry, http, offline, &mut present_blobs)?;
        totals.manifests += acquired.manifests.len();
        totals.blobs_fetched += acquired.blobs_fetched;
        totals.bytes_fetched += acquired.bytes_fetched;
        totals.objects += acquired.objects;
        entries.push(acquired);
    }
    Ok(Acquisition {
        entries,
        blobs_fetched: totals.blobs_fetched,
        bytes_fetched: totals.bytes_fetched,
    })
}

struct Acquired {
    entry: AcquiredEntry,
}

fn source_registry(entry: &crate::mirror::lock::LockedEntry) -> Result<String> {
    let source = crate::mirror::reference::RegistryReference::parse(&entry.source, "lock source")?;
    Ok(source.registry)
}

fn source_repository(entry: &crate::mirror::lock::LockedEntry) -> Result<String> {
    let source = crate::mirror::reference::RegistryReference::parse(&entry.source, "lock source")?;
    Ok(source.repository)
}

struct Work {
    descriptor: Descriptor,
    depth: u32,
}

fn acquire_entry(
    state: &crate::state::State,
    base: &str,
    repository: &str,
    entry: &crate::mirror::lock::LockedEntry,
    http: &RegistryHttp,
    offline: bool,
    present_blobs: &mut FxHashSet<Digest>,
) -> Result<Acquired> {
    let root = Descriptor {
        media_type: entry.root_media_type.clone(),
        digest: entry.root_digest.clone(),
        size: entry.root_size,
        artifact_type: None,
        annotations: rustc_hash::FxHashMap::default(),
        platform: None,
        urls: Vec::new(),
    };
    let mut queue: std::collections::VecDeque<Work> = std::collections::VecDeque::new();
    queue.push_back(Work {
        descriptor: root,
        depth: 0,
    });
    for referrer in &entry.additional_referrers {
        if !stuff_oci::is_manifest_or_index(&referrer.media_type) {
            return Err(Error::Integrity(format!(
                "entry {:?}: referrer {} has unsupported media type {}",
                entry.id, referrer.digest, referrer.media_type
            )));
        }
        queue.push_back(Work {
            descriptor: Descriptor {
                media_type: referrer.media_type.clone(),
                digest: referrer.digest.clone(),
                size: referrer.size,
                artifact_type: referrer.artifact_type.clone(),
                annotations: rustc_hash::FxHashMap::default(),
                platform: None,
                urls: Vec::new(),
            },
            depth: 0,
        });
    }
    let mut seen: FxHashSet<Digest> = FxHashSet::default();
    let mut manifests: FxHashMap<Digest, Vec<u8>> = FxHashMap::default();
    let mut required_blobs: FxHashMap<Digest, Descriptor> = FxHashMap::default();
    let mut required_manifest_digests: Vec<Digest> = Vec::new();
    let mut counts = AcquireCounts::default();
    let mut missing: Vec<String> = Vec::new();

    while let Some(work) = queue.pop_front() {
        if !seen.insert(work.descriptor.digest.clone()) {
            continue;
        }
        if counts.objects >= MAX_GRAPH_DESCRIPTORS {
            return Err(Error::Limit(format!(
                "entry {:?} exceeds the {MAX_GRAPH_DESCRIPTORS}-descriptor limit",
                entry.id
            )));
        }
        counts.objects += 1;
        if work.depth > MAX_GRAPH_DEPTH {
            return Err(Error::Limit(format!(
                "entry {:?} exceeds the graph depth limit of {MAX_GRAPH_DEPTH}",
                entry.id
            )));
        }
        let digest = work.descriptor.digest.clone();
        let local = state.manifest_path(&digest);
        let bytes = match std::fs::read(&local) {
            Ok(bytes) => {
                verify_digest_and_size(&bytes, &work.descriptor, &entry.id)?;
                bytes
            }
            Err(_) => {
                if offline {
                    missing.push(format!("manifest {digest}"));
                    continue;
                }
                fetch_manifest(http, base, repository, &work.descriptor, &entry.id)?
            }
        };
        let image = parse_image(&bytes)
            .map_err(|error| Error::Integrity(format!("manifest {digest} in entry {:?}: {error}", entry.id)))?;
        counts.manifests += 1;
        required_manifest_digests.push(digest.clone());
        manifests.insert(digest.clone(), bytes);
        match &image {
            Image::Index(index) => {
                for child in &index.manifests {
                    if stuff_oci::is_manifest_or_index(&child.media_type) {
                        queue.push_back(Work {
                            descriptor: child.clone(),
                            depth: work.depth + 1,
                        });
                    } else {
                        note_blob(&mut required_blobs, child);
                    }
                }
            }
            Image::Manifest(manifest) => {
                for blob in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
                    note_blob(&mut required_blobs, blob);
                }
            }
        }
    }

    for blob in required_blobs.values() {
        if !blob.urls.is_empty() {
            continue;
        }
        let present = present_blobs.contains(&blob.digest) && blob_path_has_size(state, &blob.digest, blob.size)?;
        if present {
            continue;
        }
        if offline {
            missing.push(format!("blob {}", blob.digest));
            continue;
        }
        std::fs::create_dir_all(state.uploads_dir())?;
        let temp = state.uploads_dir().join(format!(
            "mirror-{}-{}.tmp",
            blob.digest.hex(),
            std::process::id()
        ));
        let bytes = fetch_blob_to_file(http, base, repository, blob, &entry.id, &temp)?;
        counts.blobs_fetched += 1;
        counts.bytes_fetched += bytes;
        state.store_blob(&temp, &blob.digest)?;
        let _ = std::fs::remove_file(&temp);
        present_blobs.insert(blob.digest.clone());
    }

    if !missing.is_empty() {
        missing.sort();
        return Err(Error::Integrity(format!(
            "entry {:?} is missing local content: {}",
            entry.id,
            missing.join(", ")
        )));
    }

    // Closure succeeded; commit immutable objects only. Metadata is staged by the caller.
    let mut manifests_ordered: Vec<_> = manifests.iter().collect();
    manifests_ordered.sort_by(|a, b| a.0.cmp(b.0));
    for (digest, bytes) in manifests_ordered {
        state.store_manifest(digest, bytes)?;
    }
    required_manifest_digests.sort();
    required_manifest_digests.dedup();
    let mut blobs: Vec<Digest> = required_blobs
        .values()
        .filter(|blob| blob.urls.is_empty())
        .map(|blob| blob.digest.clone())
        .collect();
    blobs.sort();
    Ok(Acquired {
        entry: AcquiredEntry {
            id: entry.id.clone(),
            manifests: required_manifest_digests,
            blobs,
            blobs_fetched: counts.blobs_fetched,
            bytes_fetched: counts.bytes_fetched,
            objects: counts.objects,
        },
    })
}

fn note_blob(required: &mut FxHashMap<Digest, Descriptor>, descriptor: &Descriptor) {
    required.entry(descriptor.digest.clone()).or_insert_with(|| descriptor.clone());
}

fn fetch_blob_to_file(
    http: &RegistryHttp,
    base: &str,
    repository: &str,
    descriptor: &Descriptor,
    entry_id: &str,
    target: &Path,
) -> Result<u64> {
    let url = format!("{base}/blobs/{descriptor_digest}", descriptor_digest = descriptor.digest);
    let bytes = http.get_to_file(&url, descriptor.size, repository, target)?;
    if bytes != descriptor.size {
        let _ = std::fs::remove_file(target);
        return Err(Error::Integrity(format!(
            "entry {entry_id}: blob {descriptor} size is {bytes}, expected {descriptor_size}",
            descriptor = descriptor.digest,
            descriptor_size = descriptor.size
        )));
    }
    Ok(bytes)
}

fn blob_path_has_size(
    state: &crate::state::State,
    digest: &Digest,
    size: u64,
) -> Result<bool> {
    match std::fs::metadata(state.blob_path(digest)) {
        Ok(metadata) => Ok(metadata.len() == size),
        Err(_) => Ok(false),
    }
}

fn verify_digest_and_size(bytes: &[u8], descriptor: &Descriptor, entry_id: &str) -> Result<()> {
    if bytes.len() as u64 != descriptor.size {
        return Err(Error::Integrity(format!(
            "entry {entry_id}: object {} size is {}, expected {}",
            descriptor.digest,
            bytes.len(),
            descriptor.size
        )));
    }
    let actual = Digest::from_bytes(bytes);
    if actual != descriptor.digest {
        return Err(Error::Integrity(format!(
            "entry {entry_id}: object {descriptor} hashes to {actual}",
            descriptor = descriptor.digest,
        )));
    }
    Ok(())
}

fn fetch_manifest(
    http: &RegistryHttp,
    base: &str,
    repository: &str,
    descriptor: &Descriptor,
    entry_id: &str,
) -> Result<Vec<u8>> {
    let url = format!("{base}/manifests/{descriptor_digest}", descriptor_digest = descriptor.digest);
    let response = http.get_authed(&url, MANIFEST_ACCEPT, repository, MAX_MANIFEST_BYTES)?;
    match response.status {
        200 => {}
        404 => {
            return Err(Error::Integrity(format!(
                "entry {entry_id}: {url} was not found; the mirror cannot acquire what the lock records"
            )))
        }
        status => {
            return Err(Error::Network(format!(
                "entry {entry_id}: {url} returned HTTP {status}"
            )))
        }
    }
    verify_digest_and_size(&response.body, descriptor, entry_id)?;
    Ok(response.body)
}

