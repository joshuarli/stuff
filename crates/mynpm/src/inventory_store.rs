#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::inventory::{create_empty_inventory, parse_inventory, serialize_inventory, Inventory};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct InventoryLock {
    _file: std::fs::File,
}

fn lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(".inventory.lock")
}

fn inventory_path(state_dir: &Path) -> PathBuf {
    state_dir.join("inventory.json")
}

pub fn acquire_inventory_lock(state_dir: &Path) -> Result<InventoryLock> {
    fs::create_dir_all(state_dir)
        .map_err(|e| Error::Operation(format!("could not prepare inventory directory {}: {e}", state_dir.display())))?;
    let path = lock_path(state_dir);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| Error::Operation(format!("could not open inventory lock at {}: {e}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(InventoryLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::Conflict(
            "another mynpm operation is updating inventory".to_string(),
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(Error::Operation(format!(
            "could not acquire inventory lock at {}: {e}",
            path.display()
        ))),
    }
}

pub fn load_inventory(state_dir: &Path) -> Result<Inventory> {
    let path = inventory_path(state_dir);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(create_empty_inventory()),
        Err(e) => {
            return Err(Error::Operation(format!(
                "could not read inventory at {}: {e}",
                path.display()
            )));
        }
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(Error::Corrupt(format!("inventory is a symbolic link: {}", path.display())));
            }
            if !meta.is_file() {
                return Err(Error::Corrupt(format!("inventory is not a regular file: {}", path.display())));
            }
        }
    }
    let contents = fs::read_to_string(&path)
        .map_err(|e| Error::Operation(format!("could not read inventory at {}: {e}", path.display())))?;
    parse_inventory(&contents)
}

fn write_inventory_file(state_dir: &Path, inv: &Inventory) -> Result<()> {
    let path = inventory_path(state_dir);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Error::Operation(format!(
                "could not inspect inventory at {}: {e}",
                path.display()
            )));
        }
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(Error::Corrupt(format!("inventory is not a regular file: {}", path.display())));
            }
        }
    }
    let contents = serialize_inventory(inv)?;
    let tmp = state_dir.join(format!(
        ".inventory.json.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let write_result = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| Error::Operation(format!("could not write inventory at {}: {e}", path.display())))?;
        f.write_all(contents.as_bytes())
            .map_err(|e| Error::Operation(format!("could not write inventory at {}: {e}", path.display())))?;
        f.sync_all()
            .map_err(|e| Error::Operation(format!("could not write inventory at {}: {e}", path.display())))?;
        drop(f);
        fs::rename(&tmp, &path)
            .map_err(|e| Error::Operation(format!("could not write inventory at {}: {e}", path.display())))?;
        stuff_store::sync_dir(state_dir)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

pub fn update_inventory(state_dir: &Path, update: impl FnOnce(&Inventory) -> Result<Inventory>) -> Result<Inventory> {
    fs::create_dir_all(state_dir)
        .map_err(|e| Error::Operation(format!("could not prepare inventory directory {}: {e}", state_dir.display())))?;
    let _lock = acquire_inventory_lock(state_dir)?;
    let current = load_inventory(state_dir)?;
    let updated = update(&current).map_err(|e| match e {
        Error::Input(_) | Error::Corrupt(_) | Error::Conflict(_) => e,
        other => Error::Operation(format!("inventory update failed: {other}")),
    })?;
    let current_text = serialize_inventory(&current)?;
    let updated_text = serialize_inventory(&updated)?;
    if current_text != updated_text {
        write_inventory_file(state_dir, &updated)?;
    }
    Ok(updated)
}

pub fn save_inventory(state_dir: &Path, inv: &Inventory) -> Result<()> {
    fs::create_dir_all(state_dir)
        .map_err(|e| Error::Operation(format!("could not prepare inventory directory {}: {e}", state_dir.display())))?;
    let _lock = acquire_inventory_lock(state_dir)?;
    write_inventory_file(state_dir, inv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::create_empty_inventory;

    #[test]
    fn roundtrip_empty_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_inventory(dir.path()).unwrap();
        assert!(loaded.projects.is_empty() && loaded.artifacts.is_empty());
        save_inventory(dir.path(), &create_empty_inventory()).unwrap();
        let text = fs::read_to_string(dir.path().join("inventory.json")).unwrap();
        assert!(text.ends_with('\n'));
        let reloaded = load_inventory(dir.path()).unwrap();
        assert!(reloaded.projects.is_empty());
    }

    #[test]
    fn rejects_symlink_inventory() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("elsewhere", dir.path().join("inventory.json")).unwrap();
        let err = load_inventory(dir.path()).unwrap_err();
        assert!(format!("{err}").contains("symbolic link"));
    }

    #[test]
    fn second_lock_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let _first = acquire_inventory_lock(dir.path()).unwrap();
        let err = acquire_inventory_lock(dir.path()).unwrap_err();
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn update_noop_skips_write() {
        let dir = tempfile::tempdir().unwrap();
        update_inventory(dir.path(), |inv| Ok(inv.clone())).unwrap();
        assert!(!dir.path().join("inventory.json").exists());
    }
}
