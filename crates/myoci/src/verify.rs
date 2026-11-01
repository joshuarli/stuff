#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::state::State;
use stuff_oci::{filter_referrers, parse_image, render_referrers, Digest};
use stuff_store::HashAlg;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifySummary {
    pub repositories: usize,
    pub manifests: usize,
    pub blobs: usize,
    pub referrers: usize,
}

pub fn verify_state(state: &State) -> Result<VerifySummary> {
    let mut manifest_count = 0;
    let mut referrer_count = 0;
    let mut referenced_blobs: BTreeSet<Digest> = BTreeSet::new();
    let repositories = state.repository_files()?;
    for (repository, path) in &repositories {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
        let view = stuff_oci::parse_repository_json(repository, &text)
            .map_err(|error| Error::Integrity(error.to_string()))?;
        let mut referrers: BTreeMap<Digest, Vec<stuff_oci::Descriptor>> = BTreeMap::new();
        for (digest, descriptor) in &view.manifests {
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
                    referenced_blobs.insert(blob.digest.clone());
                }
            }
            if let Some(subject) = image.subject() {
                referrers
                    .entry(subject.digest.clone())
                    .or_default()
                    .push(descriptor.clone());
            }
        }
        for (tag, descriptor) in &view.tags {
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
        for (subject, descriptors) in &referrers {
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
        let missing: Vec<String> = referenced_blobs.difference(&listed).map(|d| d.to_string()).collect();
        let extra: Vec<String> = listed.difference(&referenced_blobs).map(|d| d.to_string()).collect();
        return Err(Error::Integrity(format!(
            "blobs.json disagrees with the manifests (missing: {}; extra: {})",
            missing.join(", "),
            extra.join(", ")
        )));
    }
    for digest in &referenced_blobs {
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

#[cfg(test)]
mod tests {
    use super::*;
    use stuff_oci::{RepositoryName, Tag};

    fn fixture_state() -> (tempfile::TempDir, State) {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(dir.path().to_path_buf());
        let layout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oci/layout");
        let repository = RepositoryName::parse("example/app").unwrap();
        crate::import::import_layout(&state, &layout, &repository, Some(&Tag::parse("stable").unwrap()))
            .unwrap();
        (dir, state)
    }

    fn first_blob(state: &State) -> Digest {
        state.read_blobs().unwrap().iter().next().unwrap().clone()
    }

    fn first_manifest(state: &State) -> Digest {
        let view = state
            .read_repository(&RepositoryName::parse("example/app").unwrap())
            .unwrap()
            .unwrap();
        view.manifests.keys().next().unwrap().clone()
    }

    #[test]
    fn verifies_the_imported_fixture() {
        let (_dir, state) = fixture_state();
        let summary = verify_state(&state).unwrap();
        assert_eq!(summary.repositories, 1);
        assert_eq!(summary.manifests, 4);
        assert_eq!(summary.blobs, 6);
        assert_eq!(summary.referrers, 1);
    }

    #[test]
    fn fails_on_a_tampered_blob() {
        let (_dir, state) = fixture_state();
        let digest = first_blob(&state);
        let path = state.blob_path(&digest);
        std::fs::write(&path, b"tampered").unwrap();
        assert!(verify_state(&state).is_err());
    }

    #[test]
    fn fails_on_a_tampered_manifest() {
        let (_dir, state) = fixture_state();
        let digest = first_manifest(&state);
        let path = state.manifest_path(&digest);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(b' ');
        std::fs::write(&path, bytes).unwrap();
        assert!(verify_state(&state).is_err());
    }

    #[test]
    fn fails_on_a_tampered_tag() {
        let (_dir, state) = fixture_state();
        let repository = RepositoryName::parse("example/app").unwrap();
        let path = state.repository_json_path(&repository);
        let text = std::fs::read_to_string(&path).unwrap();
        let changed = text.replace(
            "sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388",
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        );
        std::fs::write(&path, changed).unwrap();
        assert!(verify_state(&state).is_err());
    }

    #[test]
    fn fails_on_a_tampered_referrer_index() {
        let (_dir, state) = fixture_state();
        let repository = RepositoryName::parse("example/app").unwrap();
        let subject = Digest::parse(
            "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4",
        )
        .unwrap();
        let path = state.referrers_path(&repository, &subject);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"size\":671", "\"size\":672")).unwrap();
        assert!(verify_state(&state).is_err());
    }
}
