#![forbid(unsafe_code)]

use crate::catalog::{Catalog, parse_catalog};
use crate::error::{Error, Kind, Result, Stage};
use crate::models::{PackageVersion, Target};
use crate::platforms::target_python_version;
use crate::process_policy::{redacted_diagnostic, run_capture, scratch_environment};
use stuff_store::{FileLock, LockError};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::Duration;

const PYTHON_PLATFORMS: &[(&str, &str)] = &[
    ("linux-x86_64", "x86_64-manylinux_2_28"),
    ("linux-aarch64", "aarch64-manylinux_2_28"),
    ("macos-x86_64", "x86_64-apple-darwin"),
    ("macos-arm64", "aarch64-apple-darwin"),
];

fn python_platform(platform: &str) -> Option<&'static str> {
    PYTHON_PLATFORMS.iter().find(|(p, _)| *p == platform).map(|(_, v)| *v)
}

#[derive(Debug, Clone)]
pub struct ResolvedPin {
    pub package: PackageVersion,
    pub platforms: Option<Vec<String>>,
}

fn dependency_error(detail: impl Into<String>) -> Error {
    Error::new(Kind::Dependency, detail, Stage::Planning)
}

fn parse_requirement_text(requirement: &str) -> Result<PackageVersion> {
    let req = requirement.trim();
    if req.is_empty()
        || req.contains('@')
        || req.contains(';')
        || req.contains('[')
        || req.contains(']')
    {
        return Err(Error::invalid_input(
            format!("requirement must be an exact name==version pin: {requirement:?}"),
            Stage::Command,
        ));
    }
    let Some((name_part, version_part)) = req.split_once("==") else {
        return Err(Error::invalid_input(
            format!("requirement must be an exact name==version pin: {requirement:?}"),
            Stage::Command,
        ));
    };
    if version_part.contains("==") || version_part.contains(',') || version_part.contains('*') || version_part.trim() != version_part || version_part.is_empty() {
        return Err(Error::invalid_input(
            format!("requirement must be an exact name==version pin: {requirement:?}"),
            Stage::Command,
        ));
    }
    let name = name_part.trim();
    if name.is_empty() || name.chars().any(|c| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))) {
        return Err(Error::invalid_input(
            format!("requirement must be an exact name==version pin: {requirement:?}"),
            Stage::Command,
        ));
    }
    PackageVersion::parse(name, version_part).map_err(|_| {
        Error::invalid_input(format!("invalid exact pin {requirement:?}"), Stage::Command)
    })
}

fn parse_exact_requirements(requirements: &[String]) -> Result<(Vec<PackageVersion>, Vec<String>)> {
    let mut packages = Vec::new();
    let mut source = Vec::new();
    for requirement in requirements {
        let pin = parse_requirement_text(requirement)?;
        packages.push(pin);
        source.push(requirement.trim().to_string());
    }
    if packages.is_empty() {
        return Err(Error::invalid_input("at least one exact package pin is required", Stage::Command));
    }
    let mut versions_by_name: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for package in &packages {
        versions_by_name
            .entry(package.name.as_str())
            .or_default()
            .insert(package.version.to_string());
    }
    for (name, versions) in &versions_by_name {
        if versions.len() > 1 {
            let list: Vec<&str> = versions.iter().map(|s| s.as_str()).collect();
            return Err(Error::conflict(
                format!("conflicting requested pins for {name}: {}", list.join(", ")),
                Stage::Command,
            ));
        }
    }
    // dedupe preserving order
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for p in packages {
        let key = format!("{p}");
        if seen.insert(key) {
            deduped.push(p);
        }
    }
    Ok((deduped, source))
}

