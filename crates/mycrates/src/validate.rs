use crate::error::{Error, Result};
use crate::lockfile::{parse_lockfile, LockPackage};
use crate::pins::Pins;
use crate::store::Store;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub struct ValidationResult {
    pub project: String,
    pub registry_count: usize,
    pub unsupported_count: usize,
    pub is_partial: bool,
}

pub fn validate_archive(state_dir: &Path, pins_path: &Path) -> Result<ValidationResult> {
    let store = Store::new(state_dir.to_path_buf());
    let content = fs::read_to_string(pins_path)?;
    let pins: Pins = crate::toml::from_str(&content)
        .map_err(|e| Error::Input(format!("failed to parse pins {}: {e}", pins_path.display())))?;

    let project_name = pins
        .project
        .clone()
        .unwrap_or_else(|| pins_path.file_stem().unwrap().to_string_lossy().to_string());

    store.verify()?;

    for pin in &pins.registry {
        let rec_path = store
            .upstream_dir()
            .join(pin.name.to_lowercase())
            .join(format!("{}.json", pin.version));
        let blob_path = store.blobs_dir().join(format!("{}.crate", pin.checksum));

        if !rec_path.exists() || !blob_path.exists() {
            return Err(Error::Integrity(format!(
                "incomplete archive: missing package {} {}",
                pin.name, pin.version
            )));
        }

        let rec_data = fs::read_to_string(&rec_path)?;
        let rec: crate::store::UpstreamRecord = crate::json::from_str(&rec_data).map_err(|e| {
            Error::Integrity(format!("invalid upstream record {}: {e}", rec_path.display()))
        })?;
        if rec.checksum != pin.checksum {
            return Err(Error::Integrity(format!(
                "archive checksum changed for {} {}: pin {} != record {}",
                pin.name, pin.version, pin.checksum, rec.checksum
            )));
        }
        if rec.vers != pin.version || rec.name.to_lowercase() != pin.name.to_lowercase() {
            return Err(Error::Integrity(format!(
                "archive record identity changed for {} {}",
                pin.name, pin.version
            )));
        }
        let entry: crate::model::IndexEntry =
            crate::json::from_str(&rec.raw_index_line).map_err(|e| {
                Error::Integrity(format!("invalid cached index line for {} {}: {e}", pin.name, pin.version))
            })?;
        if entry.cksum != pin.checksum
            || entry.name.to_lowercase() != pin.name.to_lowercase()
            || entry.vers != pin.version
        {
            return Err(Error::Integrity(format!(
                "cached index line mismatch for {} {}",
                pin.name, pin.version
            )));
        }
        if entry.yanked {
            return Err(Error::Integrity(format!(
                "archived package {} {} is yanked upstream",
                pin.name, pin.version
            )));
        }
    }

    if let Some(ref lock_str) = pins.lock_content {
        let parsed = parse_lockfile(lock_str)?;
        let locked: BTreeMap<(String, String), &LockPackage> = parsed
            .package
            .iter()
            .filter(|p| p.checksum.is_some())
            .map(|p| ((p.name.to_lowercase(), p.version.clone()), p))
            .collect();
        for pin in &pins.registry {
            match locked.get(&(pin.name.to_lowercase(), pin.version.clone())) {
                Some(pkg) => {
                    if pkg.checksum.as_deref() != Some(pin.checksum.as_str()) {
                        return Err(Error::Integrity(format!(
                            "pin checksum changed for {} {}",
                            pin.name, pin.version
                        )));
                    }
                }
                None => {
                    return Err(Error::Integrity(format!(
                        "pinned package {} {} missing from lockfile",
                        pin.name, pin.version
                    )));
                }
            }
        }
        for pkg in &parsed.package {
            if let Some(cksum) = &pkg.checksum
                && !pins.registry.iter().any(|p| {
                    p.name.to_lowercase() == pkg.name.to_lowercase()
                        && p.version == pkg.version
                        && &p.checksum == cksum
                })
                && is_crates_io_source(pkg.source.as_deref())
            {
                return Err(Error::Integrity(format!(
                    "lockfile package {} {} not in pinned archive",
                    pkg.name, pkg.version
                )));
            }
        }
    }

    Ok(ValidationResult {
        project: project_name,
        registry_count: pins.registry.len(),
        unsupported_count: pins.unsupported.len(),
        is_partial: !pins.unsupported.is_empty(),
    })
}

fn is_crates_io_source(source: Option<&str>) -> bool {
    match source {
        Some(src) => {
            src.starts_with("registry+")
                && (src.contains("index.crates.io")
                    || src == "registry+https://github.com/rust-lang/crates.io-index")
        }
        None => false,
    }
}

