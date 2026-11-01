use crate::error::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn sync_dir(dir: &Path) -> Result<()> {
    let f = File::open(dir)?;
    f.sync_all()?;
    Ok(())
}

pub fn write_file_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::Operation(format!("path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let tmp = tmp_sibling(path)?;
    {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_dir(parent)?;
    Ok(())
}

pub fn tmp_sibling(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().ok_or_else(|| {
        Error::Operation(format!("path has no parent: {}", path.display()))
    })?;
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::Operation(format!("bad file name: {}", path.display())))?;
    let unique = format!(
        ".tmp.{}.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        fname
    );
    Ok(parent.join(unique))
}

pub fn sync_tree(dir: &Path) -> Result<()> {
    sync_dir(dir)?;
    for entry in walk_files(dir)? {
        if let Some(parent) = entry.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

fn walk_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        if entry.file_type()?.is_dir() {
            out.extend(walk_files(&p)?);
        } else {
            out.push(p);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn durable_write_creates_nested_parents_and_the_tree_syncs() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("a/b/file.txt");
        write_file_durable(&p, b"hello").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"hello");
        sync_tree(dir.path()).unwrap();
    }
}
