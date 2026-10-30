use crate::error::{Error, Result};
use crate::fsync::{sync_dir, write_file_durable};
use crate::model::{CrateName, IndexEntry, Version};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

pub struct StoreLockGuard(File);

impl Drop for StoreLockGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Clone)]
pub struct Store {
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct NativeRecord {
    pub name: String,
    pub vers: String,
    pub checksum: String,
    pub yanked: bool,
    pub entry: IndexEntry,
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct UpstreamRecord {
    pub name: String,
    pub vers: String,
    pub checksum: String,
    pub raw_index_line: String,
    pub acquired_at: u64,
}

impl Store {
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.state_dir.join("blobs")
    }

    pub fn native_dir(&self) -> PathBuf {
        self.state_dir.join("native")
    }

    pub fn upstream_dir(&self) -> PathBuf {
        self.state_dir.join("upstream")
    }

    pub fn pins_dir(&self) -> PathBuf {
        self.state_dir.join("pins")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.state_dir.join("lock")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(self.blobs_dir())?;
        fs::create_dir_all(self.native_dir())?;
        fs::create_dir_all(self.upstream_dir())?;
        fs::create_dir_all(self.pins_dir())?;
        Ok(())
    }

    pub fn lock_exclusive(&self) -> Result<StoreLockGuard> {
        self.ensure_dirs()?;
        let f = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(self.lock_path())?;
        f.lock().map_err(|e| Error::Operation(format!("store lock failed: {e}")))?;
        Ok(StoreLockGuard(f))
    }

    pub fn try_lock_exclusive(&self) -> Result<StoreLockGuard> {
        self.ensure_dirs()?;
        let f = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(self.lock_path())?;
        f.try_lock().map_err(|e| Error::Operation(format!("store try_lock failed: {e}")))?;
        Ok(StoreLockGuard(f))
    }

    pub fn recover_staging(&self) -> Result<()> {
        let blobs = self.blobs_dir();
        if blobs.exists() {
            for entry in fs::read_dir(&blobs)? {
                let entry = entry?;
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                let stale_tmp = name.starts_with(".tmp");
                let stale_upload =
                    name.starts_with(".upload.") && is_stale_upload(&entry.path(), &name);
                if stale_tmp || stale_upload {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    pub fn check_name_collision(&self, dir: &Path, name: &CrateName) -> Result<()> {
        self.check_collision_in(dir, name)
    }

    fn check_collision_in(&self, dir: &Path, name: &CrateName) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        let norm = name.norm_key();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let crate_dir = entry.path();
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let dir_norm = dir_name.to_lowercase().replace('_', "-");
            if dir_norm != norm {
                continue;
            }
            match canonical_names_in(&crate_dir)? {
                None => {
                    if dir_name != name.lower() && dir_norm == norm {
                        let existing = canonical_fallback(&crate_dir);
                        return Err(Error::Conflict(format!(
                            "crate name collision: '{}' conflicts with existing '{}'",
                            name.as_str(),
                            existing.unwrap_or(dir_name)
                        )));
                    }
                }
                Some(canonicals) => {
                    if canonicals.iter().any(|c| c == name.as_str()) {
                        continue;
                    }
                    let existing = canonicals.into_iter().next().unwrap_or(dir_name);
                    return Err(Error::Conflict(format!(
                        "crate name collision: '{}' conflicts with existing '{}'",
                        name.as_str(),
                        existing
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn ingest_staged_file(&self, path: &Path, expected_sha256: &str) -> Result<String> {
        self.ensure_dirs()?;
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
        let actual = format!("{:x}", hasher.finalize_reset());
        if actual != expected_sha256 {
            return Err(Error::Integrity(format!(
                "staged file checksum changed before ingest: expected {}, got {}",
                expected_sha256, actual
            )));
        }
        let mut src = File::open(path)?;
        self.ingest_blob(&mut src)
    }

    pub fn ingest_blob<R: Read>(&self, mut reader: R) -> Result<String> {
        self.ensure_dirs()?;
        let pid = std::process::id();
        let cnt = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_name = format!(".tmp.{}.{}.{}.crate", pid, nanos, cnt);
        let tmp_path = self.blobs_dir().join(&tmp_name);

        let mut hasher = Sha256::new();
        let mut tmp_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;

        let mut buf = [0u8; 8192];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            tmp_file.write_all(&buf[..n])?;
        }
        tmp_file.sync_all()?;
        drop(tmp_file);

        let checksum = format!("{:x}", hasher.finalize_reset());
        let final_path = self.blobs_dir().join(format!("{}.crate", checksum));

        if final_path.exists() {
            let _ = fs::remove_file(&tmp_path);
            return Ok(checksum);
        }

        fs::rename(&tmp_path, &final_path)?;
        sync_dir(&self.blobs_dir())?;
        Ok(checksum)
    }

    fn native_version_files(&self, crate_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        if !crate_dir.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(crate_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml")
                && !path.file_name().unwrap().to_string_lossy().starts_with(".tmp")
                && entry.file_type()?.is_file()
            {
                out.push(path);
            }
        }
        Ok(out)
    }

    fn upstream_version_files(&self, crate_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        if !crate_dir.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(crate_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json")
                && !path.file_name().unwrap().to_string_lossy().starts_with(".tmp")
                && entry.file_type()?.is_file()
            {
                out.push(path);
            }
        }
        Ok(out)
    }

    pub fn put_native_record(&self, record: &NativeRecord) -> Result<bool> {
        self.ensure_dirs()?;
        let name = CrateName::new(&record.name)?;
        let ver = Version::new(&record.vers)?;

        self.check_name_collision(&self.native_dir(), &name)?;

        let crate_dir = self.native_dir().join(name.lower());
        fs::create_dir_all(&crate_dir)?;

        let uniq = ver.uniqueness_key();
        for existing_path in self.native_version_files(&crate_dir)? {
            if existing_path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| stem != ver.key())
            {
                let existing_data = fs::read_to_string(&existing_path)?;
                let existing: NativeRecord = crate::toml::from_str(&existing_data)
                    .map_err(|e| Error::Integrity(format!("invalid native record {}: {e}", existing_path.display())))?;
                if let Ok(existing_ver) = Version::new(&existing.vers)
                    && existing_ver.uniqueness_key() == uniq
                {
                    return Err(Error::Conflict(format!(
                        "version {} conflicts with existing {} (build metadata ignored for identity)",
                        record.vers, existing.vers
                    )));
                }
            }
        }

        let record_path = crate_dir.join(format!("{}.toml", ver.key()));
        if record_path.exists() {
            let existing_data = fs::read_to_string(&record_path)?;
            let existing: NativeRecord = crate::toml::from_str(&existing_data)
                .map_err(|e| Error::Integrity(format!("invalid native record {}: {e}", record_path.display())))?;
            if existing.checksum == record.checksum {
                return Ok(false);
            } else {
                return Err(Error::Conflict(format!(
                    "version {} already exists with different checksum",
                    record.vers
                )));
            }
        }

        let s = crate::toml::to_string(record).map_err(|e| Error::Operation(e.to_string()))?;
        write_file_durable(&record_path, s.as_bytes())?;
        Ok(true)
    }

    pub fn set_native_yank(&self, name: &str, vers: &str, yanked: bool) -> Result<()> {
        let crate_name = CrateName::new(name)?;
        let version = Version::new(vers)?;
        let record_path = self
            .native_dir()
            .join(crate_name.lower())
            .join(format!("{}.toml", version.key()));
        if !record_path.exists() {
            return Err(Error::Input(format!("crate {} version {} not found", name, vers)));
        }
        let data = fs::read_to_string(&record_path)?;
        let mut record: NativeRecord = crate::toml::from_str(&data)?;
        record.yanked = yanked;
        record.entry.yanked = yanked;
        let s = crate::toml::to_string(&record).map_err(|e| Error::Operation(e.to_string()))?;
        write_file_durable(&record_path, s.as_bytes())?;
        Ok(())
    }

    pub fn put_upstream_record(&self, record: &UpstreamRecord) -> Result<bool> {
        self.ensure_dirs()?;
        let name = CrateName::new(&record.name)?;
        let ver = Version::new(&record.vers)?;

        self.check_name_collision(&self.upstream_dir(), &name)?;

        let crate_dir = self.upstream_dir().join(name.lower());
        fs::create_dir_all(&crate_dir)?;

        let uniq = ver.uniqueness_key();
        for existing_path in self.upstream_version_files(&crate_dir)? {
            if existing_path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|stem| stem != ver.key())
            {
                let existing_data = fs::read_to_string(&existing_path)?;
                let existing: UpstreamRecord = crate::json::from_str(&existing_data)
                    .map_err(|e| Error::Integrity(format!("invalid upstream record {}: {e}", existing_path.display())))?;
                if let Ok(existing_ver) = Version::new(&existing.vers)
                    && existing_ver.uniqueness_key() == uniq
                {
                    return Err(Error::Conflict(format!(
                        "upstream version {} conflicts with existing {} (build metadata ignored for identity)",
                        record.vers, existing.vers
                    )));
                }
            }
        }

        let record_path = crate_dir.join(format!("{}.json", ver.key()));
        if record_path.exists() {
            let existing_data = fs::read_to_string(&record_path)?;
            let existing: UpstreamRecord = crate::json::from_str(&existing_data)
                .map_err(|e| Error::Integrity(format!("invalid upstream record {}: {e}", record_path.display())))?;
            if existing.checksum == record.checksum {
                return Ok(false);
            } else {
                return Err(Error::Conflict(format!(
                    "upstream version {} already exists with different checksum",
                    record.vers
                )));
            }
        }

        let s = crate::json::to_string_pretty(record).map_err(|e| Error::Operation(e.to_string()))?;
        write_file_durable(&record_path, s.as_bytes())?;
        Ok(true)
    }

    pub fn load_native_records(&self) -> Result<Vec<NativeRecord>> {
        let mut list = Vec::new();
        let dir = self.native_dir();
        if !dir.exists() {
            return Ok(list);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for rec_entry in fs::read_dir(entry.path())? {
                    let rec_entry = rec_entry?;
                    if !rec_entry.file_type()?.is_file() {
                        continue;
                    }
                    let path = rec_entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("toml")
                        && !path.file_name().unwrap().to_string_lossy().starts_with(".tmp")
                    {
                        let s = fs::read_to_string(&path)?;
                        let rec: NativeRecord = crate::toml::from_str(&s).map_err(|e| {
                            Error::Integrity(format!("failed to parse native record {}: {e}", path.display()))
                        })?;
                        list.push(rec);
                    }
                }
            }
        }
        list.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.vers.cmp(&b.vers)));
        Ok(list)
    }

    pub fn load_upstream_records(&self) -> Result<Vec<UpstreamRecord>> {
        let mut list = Vec::new();
        let dir = self.upstream_dir();
        if !dir.exists() {
            return Ok(list);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let crate_dir_name = entry.file_name().to_string_lossy().to_string();
                for rec_entry in fs::read_dir(entry.path())? {
                    let rec_entry = rec_entry?;
                    if !rec_entry.file_type()?.is_file() {
                        continue;
                    }
                    let path = rec_entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("json")
                        && !path.file_name().unwrap().to_string_lossy().starts_with(".tmp")
                    {
                        let s = fs::read_to_string(&path)?;
                        let rec: UpstreamRecord = crate::json::from_str(&s).map_err(|e| {
                            Error::Integrity(format!("failed to parse upstream record {}: {e}", path.display()))
                        })?;
                        if rec.name.to_lowercase() != crate_dir_name {
                            return Err(Error::Integrity(format!(
                                "upstream record {} in wrong directory {}",
                                path.display(),
                                crate_dir_name
                            )));
                        }
                        list.push(rec);
                    }
                }
            }
        }
        list.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.vers.cmp(&b.vers)));
        Ok(list)
    }

    pub fn verify(&self) -> Result<()> {
        self.recover_staging()?;
        let blobs = self.blobs_dir();
        if blobs.exists() {
            for entry in fs::read_dir(&blobs)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("crate") {
                    let fname = path.file_name().unwrap().to_string_lossy();
                    if fname.starts_with(".tmp") || !entry.file_type()?.is_file() {
                        continue;
                    }
                    let expected_sha = fname.trim_end_matches(".crate");
                    let mut f = File::open(&path)?;
                    let mut hasher = Sha256::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        let n = f.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        hasher.update(&buf[..n]);
                    }
                    let actual_sha = format!("{:x}", hasher.finalize_reset());
                    if actual_sha != expected_sha {
                        return Err(Error::Integrity(format!(
                            "corrupted blob: {} (expected {}, got {})",
                            path.display(),
                            expected_sha,
                            actual_sha
                        )));
                    }
                }
            }
        }

