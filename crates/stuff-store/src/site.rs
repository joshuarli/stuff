use crate::error::{Error, Result};
use crate::fsync::{sync_dir, sync_tree};
use crate::generation::parse_generation_id;
use crate::lock::{FileLock, LockError};
use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static SITE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_suffix() -> String {
    format!(
        "{}.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        SITE_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

#[derive(Debug, Clone)]
pub struct Activation {
    pub generation_id: String,
    pub is_noop: bool,
}

#[derive(Debug, Clone)]
pub struct Site {
    pub site_dir: PathBuf,
}

pub struct Staged {
    site_dir: PathBuf,
    path: PathBuf,
}

impl Staged {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn discard(self) -> Result<()> {
        let _ = fs::remove_dir_all(&self.path);
        sync_dir(&self.site_dir)?;
        Ok(())
    }

    pub fn into_path(self) -> PathBuf {
        let this = std::mem::ManuallyDrop::new(self);
        this.path.clone()
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.path.exists() || self.path.is_symlink() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

impl Site {
    pub fn new(site_dir: PathBuf) -> Self {
        Self { site_dir }
    }

    pub fn generations_dir(&self) -> PathBuf {
        self.site_dir.join("generations")
    }

    pub fn current_link(&self) -> PathBuf {
        self.site_dir.join("current")
    }

    pub fn current_generation(&self) -> Result<Option<String>> {
        let link = self.current_link();
        match fs::symlink_metadata(&link) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(Error::Operation(format!(
                    "could not inspect current link {}: {e}",
                    link.display()
                )));
            }
            Ok(meta) if !meta.file_type().is_symlink() => {
                return Err(Error::Integrity(format!(
                    "current link is not a symlink: {}",
                    link.display()
                )));
            }
            Ok(_) => {}
        }
        let target = fs::read_link(&link)?;
        let mut parts = target.components();
        let first = parts.next();
        let second = parts.next();
        match (first, second, parts.next()) {
            (Some(std::path::Component::Normal(a)), Some(std::path::Component::Normal(b)), None)
                if a == "generations" =>
            {
                let id = b.to_string_lossy();
                if parse_generation_id(&id).is_none() {
                    return Err(Error::Integrity(format!(
                        "current link has an unsafe generation target: {}",
                        target.display()
                    )));
                }
                Ok(Some(id.to_string()))
            }
            _ => Err(Error::Integrity(format!(
                "current link points outside generations: {}",
                target.display()
            ))),
        }
    }

    pub fn lock(&self, wait: Duration) -> Result<FileLock> {
        let path = self.site_dir.join(".publish.lock");
        FileLock::acquire(&path, wait).map_err(|e| match e {
            LockError::TimedOut => Error::Conflict(format!(
                "another publisher holds the publication lock at {}",
                path.display()
            )),
            LockError::Io(e) => Error::Operation(format!(
                "could not acquire the publication lock at {}: {e}",
                path.display()
            )),
        })
    }

    pub fn check_no_symlinks_in_path(&self) -> Result<()> {
        if self.site_dir.is_symlink() {
            return Err(Error::Integrity(format!(
                "site dir itself cannot be a symlink: {}",
                self.site_dir.display()
            )));
        }
        if self.site_dir.exists() {
            for entry in fs::read_dir(&self.site_dir)? {
                let entry = entry?;
                let name = entry.file_name();
                if name != "current" && entry.path().is_symlink() {
                    return Err(Error::Integrity(format!(
                        "unexpected symlink in site dir: {}",
                        entry.path().display()
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn recover_staging(&self) -> Result<u64> {
        let mut removed = 0;
        if self.site_dir.exists() {
            for entry in fs::read_dir(&self.site_dir)? {
                let entry = entry?;
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.starts_with(".staging.")
                    || fname.starts_with(".current.tmp.")
                    || fname.starts_with(".quarantine.")
                {
                    let path = entry.path();
                    let ft = fs::symlink_metadata(&path)?.file_type();
                    if ft.is_symlink() {
                        let _ = fs::remove_file(&path);
                    } else if ft.is_dir() {
                        let _ = fs::remove_dir_all(&path);
                    } else {
                        let _ = fs::remove_file(&path);
                    }
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.site_dir)?;
        sync_dir(&self.site_dir)?;
        fs::create_dir_all(self.generations_dir())?;
        sync_dir(&self.generations_dir())?;
        Ok(())
    }

    pub fn stage(&self) -> Result<Staged> {
        let path = self.site_dir.join(format!(".staging.{}", unique_suffix()));
        fs::create_dir_all(&path)?;
        Ok(Staged {
            site_dir: self.site_dir.clone(),
            path,
        })
    }

    pub fn publish(
        &self,
        generation_id: &str,
        build: &dyn Fn(&Path) -> Result<()>,
        verify: &dyn Fn(&Path) -> Result<bool>,
    ) -> Result<Activation> {
        validate_generation_id(generation_id)?;
        self.ensure_layout()?;
        let target_gen_dir = self.generations_dir().join(generation_id);
        if self.current_generation()?.as_deref() == Some(generation_id)
            && verify(&target_gen_dir)?
        {
            return Ok(Activation {
                generation_id: generation_id.to_string(),
                is_noop: true,
            });
        }
        match fs::symlink_metadata(&target_gen_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::Operation(format!(
                    "could not inspect generation {generation_id}: {e}"
                )));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(Error::Integrity(format!(
                    "generation path is not a real directory: {}",
                    target_gen_dir.display()
                )));
            }
            Ok(_) => {
                if !verify(&target_gen_dir)? {
                    return Err(immutable_generation_error(generation_id));
                }
                self.switch_current(generation_id)?;
                return Ok(Activation {
                    generation_id: generation_id.to_string(),
                    is_noop: false,
                });
            }
        }

        let staged = self.stage()?;
        build(staged.path())?;
        sync_tree(staged.path())?;
        self.activate(staged.into_path(), generation_id, verify)
    }

    pub fn activate(
        &self,
        staging: PathBuf,
        generation_id: &str,
        verify: &dyn Fn(&Path) -> Result<bool>,
    ) -> Result<Activation> {
        validate_generation_id(generation_id)?;
        if staging.parent() != Some(self.site_dir.as_path()) {
            return Err(Error::Input(format!(
                "staging dir is not directly inside site dir: {}",
                staging.display()
            )));
        }
        let is_site_staging = staging
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".staging."));
        if !is_site_staging {
            return Err(Error::Input(format!(
                "staging dir is not a site staging directory: {}",
                staging.display()
            )));
        }
        let target_gen_dir = self.generations_dir().join(generation_id);

        if self.current_generation()?.as_deref() == Some(generation_id)
            && verify(&target_gen_dir)?
        {
            let _ = fs::remove_dir_all(&staging);
            sync_dir(&self.site_dir)?;
            return Ok(Activation {
                generation_id: generation_id.to_string(),
                is_noop: true,
            });
        }

        match fs::symlink_metadata(&target_gen_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::rename(&staging, &target_gen_dir)?;
                sync_dir(&self.generations_dir())?;
                match verify(&target_gen_dir) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => {
                        let quarantined = self.quarantine(generation_id)?;
                        return Err(Error::Integrity(format!(
                            "installed generation {generation_id} failed verification; moved it to {}",
                            quarantined.display()
                        )));
                    }
                }
            }
            Err(e) => {
                return Err(Error::Operation(format!(
                    "could not inspect generation {generation_id}: {e}"
                )));
            }
            Ok(meta) if !meta.is_dir() => {
                let _ = fs::remove_dir_all(&staging);
                return Err(Error::Integrity(format!(
                    "generation path is not a real directory: {}",
                    target_gen_dir.display()
                )));
            }
            Ok(_) => {
                if !verify(&target_gen_dir)? {
                    let _ = fs::remove_dir_all(&staging);
                    return Err(immutable_generation_error(generation_id));
                }
                let _ = fs::remove_dir_all(&staging);
            }
        }
        sync_dir(&self.site_dir)?;
        self.switch_current(generation_id)?;

        Ok(Activation {
            generation_id: generation_id.to_string(),
            is_noop: false,
        })
    }

    fn quarantine(&self, generation_id: &str) -> Result<PathBuf> {
        let path = self
            .site_dir
            .join(format!(".quarantine.{generation_id}.{}", unique_suffix()));
        fs::rename(self.generations_dir().join(generation_id), &path)?;
        sync_dir(&self.generations_dir())?;
        sync_dir(&self.site_dir)?;
        Ok(path)
    }

    fn switch_current(&self, generation_id: &str) -> Result<()> {
        let current_link = self.current_link();
        let previous = match fs::read_link(&current_link) {
            Ok(target) => Some(target),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(Error::Operation(format!(
                    "could not read current link {}: {e}",
                    current_link.display()
                )));
            }
        };
        let rel_target = format!("generations/{generation_id}");
        if previous.as_deref() == Some(Path::new(&rel_target)) {
            return Ok(());
        }

        let tmp_link = self
            .site_dir
            .join(format!(".current.tmp.{}", unique_suffix()));
        unix_fs::symlink(&rel_target, &tmp_link)?;
        if let Err(e) = fs::rename(&tmp_link, &current_link) {
            let _ = fs::remove_file(&tmp_link);
            return Err(Error::Operation(format!(
                "could not switch current to generation {generation_id}: {e}"
            )));
        }
        if let Err(e) = sync_dir(&self.site_dir) {
            let rollback = self.restore_current(previous.as_deref());
            if rollback.is_err() {
                return Err(Error::Operation(format!(
                    "current switched to generation {generation_id} but durability failed and rollback was incomplete: {e}"
                )));
            }
            return Err(Error::Operation(format!(
                "could not durably switch current to generation {generation_id}: {e}"
            )));
        }
        Ok(())
    }

    fn restore_current(&self, previous: Option<&Path>) -> Result<()> {
        let current_link = self.current_link();
        match previous {
            None => {
                fs::remove_file(&current_link)?;
            }
            Some(target) => {
                let tmp_link = self
                    .site_dir
                    .join(format!(".current.tmp.{}", unique_suffix()));
                unix_fs::symlink(target, &tmp_link)?;
                if let Err(e) = fs::rename(&tmp_link, &current_link) {
                    let _ = fs::remove_file(&tmp_link);
                    return Err(Error::Operation(format!(
                        "could not restore the prior current link: {e}"
                    )));
                }
            }
        }
        sync_dir(&self.site_dir)?;
        Ok(())
    }
}

fn validate_generation_id(generation_id: &str) -> Result<()> {
    if parse_generation_id(generation_id).is_none() {
        return Err(Error::Input(format!(
            "unsafe generation id: {generation_id:?}"
        )));
    }
    Ok(())
}

fn immutable_generation_error(generation_id: &str) -> Error {
    Error::Integrity(format!(
        "generation {generation_id} already exists with different content; refusing to replace an immutable generation"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsync::write_file_durable;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn test_site() -> (tempfile::TempDir, Site) {
        let dir = tempdir().unwrap();
        let site = Site::new(dir.path().join("site"));
        site.ensure_layout().unwrap();
        (dir, site)
    }

    fn installer(files: HashMap<String, Vec<u8>>) -> impl Fn(&Path) -> Result<()> {
        move |staging: &Path| {
            for (rel, bytes) in &files {
                write_file_durable(&staging.join(rel), bytes)?;
            }
            Ok(())
        }
    }

    fn verifier(expected: HashMap<String, Vec<u8>>) -> impl Fn(&Path) -> Result<bool> {
        move |gen_dir: &Path| {
            for (rel, bytes) in &expected {
                match fs::read(gen_dir.join(rel)) {
                    Ok(actual) if &actual == bytes => {}
                    _ => return Ok(false),
                }
            }
            Ok(true)
        }
    }

    #[test]
    fn publish_switch_and_noop() {
        let (_tmp, site) = test_site();
        let files: HashMap<String, Vec<u8>> =
            [("registry/config.json".to_string(), b"{}".to_vec())].into();
        let staged = site.stage().unwrap();
        installer(files.clone())(staged.path()).unwrap();
        let staged_path = staged.into_path();
        let first = site.activate(staged_path, "gen-a", &verifier(files.clone())).unwrap();
        assert!(!first.is_noop);
        assert_eq!(site.current_generation().unwrap(), Some("gen-a".to_string()));

        let staged = site.stage().unwrap();
        let staged_path = staged.into_path();
        let noop = site.activate(staged_path, "gen-a", &verifier(files)).unwrap();
        assert!(noop.is_noop);
        for entry in fs::read_dir(&site.site_dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.starts_with(".staging"), "leftover {name}");
        }
    }

    #[test]
    fn invalid_staging_leaves_live_target_untouched() {
        let (_tmp, site) = test_site();
        let staged = site.stage().unwrap();
        let staged_path = staged.into_path();
        site.activate(staged_path, "gen-c", &|_: &Path| Ok(true)).unwrap();
        fs::write(site.generations_dir().join("gen-c/index"), b"corrupt").unwrap();

        let staged = site.stage().unwrap();
        let staged_path = staged.into_path();
        let err = site
            .activate(staged_path, "gen-c", &|_: &Path| Ok(false))
            .unwrap_err();
        assert!(matches!(err, Error::Integrity(_)));
        assert!(site.generations_dir().join("gen-c").exists());
        assert_eq!(site.current_generation().unwrap(), Some("gen-c".to_string()));
    }

    #[test]
    fn refuses_to_replace_corrupt_generation() {
        let (_tmp, site) = test_site();
        let files: HashMap<String, Vec<u8>> =
            [("index".to_string(), b"v1".to_vec())].into();
        let staged = site.stage().unwrap();
        let staged_path = staged.into_path();
        site.activate(staged_path, "gen-b", &|_: &Path| Ok(true)).unwrap();
        fs::write(site.generations_dir().join("gen-b/index"), b"tampered").unwrap();

        let staged = site.stage().unwrap();
        installer(files.clone())(staged.path()).unwrap();
        let staged_path = staged.into_path();
        let err = site.activate(staged_path, "gen-b", &verifier(files)).unwrap_err();
        assert!(matches!(err, Error::Integrity(_)));
        assert_eq!(
            fs::read(site.generations_dir().join("gen-b/index")).unwrap(),
            b"tampered"
        );
    }

    #[test]
    fn rejects_foreign_staging_dir() {
        let (_tmp, site) = test_site();
        let err = site
            .activate(PathBuf::from("/elsewhere/staging"), "gen-x", &|_: &Path| Ok(true))
            .unwrap_err();
        assert!(matches!(err, Error::Input(_)));
    }

    #[test]
    fn recovers_crash_leftovers() {
        let (_tmp, site) = test_site();
        fs::create_dir_all(site.site_dir.join(".staging.orphan")).unwrap();
        fs::write(site.site_dir.join(".staging.file"), b"x").unwrap();
        unix_fs::symlink("generations/none", site.site_dir.join(".current.tmp.1")).unwrap();
        fs::create_dir_all(site.site_dir.join(".quarantine.gen-a.1")).unwrap();
        assert_eq!(site.recover_staging().unwrap(), 4);
        site.check_no_symlinks_in_path().unwrap();
    }

    #[test]
    fn publish_builds_once_and_skips_rebuilding_a_live_generation() {
        let (_tmp, site) = test_site();
        let files: HashMap<String, Vec<u8>> = [("index".to_string(), b"v1".to_vec())].into();
        let verifier = verifier(files.clone());
        let builds = std::cell::Cell::new(0u32);
        let build = |staging: &Path| {
            builds.set(builds.get() + 1);
            installer(files.clone())(staging)
        };
        let first = site.publish("gen-a", &build, &verifier).unwrap();
        assert!(!first.is_noop);
        assert_eq!(builds.get(), 1);
        let second = site.publish("gen-a", &build, &verifier).unwrap();
        assert!(second.is_noop);
        assert_eq!(builds.get(), 1);
        assert_eq!(site.current_generation().unwrap(), Some("gen-a".to_string()));
    }

    #[test]
    fn publish_quarantines_a_generation_that_fails_verification() {
        let (_tmp, site) = test_site();
        let err = site
            .publish("gen-q", &|_: &Path| Ok(()), &|_: &Path| Ok(false))
            .unwrap_err();
        assert!(matches!(err, Error::Integrity(_)), "{err}");
        assert!(!site.generations_dir().join("gen-q").exists());
        let quarantined: Vec<String> = fs::read_dir(&site.site_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(".quarantine.gen-q."))
            .collect();
        assert_eq!(quarantined.len(), 1, "{quarantined:?}");
        assert!(site.current_generation().unwrap().is_none());

        let files: HashMap<String, Vec<u8>> = [("index".to_string(), b"v1".to_vec())].into();
        let activation = site
            .publish("gen-q", &installer(files.clone()), &verifier(files))
            .unwrap();
        assert_eq!(activation.generation_id, "gen-q");
        assert_eq!(site.recover_staging().unwrap(), 1);
    }

    #[test]
    fn publish_rejects_unsafe_generation_ids() {
        let (_tmp, site) = test_site();
        for id in ["../escape", "gen/1", "..", ".hidden"] {
            let err = site.publish(id, &|_: &Path| Ok(()), &|_: &Path| Ok(true)).unwrap_err();
            assert!(matches!(err, Error::Input(_)), "{id}: {err}");
        }
    }

    #[test]
    fn rejects_a_current_that_is_not_a_symlink() {
        let (_tmp, site) = test_site();
        fs::write(site.current_link(), b"generations/gen-a").unwrap();
        assert!(matches!(site.current_generation(), Err(Error::Integrity(_))));
        let err = site
            .publish("gen-b", &|_: &Path| Ok(()), &|_: &Path| Ok(true))
            .unwrap_err();
        assert!(matches!(err, Error::Integrity(_)), "{err}");
    }

    #[test]
    fn detects_symlink_in_site_dir() {
        let (_tmp, site) = test_site();
        unix_fs::symlink("generations", site.site_dir.join("evil")).unwrap();
        let err = site.check_no_symlinks_in_path().unwrap_err();
        assert!(matches!(err, Error::Integrity(_)));
        fs::remove_file(site.site_dir.join("evil")).unwrap();
        site.check_no_symlinks_in_path().unwrap();
        unix_fs::symlink("generations/a", site.current_link()).unwrap();
        site.check_no_symlinks_in_path().unwrap();
    }
}
