use crate::error::{Error, Result};
use crate::index::shard_for_name;
use crate::model::{CrateName, IndexEntry};
use crate::store::{NativeRecord, Store, UpstreamRecord};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AdoptSummary {
    pub native: usize,
    pub upstream: usize,
    pub blobs: usize,
}

enum View {
    Native,
    Upstream,
}

pub fn adopt_site(store: &Store, site_dir: &Path) -> Result<AdoptSummary> {
    let generation = current_generation_dir(site_dir)?;
    let _guard = store.lock_exclusive()?;
    let mut summary = AdoptSummary::default();
    for (view_dir, view) in [("registry", View::Native), ("crates-io", View::Upstream)] {
        let root = generation.join(view_dir);
        for path in index_files(&root)? {
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| Error::Operation(format!("index file outside its view: {}", path.display())))?
                .to_string_lossy()
                .to_string();
            let text = fs::read_to_string(&path)?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let entry: IndexEntry = crate::json::from_str(line)?;
                let name = CrateName::new(&entry.name)?;
                if shard_for_name(&name.lower()) != relative {
                    return Err(Error::Integrity(format!(
                        "index line for {} is stored at {relative}",
                        entry.name
                    )));
                }
                summary.blobs += usize::from(adopt_blob(store, site_dir, &entry.cksum)?);
                match view {
                    View::Native => {
                        let record = NativeRecord {
                            name: entry.name.clone(),
                            vers: entry.vers.clone(),
                            checksum: entry.cksum.clone(),
                            yanked: entry.yanked,
                            entry,
                        };
                        store.put_native_record(&record)?;
                        summary.native += 1;
                    }
                    View::Upstream => {
                        let record = UpstreamRecord {
                            name: entry.name,
                            vers: entry.vers,
                            checksum: entry.cksum,
                            raw_index_line: line.trim().to_string(),
                            acquired_at: 0,
                        };
                        store.put_upstream_record(&record)?;
                        summary.upstream += 1;
                    }
                }
            }
        }
    }
    store.verify_locked()?;
    Ok(summary)
}

fn adopt_blob(store: &Store, site_dir: &Path, checksum: &str) -> Result<bool> {
    if checksum.len() != 64 || !checksum.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::Integrity(format!("index checksum is not lowercase SHA-256 hex: {checksum}")));
    }
    let present = store.blobs_dir().join(format!("{checksum}.crate")).exists();
    let source = site_dir.join("blobs").join(format!("{checksum}.crate"));
    let file = File::open(&source)
        .map_err(|e| Error::Integrity(format!("missing blob {}: {e}", source.display())))?;
    let actual = store.ingest_blob(file)?;
    if actual != checksum {
        return Err(Error::Integrity(format!(
            "blob {} hashes to {actual}, not the index checksum",
            source.display()
        )));
    }
    Ok(!present)
}

fn current_generation_dir(site_dir: &Path) -> Result<PathBuf> {
    let current = site_dir.join("current");
    let target = fs::read_link(&current)
        .map_err(|e| Error::Input(format!("{} is not a current symlink: {e}", current.display())))?;
    let id = target
        .to_str()
        .and_then(|t| t.strip_prefix("generations/"))
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        .ok_or_else(|| Error::Integrity(format!("unsafe current target: {}", target.display())))?;
    let generation = site_dir.join("generations").join(id);
    if !generation.is_dir() {
        return Err(Error::Integrity(format!("current generation is missing: {}", generation.display())));
    }
    Ok(generation)
}

fn index_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && !(dir == root && entry.file_name() == "config.json") {
                files.push(path);
            } else if !kind.is_file() {
                return Err(Error::Integrity(format!("unexpected index entry: {}", path.display())));
            }
        }
    }
    files.sort();
    Ok(files)
}
