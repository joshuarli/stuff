use crate::error::{Error, Result};
use crate::fsync::sync_dir;
use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SITE_COUNTER: AtomicU64 = AtomicU64::new(1);

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
        if !link.is_symlink() {
            return Ok(None);
        }
        let target = fs::read_link(&link)?;
        let mut parts = target.components();
        let first = parts.next();
        let second = parts.next();
        match (first, second, parts.next()) {
            (Some(std::path::Component::Normal(a)), Some(std::path::Component::Normal(b)), None)
                if a == "generations" =>
            {
                Ok(Some(b.to_string_lossy().to_string()))
            }
            _ => Err(Error::Integrity(format!(
                "current link points outside generations: {}",
                target.display()
            ))),
        }
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
                if fname.starts_with(".staging") || fname.starts_with(".current.tmp") {
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
        let staging_name = format!(
            ".staging.{}.{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            SITE_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let path = self.site_dir.join(&staging_name);
        fs::create_dir_all(&path)?;
        Ok(Staged {
            site_dir: self.site_dir.clone(),
            path,
        })
    }

    pub fn activate(
        &self,
        staging: PathBuf,
        generation_id: &str,
        verify: &dyn Fn(&Path) -> Result<bool>,
    ) -> Result<Activation> {
        if staging.parent() != Some(self.site_dir.as_path()) {
            return Err(Error::Input(format!(
                "staging dir is not directly inside site dir: {}",
                staging.display()
            )));
        }
        let target_gen_dir = self.generations_dir().join(generation_id);

        if self.current_link().is_symlink()
            && let Ok(link_target) = fs::read_link(self.current_link())
        {
            let expected_rel = Path::new("generations").join(generation_id);
            if link_target == expected_rel {
                let gen_dir = self.site_dir.join(&link_target);
                if verify(&gen_dir)? {
                    let _ = fs::remove_dir_all(&staging);
                    sync_dir(&self.site_dir)?;
                    return Ok(Activation {
                        generation_id: generation_id.to_string(),
                        is_noop: true,
                    });
                }
            }
        }

        if !target_gen_dir.exists() {
            fs::rename(&staging, &target_gen_dir)?;
            sync_dir(&self.generations_dir())?;
            if !verify(&target_gen_dir)? {
                return Err(Error::Integrity(format!(
                    "installed generation {generation_id} failed verification"
                )));
            }
        } else if verify(&target_gen_dir)? {
            let _ = fs::remove_dir_all(&staging);
        } else {
            let _ = fs::remove_dir_all(&target_gen_dir);
            fs::rename(&staging, &target_gen_dir)?;
            sync_dir(&self.generations_dir())?;
            if !verify(&target_gen_dir)? {
                return Err(Error::Integrity(format!(
                    "reinstalled generation {generation_id} failed verification"
                )));
            }
        }
        sync_dir(&self.site_dir)?;

        let tmp_link_name = format!(
            ".current.tmp.{}.{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            SITE_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let tmp_link = self.site_dir.join(&tmp_link_name);
        let rel_target = format!("generations/{generation_id}");

        unix_fs::symlink(&rel_target, &tmp_link)?;
        fs::rename(&tmp_link, self.current_link())?;
        sync_dir(&self.site_dir)?;

        Ok(Activation {
            generation_id: generation_id.to_string(),
            is_noop: false,
        })
    }
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

    fn installer(files: HashMap<String, Vec<u8>>) -> impl Fn(&Path) -> Result<bool> {
        move |staging: &Path| {
            for (rel, bytes) in &files {
                write_file_durable(&staging.join(rel), bytes)?;
            }
            Ok(true)
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
    fn reinstalls_corrupt_generation() {
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
        let act = site.activate(staged_path, "gen-b", &verifier(files)).unwrap();
        assert!(!act.is_noop);
        assert_eq!(
            fs::read(site.generations_dir().join("gen-b/index")).unwrap(),
            b"v1"
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
        assert_eq!(site.recover_staging().unwrap(), 3);
        site.check_no_symlinks_in_path().unwrap();
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