        for rec in self.load_native_records()? {
            let blob_path = self.blobs_dir().join(format!("{}.crate", rec.checksum));
            if !blob_path.exists() {
                return Err(Error::Integrity(format!(
                    "missing blob for native crate {} {}: {}",
                    rec.name,
                    rec.vers,
                    blob_path.display()
                )));
            }
        }

        for rec in self.load_upstream_records()? {
            let blob_path = self.blobs_dir().join(format!("{}.crate", rec.checksum));
            if !blob_path.exists() {
                return Err(Error::Integrity(format!(
                    "missing blob for upstream crate {} {}: {}",
                    rec.name,
                    rec.vers,
                    blob_path.display()
                )));
            }
        }

        Ok(())
    }
}

fn canonical_names_in(crate_dir: &Path) -> Result<Option<Vec<String>>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(crate_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if ext != "toml" && ext != "json" {
            continue;
        }
        let fname = path.file_name().unwrap().to_string_lossy();
        if fname.starts_with(".tmp") {
            continue;
        }
        let data = fs::read_to_string(&path)?;
        let name_opt = if ext == "toml" {
            crate::toml::value_from_str(&data)
                .ok()
                .and_then(|v| v.get("name")?.as_str().map(|s| s.to_string()))
        } else {
            crate::json::value_from_str(&data)
                .ok()
                .and_then(|v| v.get("name")?.as_str().map(|s| s.to_string()))
        };
        if let Some(n) = name_opt {
            out.push(n);
        }
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

fn canonical_fallback(crate_dir: &Path) -> Option<String> {
    crate_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
}

fn is_stale_upload(path: &Path, name: &str) -> bool {
    let this_pid = std::process::id().to_string();
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 5 && parts[1] == "upload" {
        if parts[2] != this_pid {
            return true;
        }
        if let Ok(meta) = fs::metadata(path)
            && let Ok(mtime) = meta.modified()
            && let Ok(elapsed) = std::time::SystemTime::now().duration_since(mtime)
        {
            return elapsed.as_secs() >= 300;
        }
        return false;
    }
    true
}