pub fn pins_from_uv_lock(source: &str) -> Result<Vec<String>> {
    let value: deser_value::Value = deser_toml::from_str(source)
        .map_err(|e| Error::invalid_input(format!("invalid uv.lock: {e}"), Stage::Command))?;
    let packages = value
        .as_map()
        .and_then(|doc| doc.get("package"))
        .and_then(|v| v.as_seq())
        .ok_or_else(|| Error::invalid_input("uv.lock has no [[package]] entries", Stage::Command))?;
    let mut pins = Vec::new();
    for package in packages.iter() {
        let Some(table) = package.as_map() else {
            return Err(Error::invalid_input("uv.lock package entry must be a table", Stage::Command));
        };
        let is_registry = table
            .get("source")
            .and_then(|v| v.as_map())
            .is_some_and(|source| source.get("registry").is_some());
        if !is_registry {
            continue;
        }
        let name = table.get("name").and_then(|v| v.as_str());
        let version = table.get("version").and_then(|v| v.as_str());
        let (Some(name), Some(version)) = (name, version) else {
            return Err(Error::invalid_input(
                "uv.lock registry package is missing name or version",
                Stage::Command,
            ));
        };
        pins.push(format!("{name}=={version}"));
    }
    Ok(pins)
}

pub fn pins_from_requirements(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or("").trim().trim_end_matches('\\').trim();
            let pin = line.split(" --hash").next().unwrap_or("").trim();
            (!pin.is_empty() && !pin.starts_with("--hash")).then(|| pin.to_string())
        })
        .collect()
}

pub fn parse_pins(requirements: &[String]) -> Result<Vec<PackageVersion>> {
    Ok(parse_exact_requirements(requirements)?.0)
}

fn parse_compiled(output: &str) -> Result<Vec<PackageVersion>> {
    let mut resolved = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pin = parse_requirement_text(line).map_err(|_| {
            dependency_error(format!("uv returned an invalid pinned requirement: {line:?}"))
        })?;
        // parse_requirement_text already enforces exact == without wildcard
        resolved.push(pin);
    }
    Ok(resolved)
}

pub fn compile_for_target(
    requirements: &[String],
    target: &Target,
    python: &str,
) -> Result<Vec<PackageVersion>> {
    let platform = python_platform(target.platform.as_str())
        .ok_or_else(|| dependency_error(format!("unsupported resolution platform: {}", target.platform)))?;
    let execution = target_python_version(target);
    let command: Vec<String> = vec![
        "uv".to_string(),
        "pip".to_string(),
        "compile".to_string(),
        "--no-config".to_string(),
        "--quiet".to_string(),
        "--no-header".to_string(),
        "--no-annotate".to_string(),
        format!("--python={python}"),
        format!("--python-version={execution}"),
        format!("--python-platform={platform}"),
        "--default-index=https://pypi.org/simple".to_string(),
        "-".to_string(),
    ];
    let scratch = crate::process_policy::make_temp_dir("mypypi-compile-")
        .map_err(|e| dependency_error(format!("uv dependency resolution failed for {}: {e}", target.platform)))?;
    let env = scratch_environment(None, &scratch);
    let input = requirements.join("\n");
    let result = run_capture(&command, Some(&scratch), &env, Some(input.as_bytes()), 300);
    let _ = std::fs::remove_dir_all(&scratch);
    match result {
        Ok(out) if out.success() => parse_compiled(&out.stdout),
        Ok(out) => Err(dependency_error(format!(
            "uv dependency resolution failed for {}: {}",
            target.platform,
            redacted_diagnostic(&out.stderr, 2000)
        ))),
        Err(crate::process_policy::RunError::Timeout { .. }) => Err(dependency_error(format!(
            "uv dependency resolution timed out for {}",
            target.platform
        ))),
        Err(e) => Err(dependency_error(format!(
            "uv dependency resolution failed for {}: {e}",
            target.platform
        ))),
    }
}

pub fn resolve_dependencies(
    requirements: &[String],
    targets: &[Target],
    python: &str,
) -> Result<Vec<PackageVersion>> {
    Ok(resolve_dependencies_inner(requirements, targets, python)?.into_iter().map(|p| p.package).collect())
}

