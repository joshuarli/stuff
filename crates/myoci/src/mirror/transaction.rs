#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::state::{State, MIRROR_MARKER, MIRROR_STAGED_DIR, MIRROR_TRANSACTION_DIR};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    Noop,
    DiscardedPartial,
    Reapplied,
}

pub fn recover_locked(state: &State) -> Result<Recovery> {
    let transaction = state.root().join(MIRROR_TRANSACTION_DIR);
    if !transaction.exists() {
        return Ok(Recovery::Noop);
    }
    let marker = transaction.join(MIRROR_MARKER);
    if marker.is_file() {
        let staged = transaction.join(MIRROR_STAGED_DIR);
        apply_staged(state, &staged)?;
        std::fs::remove_dir_all(&transaction).map_err(|error| {
            Error::Operation(format!(
                "could not remove {} after recovering it: {error}",
                transaction.display()
            ))
        })?;
        stuff_store::sync_dir(state.root())?;
        return Ok(Recovery::Reapplied);
    }
    std::fs::remove_dir_all(&transaction).map_err(|error| {
        Error::Operation(format!(
            "could not discard the partial mirror transaction at {}: {error}",
            transaction.display()
        ))
    })?;
    stuff_store::sync_dir(state.root())?;
    Ok(Recovery::DiscardedPartial)
}

pub fn stage_file(state: &State, relative: &Path, bytes: &[u8]) -> Result<()> {
    let target = state.root().join(MIRROR_TRANSACTION_DIR).join(MIRROR_STAGED_DIR).join(relative);
    stuff_store::write_file_durable(&target, bytes)?;
    Ok(())
}

pub fn commit_locked(state: &State) -> Result<Recovery> {
    let transaction = state.root().join(MIRROR_TRANSACTION_DIR);
    let marker = transaction.join(MIRROR_MARKER);
    let staged = transaction.join(MIRROR_STAGED_DIR);
    if !staged.is_dir() {
        return Err(Error::Operation(format!(
            "mirror transaction at {} has no staged snapshot",
            transaction.display()
        )));
    }
    if marker.exists() {
        return Err(Error::Operation(format!(
            "mirror transaction at {} is already committed; refusing to commit twice",
            transaction.display()
        )));
    }
    stuff_store::write_file_durable(&marker, b"committed\n")?;
    apply_staged(state, &staged)?;
    std::fs::remove_dir_all(&transaction).map_err(|error| {
        Error::Operation(format!(
            "could not remove {} after committing it: {error}",
            transaction.display()
        ))
    })?;
    stuff_store::sync_dir(state.root())?;
    Ok(Recovery::Reapplied)
}

fn apply_staged(state: &State, staged: &Path) -> Result<()> {
    for (relative, path) in staged_files(staged)? {
        let bytes = std::fs::read(&path).map_err(|error| {
            Error::Operation(format!("could not read staged file {}: {error}", path.display()))
        })?;
        let target = state.root().join(&relative);
        stuff_store::write_file_durable(&target, &bytes)?;
    }
    Ok(())
}

fn staged_files(staged: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut out = Vec::new();
    collect(staged, staged, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| Error::Operation(format!("could not read {}: {error}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::Operation(format!("could not read {}: {error}", dir.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| Error::Operation(format!("could not stat {}: {error}", path.display())))?;
        if file_type.is_dir() {
            collect(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|_| {
                Error::Integrity(format!("staged file escapes its root: {}", path.display()))
            })?;
            out.push((relative.to_path_buf(), path));
        } else {
            return Err(Error::Integrity(format!(
                "staged file is not a regular file: {}",
                path.display()
            )));
        }
    }
    Ok(())
}
