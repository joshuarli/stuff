#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::publish::PublishError;

pub struct Staging {
    path: PathBuf,
}

impl Staging {
    pub fn create(cache: &Path, label: &str) -> Result<Self, PublishError> {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        std::fs::create_dir_all(cache).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", cache.display()))
        })?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        for _ in 0..100 {
            let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = cache.join(format!(
                "{label}-{}-{nanos}-{counter}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Staging { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(PublishError::Config(format!(
                        "could not create {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(PublishError::Config(format!(
            "could not create a staging directory under {}",
            cache.display()
        )))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_directories_are_unique_and_removed() {
        let work = tempfile::tempdir().unwrap();
        let first = Staging::create(work.path(), "site").unwrap();
        let second = Staging::create(work.path(), "site").unwrap();
        assert_ne!(first.path(), second.path());
        let path = first.path().to_path_buf();
        drop(first);
        assert!(!path.exists());
    }
}
