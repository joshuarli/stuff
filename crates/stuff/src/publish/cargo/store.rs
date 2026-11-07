use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use deser_value::{Kind, Map, Seq, Value};
use sha2::{Digest, Sha256};
use stuff_store::{FileLock, HashAlg, stream_file_hash, sync_dir, write_file_durable};

use super::adopt::{NativeRecord, UpstreamRecord};
use super::model::IndexEntry;
use super::semver::Version;
use super::wirejson::{compact, json_string};
use super::{Rejection, RejectionKind, input, integrity, names};

const STALE_UPLOAD_AGE: u64 = 3600;
const TEMP_PREFIX: &str = ".tmp";
const READ_CHUNK: usize = 65536;

static SUFFIX_COUNTER: AtomicU64 = AtomicU64::new(1);

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "{}.{}.{}",
        std::process::id(),
        nanos,
        SUFFIX_COUNTER.fetch_add(1, Ordering::SeqCst)
    )
}

#[derive(Debug, Clone)]
pub struct Store {
    state_dir: PathBuf,
}

impl Store {
    pub fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            state_dir: state_dir.as_ref().to_path_buf(),
        }
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    fn blobs_dir(&self) -> PathBuf {
        self.state_dir.join("blobs")
    }

    fn native_dir(&self) -> PathBuf {
        self.state_dir.join("native")
    }

    fn upstream_dir(&self) -> PathBuf {
        self.state_dir.join("upstream")
    }

    fn pins_dir(&self) -> PathBuf {
        self.state_dir.join("pins")
    }

    fn uploads_dir(&self) -> PathBuf {
        self.state_dir.join("uploads")
    }

    fn lock_path(&self) -> PathBuf {
        self.state_dir.join("lock")
    }

    pub fn blob_path(&self, checksum: &str) -> PathBuf {
        self.blobs_dir().join(format!("{checksum}.crate"))
    }

    pub fn ensure_dirs(&self) -> Result<(), Rejection> {
        for directory in [
            self.blobs_dir(),
            self.native_dir(),
            self.upstream_dir(),
            self.pins_dir(),
            self.uploads_dir(),
        ] {
            fs::create_dir_all(&directory).map_err(|error| {
                operation(format!(
                    "could not create {}: {error}",
                    directory.display()
                ))
            })?;
        }
        Ok(())
    }

    pub fn with_lock<T>(&self, body: impl FnOnce() -> Result<T, Rejection>) -> Result<T, Rejection> {
        self.ensure_dirs()?;
        let _lock = FileLock::acquire_blocking(&self.lock_path()).map_err(|error| match error {
            stuff_store::LockError::TimedOut => Rejection::new(
                RejectionKind::Conflict,
                format!("could not acquire the state lock at {}", self.lock_path().display()),
            ),
            stuff_store::LockError::Io(error) => operation(format!("lock failed: {error}")),
        })?;
        self.recover_staging()?;
        self.recover_uploads()?;
        body()
    }

    pub fn recover_staging(&self) -> Result<(), Rejection> {
        let blobs = self.blobs_dir();
        if !blobs.exists() {
            return Ok(());
        }
        for entry in read_dir(&blobs)? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(TEMP_PREFIX) {
                let path = entry.path();
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(operation(format!(
                            "could not remove {}: {error}",
                            path.display()
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn recover_uploads(&self) -> Result<(), Rejection> {
        let uploads = self.uploads_dir();
        if !uploads.exists() {
            return Ok(());
        }
        let now = SystemTime::now();
        for entry in read_dir(&uploads)? {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.file_type().is_file() {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let Ok(age) = now.duration_since(modified) else {
                continue;
            };
            if age.as_secs() >= STALE_UPLOAD_AGE {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }

    pub fn assert_current_format(&self) -> Result<(), Rejection> {
        let mut legacy = 0;
        let pins = self.pins_dir();
        if pins.exists() {
            for entry in read_dir(&pins)? {
                if entry.file_name().to_string_lossy().ends_with(".toml") {
                    legacy += 1;
                }
            }
        }
        let native = self.native_dir();
        if native.exists() {
            for crate_dir in read_dir(&native)? {
                let Ok(metadata) = fs::symlink_metadata(crate_dir.path()) else {
                    continue;
                };
                if !metadata.file_type().is_dir() {
                    continue;
                }
                for entry in read_dir(&crate_dir.path())? {
                    if entry.file_name().to_string_lossy().ends_with(".toml") {
                        legacy += 1;
                    }
                }
            }
        }
        if legacy > 0 {
            return Err(input(format!(
                "{} holds {legacy} TOML records from the Rust mycrates; run `mycrates migrate-state` once to convert them to JSON",
                self.state_dir.display()
            )));
        }
        Ok(())
    }

    pub fn ingest_blob<R: Read>(&self, mut reader: R) -> Result<String, Rejection> {
        self.ensure_dirs()?;
        let blobs = self.blobs_dir();
        let tmp = blobs.join(format!(".tmp.{}.crate", unique_suffix()));
        let mut handle = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|error| operation(format!("could not create {}: {error}", tmp.display())))?;
        let mut digest = Sha256::new();
        if let Err(error) = transfer_blob(&mut reader, &mut handle, &mut digest) {
            let _ = fs::remove_file(&tmp);
            return Err(error);
        }
        if let Err(error) = handle.flush().and_then(|()| handle.sync_all()) {
            let _ = fs::remove_file(&tmp);
            return Err(operation(format!(
                "could not sync {}: {error}",
                tmp.display()
            )));
        }
        drop(handle);
        let checksum = format!("{:x}", digest.finalize());
        let final_path = self.blob_path(&checksum);
        if final_path.exists() {
            let _ = fs::remove_file(&tmp);
            return Ok(checksum);
        }
        fs::rename(&tmp, &final_path).map_err(|error| {
            let _ = fs::remove_file(&tmp);
            operation(format!(
                "could not install {}: {error}",
                final_path.display()
            ))
        })?;
        sync_dir(&blobs).map_err(store_error)?;
        Ok(checksum)
    }

    pub fn put_native_record(&self, record: &NativeRecord) -> Result<bool, Rejection> {
        self.ensure_dirs()?;
        self.assert_current_format()?;
        let entry = &record.entry;
        names::validate_crate_name(&entry.name)?;
        let version = Version::parse(&entry.vers)?;
        self.check_name_collision(&self.native_dir(), &entry.name)?;
        let crate_dir = self.native_dir().join(names::lower_name(&entry.name));
        fs::create_dir_all(&crate_dir).map_err(|error| {
            operation(format!(
                "could not create {}: {error}",
                crate_dir.display()
            ))
        })?;
        let version_text = version.to_string();

        for existing_path in self.version_files(&crate_dir, ".json")? {
            if existing_path.file_stem().and_then(|stem| stem.to_str()) == Some(version_text.as_str())
            {
                continue;
            }
            let existing = self.read_native(&existing_path)?;
            let Ok(existing_version) = Version::parse(&existing.entry.vers) else {
                continue;
            };
            if existing_version.uniqueness_key() == version.uniqueness_key() {
                return Err(conflict(format!(
                    "version {} conflicts with existing {} (build metadata ignored for identity)",
                    entry.vers, existing.entry.vers
                )));
            }
        }

        let record_path = crate_dir.join(format!("{version_text}.json"));
        if record_path.exists() {
            let existing = self.read_native(&record_path)?;
            if existing.entry.cksum == entry.cksum {
                return Ok(false);
            }
            return Err(conflict(format!(
                "version {} already exists with different checksum",
                entry.vers
            )));
        }
        write_file_durable(&record_path, native_record_json(entry).as_bytes()).map_err(store_error)?;
        Ok(true)
    }

    pub fn put_upstream_record(&self, record: &UpstreamRecord) -> Result<bool, Rejection> {
        self.ensure_dirs()?;
        names::validate_crate_name(&record.name)?;
        let version = Version::parse(&record.vers)?;
        self.check_name_collision(&self.upstream_dir(), &record.name)?;
        let crate_dir = self.upstream_dir().join(names::lower_name(&record.name));
        fs::create_dir_all(&crate_dir).map_err(|error| {
            operation(format!(
                "could not create {}: {error}",
                crate_dir.display()
            ))
        })?;
        let version_text = version.to_string();

        for existing_path in self.version_files(&crate_dir, ".json")? {
            if existing_path.file_stem().and_then(|stem| stem.to_str()) == Some(version_text.as_str())
            {
                continue;
            }
            let existing = self.read_upstream(&existing_path)?;
            let Ok(existing_version) = Version::parse(&existing.vers) else {
                continue;
            };
            if existing_version.uniqueness_key() == version.uniqueness_key() {
                return Err(conflict(format!(
                    "upstream version {} conflicts with existing {} (build metadata ignored for identity)",
                    record.vers, existing.vers
                )));
            }
        }

        let record_path = crate_dir.join(format!("{version_text}.json"));
        if record_path.exists() {
            let existing = self.read_upstream(&record_path)?;
            if existing.checksum == record.checksum {
                return Ok(false);
            }
            return Err(conflict(format!(
                "upstream version {} already exists with different checksum",
                record.vers
            )));
        }
        write_file_durable(&record_path, upstream_record_json(record).as_bytes())
            .map_err(store_error)?;
        Ok(true)
    }

    pub fn load_native_records(&self) -> Result<Vec<NativeRecord>, Rejection> {
        self.assert_current_format()?;
        let mut records = Vec::new();
        let native = self.native_dir();
        if !native.exists() {
            return Ok(records);
        }
        for crate_dir in read_dir(&native)? {
            let Ok(metadata) = fs::symlink_metadata(crate_dir.path()) else {
                continue;
            };
            if !metadata.file_type().is_dir() {
                continue;
            }
            for path in self.version_files(&crate_dir.path(), ".json")? {
                records.push(self.read_native(&path)?);
            }
        }
        records.sort_by(|left, right| {
            (left.entry.name.as_str(), left.entry.vers.as_str())
                .cmp(&(right.entry.name.as_str(), right.entry.vers.as_str()))
        });
        Ok(records)
    }

    pub fn load_upstream_records(&self) -> Result<Vec<UpstreamRecord>, Rejection> {
        let mut records = Vec::new();
        let upstream = self.upstream_dir();
        if !upstream.exists() {
            return Ok(records);
        }
        for crate_dir in read_dir(&upstream)? {
            let Ok(metadata) = fs::symlink_metadata(crate_dir.path()) else {
                continue;
            };
            if !metadata.file_type().is_dir() {
                continue;
            }
            let directory_name = crate_dir.file_name().to_string_lossy().to_string();
            for path in self.version_files(&crate_dir.path(), ".json")? {
                let record = self.read_upstream(&path)?;
                if names::lower_name(&record.name) != directory_name {
                    return Err(integrity(format!(
                        "upstream record {} in wrong directory {directory_name}",
                        path.display()
                    )));
                }
                records.push(record);
            }
        }
        records.sort_by(|left, right| {
            (left.name.as_str(), left.vers.as_str()).cmp(&(right.name.as_str(), right.vers.as_str()))
        });
        Ok(records)
    }

    pub fn verify(&self) -> Result<(), Rejection> {
        self.with_lock(|| self.verify_locked())
    }

    pub fn verify_locked(&self) -> Result<(), Rejection> {
        let blobs = self.blobs_dir();
        if blobs.exists() {
            for entry in read_dir(&blobs)? {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.ends_with(".crate") || name.starts_with(TEMP_PREFIX) {
                    continue;
                }
                let path = entry.path();
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if !metadata.file_type().is_file() {
                    continue;
                }
                let expected = &name[..name.len() - ".crate".len()];
                let actual = stream_file_hash(&path, HashAlg::Sha256).map_err(store_error)?;
                if actual != expected {
                    return Err(integrity(format!(
                        "corrupted blob: {} (expected {expected}, got {actual})",
                        path.display()
                    )));
                }
            }
        }
        for record in self.load_native_records()? {
            let path = self.blob_path(&record.entry.cksum);
            if !path.exists() {
                return Err(integrity(format!(
                    "missing blob for native crate {} {}: {}",
                    record.entry.name,
                    record.entry.vers,
                    path.display()
                )));
            }
        }
        for record in self.load_upstream_records()? {
            let path = self.blob_path(&record.checksum);
            if !path.exists() {
                return Err(integrity(format!(
                    "missing blob for upstream crate {} {}: {}",
                    record.name,
                    record.vers,
                    path.display()
                )));
            }
        }
        Ok(())
    }

    fn version_files(&self, crate_dir: &Path, suffix: &str) -> Result<Vec<PathBuf>, Rejection> {
        if !crate_dir.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in read_dir(crate_dir)? {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(suffix) || name.starts_with(TEMP_PREFIX) {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_file() {
                files.push(path);
            }
        }
        Ok(files)
    }

    fn check_name_collision(&self, directory: &Path, name: &str) -> Result<(), Rejection> {
        if !directory.exists() {
            return Ok(());
        }
        let wanted = names::normalized_key(name);
        for entry in read_dir(directory)? {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.file_type().is_dir() {
                continue;
            }
            let directory_name = entry.file_name().to_string_lossy().to_string();
            if names::normalized_key(&directory_name) != wanted {
                continue;
            }
            match canonical_names_in(&path)? {
                None => {
                    if directory_name != names::lower_name(name) {
                        return Err(conflict(format!(
                            "crate name collision: '{name}' conflicts with existing '{directory_name}'"
                        )));
                    }
                }
                Some(canonicals) => {
                    if !canonicals.iter().any(|canonical| canonical == name) {
                        return Err(conflict(format!(
                            "crate name collision: '{name}' conflicts with existing '{}'",
                            canonicals[0]
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn read_native(&self, path: &Path) -> Result<NativeRecord, Rejection> {
        let text = fs::read_to_string(path).map_err(|error| {
            integrity(format!(
                "invalid native record {}: {error}",
                path.display()
            ))
        })?;
        let value: Value = deser_json::from_str(&text).map_err(|error| {
            integrity(format!(
                "invalid native record {}: {error}",
                path.display()
            ))
        })?;
        native_from_value(&value, path)
    }

    fn read_upstream(&self, path: &Path) -> Result<UpstreamRecord, Rejection> {
        let text = fs::read_to_string(path).map_err(|error| {
            integrity(format!(
                "invalid upstream record {}: {error}",
                path.display()
            ))
        })?;
        let value: Value = deser_json::from_str(&text).map_err(|error| {
            integrity(format!(
                "invalid upstream record {}: {error}",
                path.display()
            ))
        })?;
        upstream_from_value(&value).map_err(|error| {
            integrity(format!(
                "invalid upstream record {}: {}",
                path.display(),
                message_of(&error)
            ))
        })
    }
}

fn native_from_value(value: &Value, path: &Path) -> Result<NativeRecord, Rejection> {
    let invalid = |message: String| {
        integrity(format!(
            "invalid native record {}: {message}",
            path.display()
        ))
    };
    let map = value
        .as_map()
        .ok_or_else(|| invalid("not an object".to_string()))?;
    let entry_value = map
        .get("entry")
        .ok_or_else(|| invalid("missing entry".to_string()))?;
    let entry = IndexEntry::from_value(entry_value)
        .map_err(|error| invalid(message_of(&error)))?;
    let name = map
        .get("name")
        .ok_or_else(|| invalid("missing name".to_string()))?;
    if name.as_str() != Some(entry.name.as_str()) {
        return Err(invalid(
            "disagrees with its index entry on name".to_string(),
        ));
    }
    let vers = map
        .get("vers")
        .ok_or_else(|| invalid("missing vers".to_string()))?;
    if vers.as_str() != Some(entry.vers.as_str()) {
        return Err(invalid(
            "disagrees with its index entry on vers".to_string(),
        ));
    }
    let checksum = map
        .get("checksum")
        .ok_or_else(|| invalid("missing checksum".to_string()))?;
    if checksum.as_str() != Some(entry.cksum.as_str()) {
        return Err(invalid(
            "disagrees with its index entry on checksum".to_string(),
        ));
    }
    let yanked = map
        .get("yanked")
        .ok_or_else(|| invalid("missing yanked".to_string()))?;
    if yanked.as_bool() != Some(entry.yanked) {
        return Err(invalid(
            "disagrees with its index entry on yanked".to_string(),
        ));
    }
    Ok(NativeRecord { entry })
}

fn upstream_from_value(value: &Value) -> Result<UpstreamRecord, Rejection> {
    let map = value
        .as_map()
        .ok_or_else(|| input("upstream record must be an object"))?;
    let name = upstream_str(map, "name")?;
    let vers = upstream_str(map, "vers")?;
    let checksum = upstream_str(map, "checksum")?;
    let raw_index_line = upstream_str(map, "raw_index_line")?;
    let acquired_at = map
        .get("acquired_at")
        .ok_or_else(|| input("upstream record is missing acquired_at"))?
        .as_u64()
        .ok_or_else(|| input("upstream record has a field of the wrong type"))?;
    Ok(UpstreamRecord {
        name,
        vers,
        checksum,
        raw_index_line,
        acquired_at,
    })
}

fn upstream_str(map: &Map, key: &str) -> Result<String, Rejection> {
    let value = map
        .get(key)
        .ok_or_else(|| input(format!("upstream record is missing {key}")))?;
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| input("upstream record has a field of the wrong type"))
}

fn canonical_names_in(crate_dir: &Path) -> Result<Option<Vec<String>>, Rejection> {
    let mut names = Vec::new();
    for entry in read_dir(crate_dir)? {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(TEMP_PREFIX) || !name.ends_with(".json") {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = deser_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(canonical) = value
            .as_map()
            .and_then(|map| map.get("name"))
            .and_then(|value| value.as_str())
        {
            names.push(canonical.to_string());
        }
    }
    Ok(if names.is_empty() { None } else { Some(names) })
}

fn native_record_json(entry: &IndexEntry) -> String {
    let mut map = Map::new();
    map.insert("name", entry.name.clone());
    map.insert("vers", entry.vers.clone());
    map.insert("checksum", entry.cksum.clone());
    map.insert("yanked", entry.yanked);
    map.insert("entry", entry_value(entry));
    pretty(&Value::from(Kind::Map(map)))
}

fn upstream_record_json(record: &UpstreamRecord) -> String {
    let mut map = Map::new();
    map.insert("name", record.name.clone());
    map.insert("vers", record.vers.clone());
    map.insert("checksum", record.checksum.clone());
    map.insert("raw_index_line", record.raw_index_line.clone());
    map.insert("acquired_at", record.acquired_at);
    pretty(&Value::from(Kind::Map(map)))
}

fn entry_value(entry: &IndexEntry) -> Value {
    let mut map = Map::new();
    map.insert("name", entry.name.clone());
    map.insert("vers", entry.vers.clone());
    let deps: Vec<Value> = entry.deps.iter().map(dep_value).collect();
    map.insert("deps", Kind::Seq(Seq::from(deps)));
    map.insert("cksum", entry.cksum.clone());
    map.insert("features", features_value(entry));
    map.insert("yanked", entry.yanked);
    if let Some(links) = &entry.links {
        map.insert("links", links.clone());
    }
    if let Some(rust_version) = &entry.rust_version {
        map.insert("rust_version", rust_version.clone());
    }
    if let Some(v) = entry.v {
        map.insert("v", u64::from(v));
    }
    let mut extra: Vec<&(String, Value)> = entry.extra.iter().collect();
    extra.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, value) in extra {
        map.insert(key.clone(), value.clone());
    }
    Value::from(Kind::Map(map))
}

fn dep_value(dep: &super::model::IndexDep) -> Value {
    let mut map = Map::new();
    map.insert("name", dep.name.clone());
    map.insert("req", dep.req.clone());
    let features: Vec<Value> = dep
        .features
        .iter()
        .map(|feature| Value::from(feature.clone()))
        .collect();
    map.insert("features", Kind::Seq(Seq::from(features)));
    map.insert("optional", dep.optional);
    map.insert("default_features", dep.default_features);
    if let Some(target) = &dep.target {
        map.insert("target", target.clone());
    }
    if let Some(kind) = &dep.kind {
        map.insert("kind", kind.as_str().to_string());
    }
    map.insert(
        "registry",
        match &dep.registry {
            Some(registry) => Value::from(registry.clone()),
            None => Value::from(Kind::Null),
        },
    );
    if let Some(package) = &dep.package {
        map.insert("package", package.clone());
    }
    Value::from(Kind::Map(map))
}

fn features_value(entry: &IndexEntry) -> Value {
    let mut map = Map::new();
    for (key, values) in entry.features.iter() {
        let items: Vec<Value> = values
            .iter()
            .map(|value| Value::from(value.clone()))
            .collect();
        map.insert(key.to_string(), Kind::Seq(Seq::from(items)));
    }
    Value::from(Kind::Map(map))
}

fn pretty(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out
}

fn write_pretty(value: &Value, indent: usize, out: &mut String) {
    match value.kind() {
        Kind::Map(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, indent + 2);
                out.push_str(&json_string(key.as_str().unwrap_or_default()));
                out.push_str(": ");
                write_pretty(item, indent + 2, out);
            }
            out.push('\n');
            push_indent(out, indent);
            out.push('}');
        }
        Kind::Seq(seq) => {
            if seq.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in seq.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, indent + 2);
                write_pretty(item, indent + 2, out);
            }
            out.push('\n');
            push_indent(out, indent);
            out.push(']');
        }
        _ => out.push_str(&compact(value)),
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

fn transfer_blob<R: Read, W: Write>(
    reader: &mut R,
    handle: &mut W,
    digest: &mut Sha256,
) -> Result<(), Rejection> {
    let mut buffer = [0u8; READ_CHUNK];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| operation(format!("could not read a blob: {error}")))?;
        if read == 0 {
            return Ok(());
        }
        digest.update(&buffer[..read]);
        handle
            .write_all(&buffer[..read])
            .map_err(|error| operation(format!("could not write a blob: {error}")))?;
    }
}

fn read_dir(directory: &Path) -> Result<Vec<fs::DirEntry>, Rejection> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| {
        operation(format!(
            "could not read {}: {error}",
            directory.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            operation(format!(
                "could not read {}: {error}",
                directory.display()
            ))
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

fn message_of(rejection: &Rejection) -> String {
    let prefix = format!("{}: ", rejection.kind.prefix());
    rejection
        .detail
        .strip_prefix(&prefix)
        .unwrap_or(&rejection.detail)
        .to_string()
}

fn conflict(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Conflict, message)
}

fn operation(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Operation, message)
}

fn store_error(error: stuff_store::Error) -> Rejection {
    use stuff_store::Error;
    match error {
        Error::Input(message) => Rejection::new(RejectionKind::Input, message),
        Error::Integrity(message) => Rejection::new(RejectionKind::Integrity, message),
        Error::Conflict(message) => Rejection::new(RejectionKind::Conflict, message),
        Error::Limit(message) => Rejection::new(RejectionKind::Limit, message),
        Error::Network(message) => Rejection::new(RejectionKind::Network, message),
        Error::Operation(message) => Rejection::new(RejectionKind::Operation, message),
    }
}