fn resolve_dependencies_inner(
    requirements: &[String],
    targets: &[Target],
    python: &str,
) -> Result<Vec<ResolvedPin>> {
    let (pins, normalized) = parse_exact_requirements(requirements)?;
    let mut resolutions: Vec<(Target, Vec<PackageVersion>)> = Vec::new();
    for target in targets {
        let resolution = compile_for_target(&normalized, target, python)?;
        let mut by_name: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for package in &resolution {
            by_name.entry(package.name.as_str()).or_default().insert(package.version.to_string());
        }
        let conflicts: Vec<(&str, &BTreeSet<String>)> =
            by_name.iter().filter(|(_, v)| v.len() > 1).map(|(k, v)| (*k, v)).collect();
        if let Some((name, found)) = conflicts.first() {
            let mut sorted: Vec<&String> = found.iter().collect();
            sorted.sort();
            let min: pep440_rs::Version = sorted[0].parse().map_err(|e| {
                Error::conflict(format!("invalid version: {e}"), Stage::Planning)
            })?;
            let pkg = PackageVersion { name: name.to_string(), version: min };
            let list: Vec<&str> = sorted.iter().map(|s| s.as_str()).collect();
            let mut e = Error::conflict(
                format!("dependency resolution conflicts for {name} on {}: {}", target.platform, list.join(", ")),
                Stage::Planning,
            );
            e = e.with_package(&pkg).with_target(&target.python_version.to_string(), &target.platform);
            return Err(e);
        }
        let resolved_by_name: BTreeMap<&str, &pep440_rs::Version> =
            resolution.iter().map(|p| (p.name.as_str(), &p.version)).collect();
        for pin in &pins {
            if let Some(resolved) = resolved_by_name.get(pin.name.as_str())
                && **resolved != pin.version
            {
                let mut e = Error::conflict(
                    format!(
                        "resolved dependency conflicts with requested pin {pin} on {}: {resolved}",
                        target.platform
                    ),
                    Stage::Planning,
                );
                e = e.with_package(pin).with_target(&target.python_version.to_string(), &target.platform);
                return Err(e);
            }
        }
        resolutions.push((target.clone(), resolution));
    }
    let root_names: BTreeSet<&str> = pins.iter().map(|p| p.name.as_str()).collect();
    let mut transitive: BTreeMap<PackageVersion, Vec<String>> = BTreeMap::new();
    for (target, resolution) in &resolutions {
        for package in resolution {
            if root_names.contains(package.name.as_str()) {
                continue;
            }
            let entry = transitive.entry(package.clone()).or_default();
            if !entry.contains(&target.platform) {
                entry.push(target.platform.clone());
            }
        }
    }
    let mut resolved: Vec<ResolvedPin> = pins.into_iter().map(|p| ResolvedPin { package: p, platforms: None }).collect();
    for (package, mut platforms) in transitive {
        platforms.sort();
        resolved.push(ResolvedPin { package, platforms: Some(platforms) });
    }
    resolved.sort_by(|a, b| a.package.cmp(&b.package));
    Ok(resolved)
}

