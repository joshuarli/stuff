#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use stuff_oci::{
    parse_blobs_json, parse_repository_json, render_blobs_json, render_referrers,
    render_repository_json, Descriptor, Digest, RepositoryName,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct State {
    root: PathBuf,
}

impl State {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs/sha256")
    }

    pub fn manifests_dir(&self) -> PathBuf {
        self.root.join("manifests/sha256")
    }

    pub fn repositories_dir(&self) -> PathBuf {
        self.root.join("repositories")
    }

    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.blobs_dir().join(digest.hex())
    }

    pub fn manifest_path(&self, digest: &Digest) -> PathBuf {
        self.manifests_dir().join(digest.hex())
    }

    pub fn repository_dir(&self, repository: &RepositoryName) -> PathBuf {
        let mut path = self.repositories_dir();
        for component in repository.as_str().split('/') {
            path.push(component);
        }
        path
    }

    pub fn repository_json_path(&self, repository: &RepositoryName) -> PathBuf {
        self.repository_dir(repository).join("repository.json")
    }

    pub fn referrers_dir(&self, repository: &RepositoryName) -> PathBuf {
        self.repository_dir(repository).join("referrers/sha256")
    }

    pub fn referrers_path(&self, repository: &RepositoryName, subject: &Digest) -> PathBuf {
        self.referrers_dir(repository).join(format!("{}.json", subject.hex()))
    }

    pub fn blobs_index_path(&self) -> PathBuf {
        self.root.join("blobs.json")
    }

    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(self.blobs_dir())?;
        std::fs::create_dir_all(self.manifests_dir())?;
        std::fs::create_dir_all(self.repositories_dir())?;
        let marker = self.root.join("state.json");
        let bytes = format!("{{\"schemaVersion\":{STATE_SCHEMA_VERSION}}}\n");
        match std::fs::read_to_string(&marker) {
            Ok(text) => {
                let value: deser_value::Value = deser_json::from_str(&text)
                    .map_err(|e| Error::Integrity(format!("state.json is not valid JSON: {e}")))?;
                let schema = value
                    .as_map()
                    .and_then(|map| map.get("schemaVersion"))
                    .and_then(|value| value.as_u64())
                    .ok_or_else(|| Error::Integrity("state.json has no schemaVersion".to_string()))?;
                if schema != u64::from(STATE_SCHEMA_VERSION) {
                    return Err(Error::Integrity(format!(
                        "unsupported state schemaVersion {schema}"
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                stuff_store::write_file_durable(&marker, bytes.as_bytes())?;
            }
            Err(error) => {
                return Err(Error::Operation(format!("could not read state.json: {error}")));
            }
        }
        Ok(())
    }

    pub fn store_blob(&self, source: &Path, digest: &Digest) -> Result<()> {
        let destination = self.blob_path(digest);
        if destination.exists() {
            let hash = stuff_store::stream_file_hash(&destination, stuff_store::HashAlg::Sha256)?;
            if hash != digest.hex() {
                return Err(Error::Integrity(format!(
                    "blob {digest} is immutable and hashes to sha256:{hash}"
                )));
            }
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(source)
            .map_err(|_| Error::Integrity(format!("blob {digest} is missing from {}", source.display())))?;
        if !metadata.file_type().is_file() {
            return Err(Error::Integrity(format!("blob {digest} must be a regular file")));
        }
        let hash = stuff_store::stream_file_hash(source, stuff_store::HashAlg::Sha256)?;
        if hash != digest.hex() {
            return Err(Error::Integrity(format!(
                "blob {digest} hashes to sha256:{hash} at {}",
                source.display()
            )));
        }
        std::fs::create_dir_all(self.blobs_dir())?;
        let temporary = stuff_store::tmp_sibling(&destination)?;
        std::fs::copy(source, &temporary)?;
        let file = std::fs::File::open(&temporary)?;
        file.sync_all()?;
        drop(file);
        let copied = stuff_store::stream_file_hash(&temporary, stuff_store::HashAlg::Sha256)?;
        if copied != digest.hex() {
            let _ = std::fs::remove_file(&temporary);
            return Err(Error::Integrity(format!(
                "copied blob {digest} hashes to sha256:{copied}"
            )));
        }
        std::fs::rename(&temporary, &destination)?;
        stuff_store::sync_dir(&self.blobs_dir())?;
        Ok(())
    }

    pub fn store_manifest(&self, digest: &Digest, bytes: &[u8]) -> Result<()> {
        let path = self.manifest_path(digest);
        if path.exists() {
            let existing = std::fs::read(&path)
                .map_err(|e| Error::Operation(format!("could not read {}: {e}", path.display())))?;
            if existing != bytes {
                return Err(Error::Integrity(format!(
                    "manifest {} is immutable and its bytes differ",
                    digest
                )));
            }
            return Ok(());
        }
        stuff_store::write_file_durable(&path, bytes)?;
        Ok(())
    }

    pub fn read_manifest(&self, digest: &Digest) -> Result<Vec<u8>> {
        let path = self.manifest_path(digest);
        std::fs::read(&path)
            .map_err(|e| Error::Operation(format!("could not read manifest {}: {e}", digest)))
    }

    pub fn read_repository(&self, repository: &RepositoryName) -> Result<Option<stuff_oci::RepositoryView>> {
        let path = self.repository_json_path(repository);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(Error::Operation(format!("could not read {}: {error}", path.display())));
            }
        };
        parse_repository_json(repository, &text)
            .map(Some)
            .map_err(|error| Error::Integrity(error.to_string()))
    }

    pub fn write_repository(&self, view: &stuff_oci::RepositoryView) -> Result<()> {
        let path = self.repository_json_path(&view.name);
        stuff_store::write_file_durable(&path, render_repository_json(view).as_bytes())?;
        Ok(())
    }

    pub fn write_referrers(
        &self,
        repository: &RepositoryName,
        subject: &Digest,
        descriptors: &[Descriptor],
    ) -> Result<()> {
        let path = self.referrers_path(repository, subject);
        stuff_store::write_file_durable(&path, render_referrers(descriptors).as_bytes())?;
        Ok(())
    }

    pub fn read_blobs(&self) -> Result<BTreeSet<Digest>> {
        let path = self.blobs_index_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => parse_blobs_json(&text).map_err(|error| Error::Integrity(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(error) => Err(Error::Operation(format!("could not read {}: {error}", path.display()))),
        }
    }

    pub fn write_blobs(&self, blobs: &BTreeSet<Digest>) -> Result<()> {
        stuff_store::write_file_durable(&self.blobs_index_path(), render_blobs_json(blobs).as_bytes())?;
        Ok(())
    }

    pub fn repository_files(&self) -> Result<Vec<(RepositoryName, PathBuf)>> {
        let mut out = Vec::new();
        collect_repository_files(self, &self.repositories_dir(), &mut out)?;
        out.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
        Ok(out)
    }

    pub fn set_tag(&self, repository: &RepositoryName, tag: stuff_oci::Tag, digest: &Digest) -> Result<()> {
        let mut view = self
            .read_repository(repository)?
            .ok_or_else(|| Error::Input(format!("repository {} is not in state", repository.as_str())))?;
        let descriptor = view
            .manifests
            .get(digest)
            .cloned()
            .ok_or_else(|| Error::Integrity(format!("manifest {digest} is not in the repository view")))?;
        view.tag(tag, descriptor);
        self.write_repository(&view)
    }

    pub fn remove_tag(&self, repository: &RepositoryName, tag: &stuff_oci::Tag) -> Result<bool> {
        let mut view = self
            .read_repository(repository)?
            .ok_or_else(|| Error::Input(format!("repository {} is not in state", repository.as_str())))?;
        let removed = view.remove_tag(tag).is_some();
        if removed {
            self.write_repository(&view)?;
        }
        Ok(removed)
    }

    pub fn rebuild_referrers(&self, view: &stuff_oci::RepositoryView) -> Result<usize> {
        let mut referrers: std::collections::BTreeMap<Digest, Vec<Descriptor>> = std::collections::BTreeMap::new();
        for digest in view.manifests.keys() {
            let bytes = self.read_manifest(digest)?;
            let image = stuff_oci::parse_image(&bytes)
                .map_err(|error| Error::Integrity(format!("manifest {digest}: {error}")))?;
            if let Some(subject) = image.subject() {
                referrers
                    .entry(subject.digest.clone())
                    .or_default()
                    .push(view.manifests[digest].clone());
            }
        }
        for (subject, descriptors) in &referrers {
            let sorted = stuff_oci::filter_referrers(descriptors, None);
            self.write_referrers(&view.name, subject, &sorted)?;
        }
        Ok(referrers.values().map(Vec::len).sum())
    }

    pub fn referrer_subjects(&self, repository: &RepositoryName) -> Result<Vec<Digest>> {
        let dir = self.referrers_dir(repository);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(Error::Operation(format!("could not read {}: {error}", dir.display())));
            }
        };
        let mut subjects = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|e| Error::Operation(format!("could not read {}: {e}", dir.display())))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(hex) = name.strip_suffix(".json") else { continue };
            let digest = Digest::parse(&format!("sha256:{hex}"))
                .map_err(|error| Error::Integrity(format!("referrer file {name}: {error}")))?;
            subjects.push(digest);
        }
        subjects.sort();
        Ok(subjects)
    }
}

fn collect_repository_files(
    state: &State,
    dir: &Path,
    out: &mut Vec<(RepositoryName, PathBuf)>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Error::Operation(format!("could not read {}: {error}", dir.display())));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|e| Error::Operation(format!("could not read {}: {e}", dir.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| Error::Operation(format!("could not stat {}: {e}", path.display())))?;
        if file_type.is_dir() {
            collect_repository_files(state, &path, out)?;
        } else if path.file_name().and_then(|name| name.to_str()) == Some("repository.json") {
            let parent = path
                .parent()
                .ok_or_else(|| Error::Integrity("repository.json has no parent".to_string()))?;
            let relative = parent.strip_prefix(state.repositories_dir()).map_err(|_| {
                Error::Integrity(format!("repository path escapes the state: {}", parent.display()))
            })?;
            let mut parts = Vec::new();
            for component in relative.components() {
                let text = component
                    .as_os_str()
                    .to_str()
                    .ok_or_else(|| Error::Integrity("repository path is not UTF-8".to_string()))?;
                parts.push(text);
            }
            let name = RepositoryName::parse(&parts.join("/"))
                .map_err(|e| Error::Integrity(format!("invalid repository directory: {e}")))?;
            out.push((name, path));
        }
    }
    Ok(())
}
