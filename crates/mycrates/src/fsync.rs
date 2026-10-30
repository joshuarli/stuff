use crate::error::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

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

pub fn tmp_sibling(path: &Path) -> Result<std::path::PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn durable_write_survives_listing() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("sub/file.txt");
        write_file_durable(&p, b"hello").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"hello");
    }
}