pub fn append_entries(source: &str, packages: &[ResolvedPin], existing: &Catalog) -> Result<String> {
    let mut present: BTreeMap<&PackageVersion, &crate::catalog::PackageEntry> = BTreeMap::new();
    for entry in &existing.entries {
        present.insert(&entry.package, entry);
    }
    let mut requested: BTreeMap<&PackageVersion, Option<BTreeSet<String>>> = BTreeMap::new();
    for item in packages {
        let entry = requested.entry(&item.package).or_insert(None);
        match &item.platforms {
            None => *entry = None,
            Some(plats) => match entry {
                Some(set) => {
                    for p in plats {
                        set.insert(p.clone());
                    }
                }
                none @ None => {
                    *none = Some(plats.iter().cloned().collect());
                }
            },
        }
    }
    let mut additions: Vec<(&PackageVersion, Option<Vec<String>>)> = Vec::new();
    for (package, platforms) in &requested {
        let current = present.get(package);
        if current.is_some_and(|c| c.platforms.is_none()) {
            continue;
        }
        match platforms {
            None => additions.push((*package, None)),
            Some(wanted) => {
                let current_set: BTreeSet<&str> = current
                    .and_then(|c| c.platforms.as_ref())
                    .map(|v| v.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                let missing: Vec<String> =
                    wanted.iter().filter(|p| !current_set.contains(p.as_str())).cloned().collect();
                if !missing.is_empty() {
                    additions.push((*package, Some(missing)));
                }
            }
        }
    }
    if additions.is_empty() {
        return Ok(source.to_string());
    }
    // remove `packages = []` marker line if present
    let mut updated = String::new();
    for line in source.lines() {
        if line.trim() == "packages = []" {
            continue;
        }
        updated.push_str(line);
        updated.push('\n');
    }
    // source may not end with newline; normalize: ensure separator
    let mut out = updated;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for (package, platforms) in &additions {
        out.push_str("\n[[packages]]\n");
        out.push_str(&format!("name = \"{}\"\n", package.name));
        out.push_str(&format!("version = \"{}\"\n", package.version));
        if let Some(plats) = platforms {
            out.push_str("platforms = [");
            for (i, p) in plats.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("{p:?}"));
            }
            out.push_str("]\n");
        }
    }
    parse_catalog(&out)?;
    Ok(out)
}

pub fn add_packages(catalog_path: &Path, requirements: &[String], targets: &[Target], python: &str) -> Result<Vec<PackageVersion>> {
    let initial = read_catalog(catalog_path)?;
    parse_catalog(&initial)?;
    let resolved = resolve_dependencies_inner(requirements, targets, python)?;
    let guard = lock_catalog(catalog_path)?;
    let source = read_catalog(catalog_path)?;
    let existing = parse_catalog(&source)?;
    let updated = append_entries(&source, &resolved, &existing)?;
    parse_catalog(&updated)?;
    if updated != source {
        replace_catalog(catalog_path, &updated)?;
    }
    drop(guard);
    Ok(resolved.into_iter().map(|p| p.package).collect())
}

fn read_catalog(path: &Path) -> Result<String> {
    let md = std::fs::symlink_metadata(path)
        .map_err(|e| Error::io(format!("could not read catalog at {}: {e}", path.display()), Stage::Configuration))?;
    if md.file_type().is_symlink() || !md.file_type().is_file() {
        return Err(Error::invalid_input(
            format!("catalog path must be a regular file: {}", path.display()),
            Stage::Configuration,
        ));
    }
    std::fs::read_to_string(path).map_err(|e| {
        Error::io(format!("could not read catalog at {}: {e}", path.display()), Stage::Configuration)
    })
}

fn lock_catalog(catalog: &Path) -> Result<FileLock> {
    let name = catalog.file_name().and_then(|s| s.to_str()).unwrap_or("packages.toml");
    let lock_path = catalog.with_file_name(format!(".{name}.lock"));
    FileLock::acquire(&lock_path, Duration::from_secs(120)).map_err(|e| {
        let reason = match e {
            LockError::TimedOut => "timed out".to_string(),
            LockError::Io(e) => e.to_string(),
        };
        Error::io(format!("could not lock catalog at {}: {reason}", lock_path.display()), Stage::Configuration)
    })
}

fn replace_catalog(path: &Path, source: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let md = std::fs::symlink_metadata(path)
        .map_err(|e| Error::io(format!("could not update catalog at {}: {e}", path.display()), Stage::Configuration))?;
    if md.file_type().is_symlink() || !md.file_type().is_file() {
        return Err(Error::io(
            format!("could not update catalog at {}", path.display()),
            Stage::Configuration,
        ));
    }
    let mode = md.permissions().mode() & 0o777;
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("packages.toml");
    let tmp = path.with_file_name(format!(
        ".{name}.{:x}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let write_result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
            .map_err(|e| Error::io(format!("could not update catalog at {}: {e}", path.display()), Stage::Configuration))?;
        f.write_all(source.as_bytes())
            .map_err(|e| Error::io(format!("could not update catalog at {}: {e}", path.display()), Stage::Configuration))?;
        f.sync_all()
            .map_err(|e| Error::io(format!("could not update catalog at {}: {e}", path.display()), Stage::Configuration))?;
        drop(f);
        std::fs::rename(&tmp, path)
            .map_err(|e| Error::io(format!("could not update catalog at {}: {e}", path.display()), Stage::Configuration))?;
        let parent = path.parent().unwrap_or(Path::new("."));
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok::<(), Error>(())
    })();
    let _ = std::fs::remove_file(&tmp);
    write_result
}

