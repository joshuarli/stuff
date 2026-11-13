#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::state::State;
use rustc_hash::{FxHashMap, FxHashSet};
use stuff_oci::{filter_referrers, parse_image, render_referrers, Digest};
use stuff_store::HashAlg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifySummary {
    pub repositories: usize,
    pub manifests: usize,
    pub blobs: usize,
    pub referrers: usize,
}

pub fn verify_state(state: &State) -> Result<VerifySummary> {
    let _lock = state.lock()?;
    let _ = crate::mirror::transaction::recover_locked(state)?;
    verify_state_locked(state)
}

pub(crate) fn verify_state_locked(state: &State) -> Result<VerifySummary> {
    let mut manifest_count = 0;
    let mut referrer_count = 0;
    let mut referenced_blobs: FxHashSet<Digest> = FxHashSet::default();
    let repositories = state.repository_files()?;
    for (repository, path) in &repositories {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        let view = stuff_oci::parse_repository_json(repository, &text)
            .map_err(|error| Error::Integrity(error.to_string()))?;
        let mut referrers: FxHashMap<Digest, Vec<stuff_oci::Descriptor>> = FxHashMap::default();
        let mut manifests: Vec<_> = view.manifests.iter().collect();
        manifests.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (digest, descriptor) in manifests {
            manifest_count += 1;
            let bytes = state.read_manifest(digest)?;
            let actual = Digest::from_bytes(&bytes);
            if actual != *digest {
                return Err(Error::Integrity(format!("manifest {digest} hashes to {actual}")));
            }
            if bytes.len() as u64 != descriptor.size {
                return Err(Error::Integrity(format!(
                    "manifest {digest} size is {}, descriptor says {}",
                    bytes.len(),
                    descriptor.size
                )));
            }
            let image = parse_image(&bytes)
                .map_err(|error| Error::Integrity(format!("manifest {digest}: {error}")))?;
            if image.media_type() != descriptor.media_type {
                return Err(Error::Integrity(format!(
                    "manifest {digest} media type is {}, descriptor says {}",
                    image.media_type(),
                    descriptor.media_type
                )));
            }
            if let stuff_oci::Image::Index(index) = &image {
                for child in &index.manifests {
                    if !view.manifests.contains_key(&child.digest) {
                        return Err(Error::Integrity(format!(
                            "index {digest} references missing manifest {}",
                            child.digest
                        )));
                    }
                }
            }
            if let stuff_oci::Image::Manifest(manifest) = &image {
                for blob in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
                    if blob.urls.is_empty() {
                        referenced_blobs.insert(blob.digest.clone());
                    }
                }
            }
            if let Some(subject) = image.subject()
                && let Some(referrer) =
                    stuff_oci::referrer_descriptor(&image, descriptor.digest.clone(), descriptor.size)
            {
                referrers
                    .entry(subject.digest.clone())
                    .or_default()
                    .push(referrer);
            }
        }
        let mut tags: Vec<_> = view.tags.iter().collect();
        tags.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (tag, descriptor) in tags {
            let Some(stored) = view.manifests.get(&descriptor.digest) else {
                return Err(Error::Integrity(format!(
                    "tag {} points at missing manifest {}",
                    tag.as_str(),
                    descriptor.digest
                )));
            };
            if stored.size != descriptor.size || stored.media_type != descriptor.media_type {
                return Err(Error::Integrity(format!(
                    "tag {} descriptor disagrees with the stored manifest",
                    tag.as_str()
                )));
            }
        }
        let mut referrer_entries: Vec<_> = referrers.iter().collect();
        referrer_entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (subject, descriptors) in referrer_entries {
            referrer_count += descriptors.len();
            let expected = render_referrers(&filter_referrers(descriptors, None));
            let path = state.referrers_path(repository, subject);
            let actual = std::fs::read_to_string(&path).map_err(|_| {
                Error::Integrity(format!("referrers for {subject} are missing at {}", path.display()))
            })?;
            if actual != expected {
                return Err(Error::Integrity(format!(
                    "referrers for {subject} do not match the manifests"
                )));
            }
        }
        for subject in state.referrer_subjects(repository)? {
            if !referrers.contains_key(&subject) {
                return Err(Error::Integrity(format!(
                    "referrers for {subject} exist but no manifest names it as a subject"
                )));
            }
        }
    }
    let listed = state.read_blobs()?;
    if listed != referenced_blobs {
        let mut missing: Vec<String> = referenced_blobs.difference(&listed).map(|d| d.to_string()).collect();
        let mut extra: Vec<String> = listed.difference(&referenced_blobs).map(|d| d.to_string()).collect();
        missing.sort_unstable();
        extra.sort_unstable();
        return Err(Error::Integrity(format!(
            "blobs.json disagrees with the manifests (missing: {}; extra: {})",
            missing.join(", "),
            extra.join(", ")
        )));
    }
    let mut referenced: Vec<&Digest> = referenced_blobs.iter().collect();
    referenced.sort_unstable();
    for digest in referenced {
        let path = state.blob_path(digest);
        let hash = stuff_store::stream_file_hash(&path, HashAlg::Sha256).map_err(|_| {
            Error::Integrity(format!("blob {digest} is missing at {}", path.display()))
        })?;
        if hash != digest.hex() {
            return Err(Error::Integrity(format!("blob {digest} hashes to sha256:{hash}")));
        }
    }
    Ok(VerifySummary {
        repositories: repositories.len(),
        manifests: manifest_count,
        blobs: referenced_blobs.len(),
        referrers: referrer_count,
    })
}
