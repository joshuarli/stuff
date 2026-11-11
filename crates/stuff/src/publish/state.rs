#![forbid(unsafe_code)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use deser_value::Value;
use stuff_store::write_file_durable;

use crate::publish::paths::Paths;
use crate::publish::{Environment, PublishError, Registry};

pub const LEGACY_STATE_VARIABLE: &str = "STUFF_MYCRATES_STATE";
pub const BACKUP_DIRECTORY: &str = "mycrates-backup";
const LEGACY_DIRECTORIES: [&str; 4] = ["blobs", "native", "upstream", "pins"];
const COMPARE_CHUNK: usize = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoLayout {
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub site_dir: PathBuf,
    pub cargo_home: PathBuf,
}

impl CargoLayout {
    pub fn from_environment(environment: &Environment) -> Self {
        let paths = Paths::from_environment(environment);
        let cache_dir = paths.cache_dir("cargo");
        CargoLayout {
            state_dir: paths.publish_state_dir(Registry::Cargo),
            site_dir: cache_dir.join("site"),
            cargo_home: cache_dir.join("cargo-home"),
            cache_dir,
        }
    }

    pub fn prepare(&self, environment: &Environment) -> Result<MigrationReport, PublishError> {
        create_directory(&self.state_dir)?;
        create_directory(&self.cache_dir)?;
        let Some(legacy) = legacy_state_dir(environment) else {
            return Ok(MigrationReport::default());
        };
        migrate_legacy_state(&self.state_dir, &legacy)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub source: Option<PathBuf>,
    pub backup: Option<PathBuf>,
    pub blobs: usize,
    pub native: usize,
    pub upstream: usize,
    pub pins: usize,
    pub converted: usize,
}

impl MigrationReport {
    pub fn copied(&self) -> usize {
        self.blobs + self.native + self.upstream + self.pins
    }
}

pub fn legacy_state_dir(environment: &Environment) -> Option<PathBuf> {
    if let Some(value) = environment.non_empty(LEGACY_STATE_VARIABLE) {
        return Some(PathBuf::from(value));
    }
    let canonical = Paths::from_environment(environment)
        .state_dir("mycrates")
        .join("state");
    if canonical.is_dir() {
        return Some(canonical);
    }
    environment
        .non_empty("PWD")
        .map(|pwd| PathBuf::from(pwd).join(".work").join("state"))
}

pub fn migrate_legacy_state(
    target: &Path,
    legacy: &Path,
) -> Result<MigrationReport, PublishError> {
    if !legacy.is_dir() || same_directory(target, legacy) || !has_state_content(legacy) {
        return Ok(MigrationReport::default());
    }
    let backup = target.join(BACKUP_DIRECTORY);
    if !backup.exists() {
        create_directory(&backup)?;
        copy_tree(legacy, &backup)?;
    }
    let mut report = MigrationReport {
        source: Some(legacy.to_path_buf()),
        backup: Some(backup),
        ..MigrationReport::default()
    };
    merge_native(target, legacy, &mut report)?;
    merge_records(
        &legacy.join("upstream"),
        &target.join("upstream"),
        RecordKind::Upstream,
        &mut report,
    )?;
    merge_pins(target, legacy, &mut report)?;
    merge_blobs(target, legacy, &mut report)?;
    Ok(report)
}

#[derive(Clone, Copy)]
enum RecordKind {
    Native,
    Upstream,
}

fn has_state_content(legacy: &Path) -> bool {
    LEGACY_DIRECTORIES.iter().any(|name| {
        let directory = legacy.join(name);
        fs::read_dir(&directory)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false)
    })
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn create_directory(directory: &Path) -> Result<(), PublishError> {
    fs::create_dir_all(directory).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            directory.display()
        ))
    })
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), PublishError> {
    for entry in read_dir(source)? {
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let metadata = entry.metadata().map_err(|error| {
            PublishError::Config(format!("could not inspect {}: {error}", path.display()))
        })?;
        if metadata.is_dir() {
            create_directory(&destination)?;
            copy_tree(&path, &destination)?;
        } else if metadata.is_file() {
            fs::copy(&path, &destination).map_err(|error| {
                PublishError::Config(format!(
                    "could not copy {} to {}: {error}",
                    path.display(),
                    destination.display()
                ))
            })?;
        } else {
            return Err(PublishError::Integrity(format!(
                "legacy state contains a non-regular entry: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn merge_native(
    target: &Path,
    legacy: &Path,
    report: &mut MigrationReport,
) -> Result<(), PublishError> {
    merge_records(
        &legacy.join("native"),
        &target.join("native"),
        RecordKind::Native,
        report,
    )
}

fn merge_records(
    source_root: &Path,
    target_root: &Path,
    kind: RecordKind,
    report: &mut MigrationReport,
) -> Result<(), PublishError> {
    if !source_root.is_dir() {
        return Ok(());
    }
    for crate_entry in read_dir(source_root)? {
        if !crate_entry.path().is_dir() {
            continue;
        }
        let target_dir = target_root.join(crate_entry.file_name());
        for entry in read_dir(&crate_entry.path())? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.path().is_file() {
                continue;
            }
            let copied = if let Some(project) = name.strip_suffix(".toml") {
                let destination = target_dir.join(format!("{project}.json"));
                let converted = convert_toml(&entry.path(), &destination)?;
                if converted {
                    report.converted += 1;
                }
                converted
            } else if name.ends_with(".json") {
                copy_file(&entry.path(), &target_dir.join(&name))?
            } else {
                continue;
            };
            if copied {
                match kind {
                    RecordKind::Native => report.native += 1,
                    RecordKind::Upstream => report.upstream += 1,
                }
            }
        }
    }
    Ok(())
}

fn merge_pins(
    target: &Path,
    legacy: &Path,
    report: &mut MigrationReport,
) -> Result<(), PublishError> {
    let source = legacy.join("pins");
    if !source.is_dir() {
        return Ok(());
    }
    let destination = target.join("pins");
    for entry in read_dir(&source)? {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !entry.path().is_file() {
            continue;
        }
        if let Some(project) = name.strip_suffix(".toml") {
            if convert_toml(&entry.path(), &destination.join(format!("{project}.json")))? {
                report.pins += 1;
                report.converted += 1;
            }
        } else if name.ends_with(".json") && copy_file(&entry.path(), &destination.join(&name))? {
            report.pins += 1;
        }
    }
    Ok(())
}

fn merge_blobs(
    target: &Path,
    legacy: &Path,
    report: &mut MigrationReport,
) -> Result<(), PublishError> {
    let source = legacy.join("blobs");
    if !source.is_dir() {
        return Ok(());
    }
    let destination = target.join("blobs");
    for entry in read_dir(&source)? {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !name.ends_with(".crate") || !entry.path().is_file() {
            continue;
        }
        if copy_file(&entry.path(), &destination.join(&name))? {
            report.blobs += 1;
        }
    }
    Ok(())
}

fn copy_file(source: &Path, target: &Path) -> Result<bool, PublishError> {
    if target.exists() {
        if files_equal(source, target)? {
            return Ok(false);
        }
        return Err(PublishError::Conflict(format!(
            "{} already exists with different content; refusing to overwrite migrated state",
            target.display()
        )));
    }
    if let Some(parent) = target.parent() {
        create_directory(parent)?;
    }
    fs::copy(source, target).map(|_| ()).map_err(|error| {
        PublishError::Config(format!(
            "could not copy {} to {}: {error}",
            source.display(),
            target.display()
        ))
    })?;
    Ok(true)
}

fn convert_toml(source: &Path, target: &Path) -> Result<bool, PublishError> {
    if target.exists() {
        return Ok(false);
    }
    let text = fs::read_to_string(source).map_err(|error| {
        PublishError::Integrity(format!("cannot read legacy record {}: {error}", source.display()))
    })?;
    let value: Value = deser_toml::from_str(&text).map_err(|error| {
        PublishError::Integrity(format!("cannot read legacy record {}: {error}", source.display()))
    })?;
    let json = deser_json::to_string(&value).map_err(|error| {
        PublishError::Integrity(format!(
            "cannot convert legacy record {} to JSON: {error}",
            source.display()
        ))
    })?;
    if let Some(parent) = target.parent() {
        create_directory(parent)?;
    }
    write_file_durable(target, json.as_bytes()).map_err(|error| {
        PublishError::Config(format!(
            "could not write {}: {error}",
            target.display()
        ))
    })?;
    Ok(true)
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, PublishError> {
    let left_metadata = fs::metadata(left).map_err(|error| {
        PublishError::Config(format!("could not inspect {}: {error}", left.display()))
    })?;
    let right_metadata = fs::metadata(right).map_err(|error| {
        PublishError::Config(format!("could not inspect {}: {error}", right.display()))
    })?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    let mut left_file = fs::File::open(left).map_err(|error| {
        PublishError::Config(format!("could not read {}: {error}", left.display()))
    })?;
    let mut right_file = fs::File::open(right).map_err(|error| {
        PublishError::Config(format!("could not read {}: {error}", right.display()))
    })?;
    let mut left_buffer = [0u8; COMPARE_CHUNK];
    let mut right_buffer = [0u8; COMPARE_CHUNK];
    loop {
        let left_read = left_file.read(&mut left_buffer).map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", left.display()))
        })?;
        let right_read = right_file.read(&mut right_buffer).map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", right.display()))
        })?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn read_dir(directory: &Path) -> Result<Vec<fs::DirEntry>, PublishError> {
    fs::read_dir(directory)
        .map_err(|error| {
            PublishError::Config(format!(
                "could not read {}: {error}",
                directory.display()
            ))
        })?
        .map(|entry| {
            entry.map_err(|error| {
                PublishError::Config(format!(
                    "could not read {}: {error}",
                    directory.display()
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::Environment;

    fn environment(pairs: &[(&str, &str)]) -> Environment {
        Environment::from_pairs(pairs.iter().copied())
    }

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn legacy_state(root: &Path) {
        write(
            &root.join("pins/demo.json"),
            br#"{"project":"demo","registry":[{"name":"alpha","version":"0.2.0","checksum":"abc","source":"registry+crates"}]}"#,
        );
        write(
            &root.join("native/alpha/0.2.0.json"),
            br#"{"name":"alpha","vers":"0.2.0","checksum":"abc","yanked":false,"entry":{"name":"alpha","vers":"0.2.0","deps":[],"cksum":"abc","features":{},"yanked":false}}"#,
        );
        write(
            &root.join("upstream/serde/1.0.0.json"),
            br#"{"name":"serde","vers":"1.0.0","checksum":"def","raw_index_line":"{\"name\":\"serde\"}","acquired_at":123}"#,
        );
        write(&root.join("blobs/abc.crate"), b"archive");
    }

    #[test]
    fn the_cargo_layout_uses_the_registry_state_and_cache_directories() {
        let environment = environment(&[
            ("HOME", "/home/person"),
            ("STUFF_STATE_HOME", "/state"),
            ("STUFF_CACHE_HOME", "/cache"),
        ]);
        let layout = CargoLayout::from_environment(&environment);
        assert_eq!(layout.state_dir, PathBuf::from("/state/cargo"));
        assert_eq!(layout.cache_dir, PathBuf::from("/cache/cargo"));
        assert_eq!(layout.site_dir, PathBuf::from("/cache/cargo/site"));
        assert_eq!(layout.cargo_home, PathBuf::from("/cache/cargo/cargo-home"));
    }

    #[test]
    fn the_legacy_location_prefers_the_explicit_variable() {
        let explicit = environment(&[("PWD", "/repo"), ("STUFF_MYCRATES_STATE", "/legacy")]);
        assert_eq!(legacy_state_dir(&explicit), Some(PathBuf::from("/legacy")));
        let default = environment(&[("PWD", "/repo")]);
        assert_eq!(
            legacy_state_dir(&default),
            Some(PathBuf::from("/repo/.work/state"))
        );
        assert_eq!(legacy_state_dir(&Environment::default()), None);
    }

    #[test]
    fn migration_backs_up_and_copies_pins_and_provenance() {
        let work = tempfile::tempdir().unwrap();
        let legacy = work.path().join("legacy");
        legacy_state(&legacy);
        let target = work.path().join("state/cargo");
        fs::create_dir_all(&target).unwrap();

        let report = migrate_legacy_state(&target, &legacy).unwrap();
        assert_eq!(report.native, 1);
        assert_eq!(report.upstream, 1);
        assert_eq!(report.pins, 1);
        assert_eq!(report.blobs, 1);
        assert_eq!(report.converted, 0);
        assert_eq!(report.source.as_deref(), Some(legacy.as_path()));
        assert_eq!(
            report.backup.as_deref(),
            Some(target.join(BACKUP_DIRECTORY).as_path())
        );

        for relative in [
            "pins/demo.json",
            "native/alpha/0.2.0.json",
            "upstream/serde/1.0.0.json",
            "blobs/abc.crate",
        ] {
            let source = legacy.join(relative);
            let copied = target.join(relative);
            assert_eq!(fs::read(&source).unwrap(), fs::read(&copied).unwrap(), "{relative}");
            let backed_up = target.join(BACKUP_DIRECTORY).join(relative);
            assert_eq!(fs::read(&source).unwrap(), fs::read(&backed_up).unwrap(), "{relative}");
            assert!(source.is_file(), "the original {relative} must survive");
        }
    }

    #[test]
    fn a_repeated_migration_is_idempotent() {
        let work = tempfile::tempdir().unwrap();
        let legacy = work.path().join("legacy");
        legacy_state(&legacy);
        let target = work.path().join("state/cargo");
        fs::create_dir_all(&target).unwrap();

        migrate_legacy_state(&target, &legacy).unwrap();
        let before = fs::read(target.join("pins/demo.json")).unwrap();
        let second = migrate_legacy_state(&target, &legacy).unwrap();
        assert_eq!(second.copied(), 0);
        assert_eq!(second.converted, 0);
        assert_eq!(fs::read(target.join("pins/demo.json")).unwrap(), before);
    }

    #[test]
    fn conflicting_destination_files_are_refused_without_overwriting() {
        let work = tempfile::tempdir().unwrap();
        let legacy = work.path().join("legacy");
        legacy_state(&legacy);
        let target = work.path().join("state/cargo");
        write(&target.join("pins/demo.json"), b"different");
        let error = migrate_legacy_state(&target, &legacy).unwrap_err();
        assert_eq!(error.exit_code(), 3);
        assert_eq!(fs::read(target.join("pins/demo.json")).unwrap(), b"different");
    }

    #[test]
    fn legacy_toml_records_are_converted_to_json() {
        let work = tempfile::tempdir().unwrap();
        let legacy = work.path().join("legacy");
        write(
            &legacy.join("pins/demo.toml"),
            b"project = \"demo\"\nunsupported = [\"git: alpha 0.2.0\"]\n\n[[registry]]\nname = \"alpha\"\nversion = \"0.2.0\"\nchecksum = \"abc\"\nsource = \"registry+crates\"\n",
        );
        write(
            &legacy.join("native/alpha/0.2.0.toml"),
            b"name = \"alpha\"\nvers = \"0.2.0\"\nchecksum = \"abc\"\nyanked = false\n\n[entry]\nname = \"alpha\"\nvers = \"0.2.0\"\ndeps = []\ncksum = \"abc\"\nyanked = false\n\n[entry.features]\n",
        );
        let target = work.path().join("state/cargo");
        fs::create_dir_all(&target).unwrap();

        let report = migrate_legacy_state(&target, &legacy).unwrap();
        assert_eq!(report.converted, 2);
        assert_eq!(report.pins, 1);
        assert_eq!(report.native, 1);
        let pins: Value =
            deser_json::from_str(&fs::read_to_string(target.join("pins/demo.json")).unwrap()).unwrap();
        assert_eq!(
            pins.as_map()
                .and_then(|map| map.get("project"))
                .and_then(|value| value.as_str()),
            Some("demo")
        );
        let native: Value = deser_json::from_str(
            &fs::read_to_string(target.join("native/alpha/0.2.0.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            native
                .as_map()
                .and_then(|map| map.get("checksum"))
                .and_then(|value| value.as_str()),
            Some("abc")
        );
        assert!(legacy.join("pins/demo.toml").is_file());
        assert!(!target.join("pins/demo.toml").exists());
    }
}
