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
        stuff_store::sync_immutable_file(source, &self.blob_path(digest), digest.hex(), stuff_store::HashAlg::Sha256)?;
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
