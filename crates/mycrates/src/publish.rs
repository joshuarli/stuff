use crate::error::{Error, Result};
use crate::fsync::{sync_dir, write_file_durable};
use crate::index::{mirror_config_json, native_config_json, serialize_index_line, shard_for_name};
use crate::model::CrateName;
use crate::store::Store;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const RENDERER_VERSION: u32 = 1;

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct PublishResult {
    pub generation_id: String,
    pub is_noop: bool,
}

pub struct Publisher<'a> {
    pub store: &'a Store,
    pub site_dir: PathBuf,
    pub public_base_url: String,
}

impl<'a> Publisher<'a> {
    pub fn new(store: &'a Store, site_dir: PathBuf, public_base_url: String) -> Self {
        Self {
            store,
            site_dir,
            public_base_url,
        }
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.site_dir.join("blobs")
    }

    pub fn generations_dir(&self) -> PathBuf {
        self.site_dir.join("generations")
    }

    pub fn current_link(&self) -> PathBuf {
        self.site_dir.join("current")
    }

    fn check_no_symlinks_in_path(&self) -> Result<()> {
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

    pub fn recover_staging(&self) -> Result<()> {
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
                }
            }
        }
        Ok(())
    }

    fn sync_blob(&self, sha256: &str) -> Result<()> {
        let src_blob = self.store.blobs_dir().join(format!("{}.crate", sha256));
        if !src_blob.exists() {
            return Err(Error::Integrity(format!("missing blob in store: {}", sha256)));
        }

        let actual_sha = stream_file_hash(&src_blob)?;
        if actual_sha != sha256 {
            return Err(Error::Integrity(format!(
                "tampered blob in store: {} (expected {}, got {})",
                src_blob.display(),
                sha256,
                actual_sha
            )));
        }

        let dst_blob = self.blobs_dir().join(format!("{}.crate", sha256));
        if dst_blob.exists() {
            let dst_sha = stream_file_hash(&dst_blob)?;
            if dst_sha == sha256 {
                return Ok(());
            }
            return Err(Error::Integrity(format!(
                "served blob mismatch for {}: destination is stale or replaced",
                sha256
            )));
        }
        fs::create_dir_all(self.blobs_dir())?;
        if fs::hard_link(&src_blob, &dst_blob).is_err() {
            let tmp = self.blobs_dir().join(format!(
                ".tmp.install.{}.{}.crate",
                std::process::id(),
                STAGING_COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            {
                let mut src = File::open(&src_blob)?;
                let mut dst = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
                let mut buf = [0u8; 65536];
                loop {
                    let n = src.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    dst.write_all(&buf[..n])?;
                }
                dst.sync_all()?;
            }
            fs::rename(&tmp, &dst_blob).map_err(|_| {
                let _ = fs::remove_file(&tmp);
                Error::Integrity(format!("served blob changed during install: {}", sha256))
            })?;
            let installed_sha = stream_file_hash(&dst_blob)?;
            if installed_sha != sha256 {
                return Err(Error::Integrity(format!(
                    "served blob changed during install: {}",
                    sha256
                )));
            }
        } else {
            let dst_sha = stream_file_hash(&dst_blob)?;
            if dst_sha != sha256 {
                return Err(Error::Integrity(format!(
                    "served blob mismatch for {}: destination is stale or replaced",
                    sha256
                )));
            }
        }
        sync_dir(&self.blobs_dir())?;
        Ok(())
    }

    fn generation_matches(&self, gen_dir: &Path, expected_manifest: &BTreeMap<(String, String), String>) -> Result<bool> {
        for ((view, rel), sha) in expected_manifest {
            let p = gen_dir.join(view).join(rel);
            let Ok(bytes) = fs::read(&p) else {
                return Ok(false);
            };
            let mut h = Sha256::new();
            h.update(&bytes);
            if format!("{:x}", h.finalize_reset()) != *sha {
                return Ok(false);
            }
        }
        for blob in self.blobs_under(gen_dir)? {
            if !self.store.blobs_dir().join(&blob).exists() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn blobs_under(&self, _gen_dir: &Path) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    pub fn publish(&self) -> Result<PublishResult> {
        self.store.recover_staging()?;
        self.recover_staging()?;
        self.check_no_symlinks_in_path()?;

        fs::create_dir_all(&self.site_dir)?;
        sync_dir(&self.site_dir)?;
        fs::create_dir_all(self.blobs_dir())?;
        sync_dir(&self.blobs_dir())?;
        fs::create_dir_all(self.generations_dir())?;
        sync_dir(&self.generations_dir())?;

        let native_records = self.store.load_native_records()?;
        let upstream_records = self.store.load_upstream_records()?;

        for rec in &native_records {
            self.sync_blob(&rec.checksum)?;
        }
        for rec in &upstream_records {
            self.sync_blob(&rec.checksum)?;
        }

        let pid = std::process::id();
        let cnt = STAGING_COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let staging_name = format!(".staging.{}.{}.{}", pid, nanos, cnt);
        let staging_dir = self.site_dir.join(&staging_name);
        fs::create_dir_all(&staging_dir)?;

        let staging_registry = staging_dir.join("registry");
        let staging_mirror = staging_dir.join("crates-io");
        fs::create_dir_all(&staging_registry)?;
        fs::create_dir_all(&staging_mirror)?;

        let mut manifest: BTreeMap<(String, String), String> = BTreeMap::new();

        let native_cfg = native_config_json(&self.public_base_url);
        let native_cfg_path = staging_registry.join("config.json");
        write_file_durable(&native_cfg_path, native_cfg.as_bytes())?;
        let mut h = Sha256::new();
        h.update(native_cfg.as_bytes());
        manifest.insert(("registry".to_string(), "config.json".to_string()), format!("{:x}", h.finalize_reset()));

        let mirror_cfg = mirror_config_json(&self.public_base_url);
        let mirror_cfg_path = staging_mirror.join("config.json");
        write_file_durable(&mirror_cfg_path, mirror_cfg.as_bytes())?;
        let mut h = Sha256::new();
        h.update(mirror_cfg.as_bytes());
        manifest.insert(("crates-io".to_string(), "config.json".to_string()), format!("{:x}", h.finalize_reset()));

        let mut native_by_crate: BTreeMap<String, Vec<&crate::store::NativeRecord>> = BTreeMap::new();
        for rec in &native_records {
            let lower = CrateName::new(&rec.name)?.lower();
            native_by_crate.entry(lower).or_default().push(rec);
        }

        for (crate_lower, mut recs) in native_by_crate {
            recs.sort_by(|a, b| {
                let va = crate::version::Version::parse(&a.vers);
                let vb = crate::version::Version::parse(&b.vers);
                match (va, vb) {
                    (Ok(a_ver), Ok(b_ver)) => a_ver.cmp(&b_ver),
                    _ => a.vers.cmp(&b.vers),
                }
            });

            let shard = shard_for_name(&crate_lower);
            let out_path = staging_registry.join(&shard);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut lines = Vec::new();
            for r in recs {
                let line = serialize_index_line(&r.entry)?;
                lines.push(line);
            }
            let content = format!("{}\n", lines.join("\n"));
            write_file_durable(&out_path, content.as_bytes())?;

            let mut h = Sha256::new();
            h.update(content.as_bytes());
            manifest.insert(("registry".to_string(), shard), format!("{:x}", h.finalize_reset()));
        }

        let mut upstream_by_crate: BTreeMap<String, Vec<&crate::store::UpstreamRecord>> = BTreeMap::new();
        for rec in &upstream_records {
            let lower = CrateName::new(&rec.name)?.lower();
            upstream_by_crate.entry(lower).or_default().push(rec);
        }

        for (crate_lower, mut recs) in upstream_by_crate {
            recs.sort_by(|a, b| {
                let va = crate::version::Version::parse(&a.vers);
                let vb = crate::version::Version::parse(&b.vers);
                match (va, vb) {
                    (Ok(a_ver), Ok(b_ver)) => a_ver.cmp(&b_ver),
                    _ => a.vers.cmp(&b.vers),
                }
            });

            let shard = shard_for_name(&crate_lower);
            let out_path = staging_mirror.join(&shard);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut lines = Vec::new();
            for r in recs {
                lines.push(r.raw_index_line.trim().to_string());
            }
            let content = format!("{}\n", lines.join("\n"));
            write_file_durable(&out_path, content.as_bytes())?;

            let mut h = Sha256::new();
            h.update(content.as_bytes());
            manifest.insert(("crates-io".to_string(), shard), format!("{:x}", h.finalize_reset()));
        }

        let mut canonical_text = format!("v:{}\n", RENDERER_VERSION);
        for ((view, path), sha) in &manifest {
            canonical_text.push_str(&format!("{}|{}|{}\n", view, path, sha));
        }
        let mut h = Sha256::new();
        h.update(canonical_text.as_bytes());
        let full_sha = format!("{:x}", h.finalize_reset());
        let gen_id = full_sha[..24].to_string();

        let target_gen_dir = self.generations_dir().join(&gen_id);

        sync_dir(&staging_dir)?;
        sync_dir(&staging_dir.join("registry"))?;
        sync_dir(&staging_dir.join("crates-io"))?;
        for entry in walk_files(&staging_dir)? {
            if let Some(parent) = entry.parent() {
                sync_dir(parent)?;
            }
        }

        if self.current_link().is_symlink()
            && let Ok(link_target) = fs::read_link(self.current_link())
        {
            let expected_rel = Path::new("generations").join(&gen_id);
            if link_target == expected_rel {
                let gen_dir = self.site_dir.join(&link_target);
                if self.generation_matches(&gen_dir, &manifest)? {
                    let _ = fs::remove_dir_all(&staging_dir);
                    sync_dir(&self.site_dir)?;
                    return Ok(PublishResult {
                        generation_id: gen_id,
                        is_noop: true,
                    });
                }
            }
        }

        let mut installed = false;
        if !target_gen_dir.exists() {
            fs::rename(&staging_dir, &target_gen_dir)?;
            sync_dir(&self.generations_dir())?;
            installed = true;
            if !self.generation_matches(&target_gen_dir, &manifest)? {
                return Err(Error::Integrity(format!(
                    "installed generation {} failed verification",
                    gen_id
                )));
            }
        } else {
            if self.generation_matches(&target_gen_dir, &manifest)? {
                let _ = fs::remove_dir_all(&staging_dir);
            } else {
                let _ = fs::remove_dir_all(&target_gen_dir);
                fs::rename(&staging_dir, &target_gen_dir)?;
                sync_dir(&self.generations_dir())?;
                installed = true;
                if !self.generation_matches(&target_gen_dir, &manifest)? {
                    return Err(Error::Integrity(format!(
                        "reinstalled generation {} failed verification",
                        gen_id
                    )));
                }
            }
        }
        let _ = installed;
        sync_dir(&self.site_dir)?;

        let tmp_link_name = format!(".current.tmp.{}.{}.{}", pid, nanos, cnt);
        let tmp_link = self.site_dir.join(&tmp_link_name);
        let rel_target = format!("generations/{}", gen_id);

        unix_fs::symlink(&rel_target, &tmp_link)?;
        fs::rename(&tmp_link, self.current_link())?;
        sync_dir(&self.site_dir)?;

        Ok(PublishResult {
            generation_id: gen_id,
            is_noop: false,
        })
    }
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

fn stream_file_hash(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