pub fn validate_project(
    state_dir: &Path,
    pins_path: &Path,
    mirror_index_url: &str,
    project_dir: Option<&Path>,
) -> Result<ValidationResult> {
    let archived = validate_archive(state_dir, pins_path)?;
    if archived.registry_count == 0 {
        return Ok(archived);
    }

    let content = fs::read_to_string(pins_path)?;
    let pins: Pins = crate::toml::from_str(&content)
        .map_err(|e| Error::Input(format!("failed to parse pins {}: {e}", pins_path.display())))?;

    let Some(ref lock_str) = pins.lock_content else {
        return Err(Error::Input(
            "validate: pins file has no captured Cargo.lock (re-import the lockfile)".to_string(),
        ));
    };
    let _parsed = parse_lockfile(lock_str)?;

    let Some(proj_dir) = project_dir else {
        return Err(Error::Input(
            "validate: pinned archive is intact but project manifests are required for offline verification; re-run with --project-dir <workspace> (or --manifest-path <Cargo.toml>)".to_string(),
        ));
    };
    if !proj_dir.exists() {
        return Err(Error::Input(format!(
            "validate: project directory not found: {}",
            proj_dir.display()
        )));
    }
    let manifest_path = if proj_dir.is_file() {
        proj_dir.to_path_buf()
    } else {
        proj_dir.join("Cargo.toml")
    };
    let actual_proj_dir = if manifest_path.is_file() && proj_dir.is_file() {
        manifest_path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        proj_dir.to_path_buf()
    };
    if !manifest_path.exists() {
        return Err(Error::Input(format!(
            "validate: manifest not found: {} (supply --project-dir pointing at the workspace root)",
            manifest_path.display()
        )));
    }
    let proj_lock_path = actual_proj_dir.join("Cargo.lock");
    if !proj_lock_path.exists() {
        return Err(Error::Input(format!(
            "validate: project has no Cargo.lock at {} (generate it with cargo before validating)",
            proj_lock_path.display()
        )));
    }
    let proj_lock = fs::read_to_string(&proj_lock_path)?;
    if proj_lock != *lock_str {
        return Err(Error::Integrity(
            "validate: project Cargo.lock differs from imported pins lock_content (re-run import-lock)".to_string(),
        ));
    }

    let val_tmp = state_dir.join(format!(
        ".validate_tmp_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&val_tmp);
    fs::create_dir_all(&val_tmp)?;
    let val_tmp = val_tmp.canonicalize()?;
    let cleanup = ValidateCleanup(val_tmp.clone());
    let test_proj = val_tmp.join("proj");
    copy_project(&actual_proj_dir, &test_proj)?;

    let cargo_home = val_tmp.join("cargo_home");
    fs::create_dir_all(&cargo_home)?;
    let cargo_config = format!(
        "[source.crates-io]\nreplace-with = \"mycrates-mirror\"\n\n[source.mycrates-mirror]\nregistry = \"sparse+{}\"\n",
        mirror_index_url
    );
    fs::write(cargo_home.join("config.toml"), cargo_config)?;

    let mut cmd = Command::new("cargo");
    cmd.current_dir(&test_proj);
    cmd.env("CARGO_HOME", &cargo_home);
    cmd.env("CARGO_TERM_COLOR", "never");
    cmd.env("CARGO_HTTP_PROXY", "http://127.0.0.1:1");
    cmd.env("HTTPS_PROXY", "http://127.0.0.1:1");
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.args(["metadata", "--locked", "--format-version", "1"]);
    let out = cmd.output().map_err(|e| Error::Operation(format!("failed to run cargo metadata: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Operation(format!("cargo metadata --locked failed: {stderr}")));
    }

    let mut cmd_fetch = Command::new("cargo");
    cmd_fetch.current_dir(&test_proj);
    cmd_fetch.env("CARGO_HOME", &cargo_home);
    cmd_fetch.env("CARGO_TERM_COLOR", "never");
    cmd_fetch.env("CARGO_HTTP_PROXY", "http://127.0.0.1:1");
    cmd_fetch.env("HTTPS_PROXY", "http://127.0.0.1:1");
    cmd_fetch.env("NO_PROXY", "127.0.0.1,localhost");
    cmd_fetch.args(["fetch", "--locked"]);
    let out_fetch = cmd_fetch
        .output()
        .map_err(|e| Error::Operation(format!("failed to run cargo fetch: {e}")))?;
    if !out_fetch.status.success() {
        let stderr = String::from_utf8_lossy(&out_fetch.stderr);
        return Err(Error::Operation(format!("cargo fetch --locked failed: {stderr}")));
    }

    let lock_after = fs::read_to_string(test_proj.join("Cargo.lock"))?;
    drop(cleanup);
    let _ = fs::remove_dir_all(&val_tmp);
    if lock_after != *lock_str {
        return Err(Error::Integrity("Cargo.lock modified during verification".to_string()));
    }

    Ok(archived)
}

fn copy_project(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "target" || name == ".git" || name == ".work" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = fs::symlink_metadata(&src_path)?.file_type();
        if ft.is_symlink() {
            return Err(Error::Input(format!(
                "validate: refusing to copy symlink in project: {}",
                src_path.display()
            )));
        } else if ft.is_dir() {
            copy_project(&src_path, &dst_path)?;
        } else if ft.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

struct ValidateCleanup(std::path::PathBuf);

impl Drop for ValidateCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