pub fn canonical_pin(name: &str, version: &str) -> Result<PackageVersion> {
    PackageVersion::parse(name, version).map_err(|e| {
        Error::invalid_input(format!("invalid exact pin {name:?}=={version:?}: {}", e.detail), Stage::Command)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_pins_and_rejects_slop() {
        let pins = parse_pins(&["foo-bar==1.0".to_string()]).unwrap();
        assert_eq!(pins.len(), 1);
        assert!(parse_pins(&["foo>=1.0".to_string()]).is_err());
        assert!(parse_pins(&["foo==1.*".to_string()]).is_err());
        assert!(parse_pins(&["foo==1.0; python_version>'3'".to_string()]).is_err());
        assert!(parse_pins(&["foo @ https://example.test/foo.tar.gz".to_string()]).is_err());
        assert!(parse_pins(&[] as &[String]).is_err());
        assert!(parse_pins(&["a==1.0".to_string(), "a==2.0".to_string()]).is_err());
    }

    #[test]
    fn reads_registry_pins_from_uv_lock() {
        let lock = "version = 1\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\nsource = { virtual = \".\" }\n\n[[package]]\nname = \"colorama\"\nversion = \"0.4.6\"\nsource = { registry = \"https://pypi.org/simple\" }\n\n[[package]]\nname = \"lib\"\nversion = \"1.0\"\nsource = { git = \"https://example.test/lib\" }\n";
        assert_eq!(pins_from_uv_lock(lock).unwrap(), vec!["colorama==0.4.6".to_string()]);
        assert!(pins_from_uv_lock("version = 1\n").is_err());
    }

    #[test]
    fn reads_pins_from_requirements_with_hashes_and_comments() {
        let text = "# header\nfoo==1.0 \\\n    --hash=sha256:abc\nbar==2.0  # note\n\n";
        assert_eq!(pins_from_requirements(text), vec!["foo==1.0".to_string(), "bar==2.0".to_string()]);
    }

    #[test]
    fn appends_new_entries_idempotently() {
        let existing = parse_catalog("").unwrap_or(Catalog { entries: Vec::new() });
        let pins = vec![ResolvedPin {
            package: PackageVersion::parse("foo", "1.0").unwrap(),
            platforms: None,
        }];
        let updated = append_entries("", &pins, &existing).unwrap();
        assert!(updated.contains("[[packages]]"));
        let catalog = parse_catalog(&updated).unwrap();
        assert_eq!(catalog.entries.len(), 1);
        // second append is a no-op
        let again = append_entries(&updated, &pins, &catalog).unwrap();
        assert_eq!(again, updated);
    }

    #[test]
    fn appends_split_platform_rows() {
        let existing = parse_catalog("[[packages]]\nname = \"foo\"\nversion = \"1.0\"\nplatforms = [\"linux-x86_64\"]\n").unwrap();
        let pins = vec![ResolvedPin {
            package: PackageVersion::parse("foo", "1.0").unwrap(),
            platforms: Some(vec!["macos-arm64".to_string()]),
        }];
        let updated = append_entries("[[packages]]\nname = \"foo\"\nversion = \"1.0\"\nplatforms = [\"linux-x86_64\"]\n", &pins, &existing).unwrap();
        assert!(updated.contains("macos-arm64"));
    }
}
