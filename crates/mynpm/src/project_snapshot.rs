#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::inventory::{Manager, NpmInstallOptions, ProjectRecord, ProjectSnapshot, SnapshotFile};
use crate::value;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

fn safe_segments(relative: &str, label: &str) -> Result<Vec<String>> {
    if relative.is_empty() || relative.starts_with('/') || relative.contains('\\') {
        return Err(Error::Input(format!("{label} is not a safe project-relative path")));
    }
    let segments: Vec<String> = relative.split('/').map(|s| s.to_string()).collect();
    if segments.iter().any(|s| s.is_empty() || s == "." || s == "..") {
        return Err(Error::Input(format!("{label} is not a safe project-relative path")));
    }
    Ok(segments)
}

fn read_project_file(project_root: &Path, relative_path: &str, optional: bool) -> Result<Option<String>> {
    let segments = safe_segments(relative_path, "project snapshot path").map_err(|_| {
        Error::Input(format!("project snapshot path is not safe: {relative_path}"))
    })?;
    let mut directory = project_root.to_path_buf();
    for segment in &segments[..segments.len() - 1] {
        directory = directory.join(segment);
        match fs_symlink_dir_check(&directory) {
            Ok(is_dir) => {
                if !is_dir {
                    return Err(Error::Input(format!(
                        "project snapshot path is not a regular directory: {relative_path}"
                    )));
                }
            }
            Err(e) => {
                if optional && e.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(Error::Input(format!(
                    "could not inspect project snapshot path {relative_path}"
                )));
            }
        }
    }
    let file_path = directory.join(&segments[segments.len() - 1]);
    let mut file = match open_no_follow(&file_path) {
        Ok(f) => f,
        Err(e) => {
            if optional && (e.kind() == std::io::ErrorKind::NotFound) {
                return Ok(None);
            }
            return Err(Error::Input(format!("could not read project snapshot path {relative_path}")));
        }
    };
    let meta = file.metadata().map_err(|_| {
        Error::Input(format!("could not read project snapshot path {relative_path}"))
    })?;
    if !meta.is_file() {
        return Err(Error::Input(format!(
            "project snapshot path is not a regular file: {relative_path}"
        )));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(|_| {
        Error::Input(format!("could not read project snapshot path {relative_path}"))
    })?;
    Ok(Some(contents))
}

fn fs_symlink_dir_check(path: &Path) -> std::io::Result<bool> {
    let meta = std::fs::symlink_metadata(path)?;
    Ok(meta.is_dir() && !meta.file_type().is_symlink())
}

fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

fn find_urls(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(pos) = text[i..].find("://") {
            let abs = i + pos;
            let mut start = abs;
            while start > i {
                let c = bytes[start - 1];
                if c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.' {
                    start -= 1;
                } else {
                    break;
                }
            }
            if start < abs && bytes[start].is_ascii_alphabetic() {
                let mut end = abs + 3;
                while end < bytes.len() {
                    let c = bytes[end];
                    if c.is_ascii_whitespace() || c == b'"' || c == b'\'' || c == b'<' || c == b'>' {
                        break;
                    }
                    end += 1;
                }
                out.push(text[start..end].to_string());
                i = end;
                continue;
            }
            i = abs + 3;
        } else {
            break;
        }
    }
    out
}

fn assert_no_credential_urls_text(text: &str, label: &str) -> Result<()> {
    for candidate in find_urls(text) {
        let normalized = if candidate.len() > 4 && candidate[..4].eq_ignore_ascii_case("git+") {
            &candidate[4..]
        } else {
            &candidate[..]
        };
        let after_scheme = normalized.find("://").map(|p| &normalized[p + 3..]).unwrap_or("");
        let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
        match url::Url::parse(normalized) {
            Ok(parsed) => {
                if authority.contains('@')
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.query().is_some()
                {
                    return Err(Error::Input(format!("{label} contains a credential-bearing URL")));
                }
            }
            Err(_) => {
                if authority.contains('@') || normalized.contains('?') {
                    return Err(Error::Input(format!("{label} contains a credential-bearing URL")));
                }
            }
        }
    }
    Ok(())
}

fn assert_no_credential_urls(value: &deser_value::Value, label: &str) -> Result<()> {
    if let Some(s) = value.as_str() {
        return assert_no_credential_urls_text(s, label);
    }
    if let Some(seq) = value.as_seq() {
        for item in seq.iter() {
            assert_no_credential_urls(item, label)?;
        }
        return Ok(());
    }
    if let Some(map) = value.as_map() {
        for i in 0..map.len() {
            if let Some((_, item)) = map.get_index(i) {
                assert_no_credential_urls(item, label)?;
            }
        }
    }
    Ok(())
}

fn parse_manifest(text: &str, label: &str, manager: &Manager) -> Result<deser_value::Value> {
    let manifest: deser_value::Value = deser_json::from_str(text)
        .map_err(|_| Error::Input(format!("{label} is not valid JSON")))?;
    if manifest.as_map().is_none() {
        return Err(Error::Input(format!("{label} must contain an object")));
    }
    assert_no_credential_urls(&manifest, label)?;
    if *manager == Manager::Pnpm {
        let null = deser_value::Value::from(deser_value::Kind::Null);
        if let Some(pnpm) = manifest.as_map().and_then(|m| m.get("pnpm")) {
            inspect_pnpm_configuration(pnpm, &format!("{label}.pnpm"))?;
        }
        let _ = &null;
        if let Some(dev_engines) = manifest.as_map().and_then(|m| m.get("devEngines"))
            && dev_engines.as_map().is_some_and(|m| m.get("runtime").is_some()) {
                return Err(Error::Input(format!("unsupported pnpm project configuration: runtime in {label}")));
            }
    }
    Ok(manifest)
}

fn parse_npmrc(text: Option<&str>, manager: &Manager) -> Result<NpmInstallOptions> {
    let mut options = NpmInstallOptions { legacy_peer_deps: None, install_links: None };
    let Some(text) = text else { return Ok(options) };
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    for (index, raw_line) in text.split(['\n']).enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line).trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let (key, raw_value) = match line.find('=') {
            Some(sep) => (line[..sep].trim().to_lowercase(), line[sep + 1..].trim().to_lowercase()),
            None => (line.trim().to_lowercase(), "true".to_string()),
        };
        let slot = if key == "legacy-peer-deps" {
            "legacyPeerDeps"
        } else if key == "install-links" {
            "installLinks"
        } else {
            ""
        };
        if slot.is_empty() {
            let display = if key.is_empty() { format!("on line {}", index + 1) } else { key.clone() };
            return Err(Error::Input(format!("unsupported .npmrc setting {display}")));
        }
        if *manager != Manager::Npm {
            return Err(Error::Input(format!("unsupported .npmrc setting {key} for pnpm")));
        }
        if !seen.insert(key.clone()) {
            return Err(Error::Input(format!("duplicate .npmrc setting {key}")));
        }
        if raw_value != "true" && raw_value != "false" {
            return Err(Error::Input(format!(".npmrc setting {key} must be true or false")));
        }
        let val = raw_value == "true";
        if slot == "legacyPeerDeps" {
            options.legacy_peer_deps = Some(val);
        } else {
            options.install_links = Some(val);
        }
    }
    Ok(options)
}

fn parse_yaml_documents(text: &str, label: &str) -> Result<Vec<deser_value::Value>> {
    let mut de = deser_yaml::Deserializer::from_str(text);
    let mut docs = Vec::new();
    loop {
        if de.is_end() {
            break;
        }
        match de.deserialize::<deser_value::Value>() {
            Ok(doc) => docs.push(doc),
            Err(e) => return Err(Error::Input(format!("{label} is not valid YAML: {e}"))),
        }
    }
    Ok(docs)
}

fn has_entries(value: &deser_value::Value) -> bool {
    if value::is_null(value) {
        return false;
    }
    if let Some(b) = value.as_bool() {
        return b;
    }
    if let Some(s) = value.as_str() {
        return !s.is_empty();
    }
    if let Some(seq) = value.as_seq() {
        return !seq.is_empty();
    }
    if let Some(map) = value.as_map() {
        return !map.is_empty();
    }
    true
}

fn unsupported_pnpm_key(key: &str) -> Option<&'static str> {
    let normalized: String = key.to_lowercase().chars().filter(|c| *c != '-' && *c != '_').collect();
    if normalized.contains("registry") {
        return Some("custom registry");
    }
    if ["runtime", "nodeversion", "usenodeversion", "executionenv"].contains(&normalized.as_str()) {
        return Some("runtime");
    }
    if normalized == "pnpmfile" || normalized == "hooks" {
        return Some("executable hook");
    }
    if normalized == "configdependencies" {
        return Some("configuration dependency");
    }
    if normalized == "patcheddependencies" || normalized == "patches" {
        return Some("patch configuration");
    }
    None
}

fn inspect_pnpm_configuration(value: &deser_value::Value, label: &str) -> Result<()> {
    let Some(map) = value.as_map() else { return Ok(()) };
    for i in 0..map.len() {
        let Some((k, child)) = map.get_index(i) else { continue };
        let key = k.as_str().unwrap_or("").to_string();
        if let Some(kind) = unsupported_pnpm_key(&key)
            && (kind == "custom registry" || has_entries(child)) {
                return Err(Error::Input(format!("unsupported pnpm project configuration: {kind} in {label}")));
            }
        if key == "settings" || key == "pnpm" {
            inspect_pnpm_configuration(child, &format!("{label}.{key}"))?;
        }
    }
    Ok(())
}

fn preflight_pnpm_lock(value: &deser_value::Value, label: &str) -> Result<()> {
    let Some(map) = value.as_map() else {
        return Err(Error::Input(format!("{label} document must be an object")));
    };
    let null = deser_value::Value::from(deser_value::Kind::Null);
    if let Some(settings) = map.get("settings") {
        inspect_pnpm_configuration(settings, &format!("{label}.settings"))?;
    }
    let _ = &null;
    for i in 0..map.len() {
        let Some((k, child)) = map.get_index(i) else { continue };
        let key = k.as_str().unwrap_or("").to_string();
        if key == "settings" {
            continue;
        }
        if let Some(kind) = unsupported_pnpm_key(&key)
            && (kind == "custom registry" || has_entries(child)) {
                return Err(Error::Input(format!("unsupported pnpm project configuration: {kind} in {label}")));
            }
    }
    if let Some(importers) = map.get("importers").and_then(|v| v.as_map()) {
        for i in 0..importers.len() {
            let Some((ik, entry)) = importers.get_index(i) else { continue };
            let importer = ik.as_str().unwrap_or("");
            if let Some(em) = entry.as_map() {
                for probe in ["configDependencies", "packageManagerDependencies"] {
                    if em.get(probe).is_some_and(has_entries) {
                        let kind = if probe == "configDependencies" {
                            "configuration dependency"
                        } else {
                            "package-manager boot dependency"
                        };
                        return Err(Error::Input(format!(
                            "unsupported pnpm project configuration: {kind} in {label}.importers.{importer}"
                        )));
                    }
                }
            }
        }
    }
    for section in ["packages", "snapshots"] {
        if let Some(entries) = map.get(section).and_then(|v| v.as_map()) {
            for i in 0..entries.len() {
                if let Some((k, _)) = entries.get_index(i)
                    && k.as_str().is_some_and(|s| s.to_lowercase().contains("patch_hash=")) {
                        return Err(Error::Input(format!(
                            "unsupported pnpm project configuration: patch configuration in {label}.{section}"
                        )));
                    }
            }
        }
    }
    Ok(())
}

fn preflight_pnpm_config(value: &deser_value::Value, label: &str) -> Result<()> {
    let Some(map) = value.as_map() else { return Ok(()) };
    inspect_pnpm_configuration(value, label)?;
    if let Some(nested) = map.get("pnpm") {
        inspect_pnpm_configuration(nested, &format!("{label}.pnpm"))?;
    }
    Ok(())
}

fn preflight_pnpm_lockfile(text: &str) -> Result<()> {
    let documents = parse_yaml_documents(text, "pnpm lockfile")?;
    for (index, document) in documents.iter().enumerate() {
        assert_no_credential_urls(document, &format!("pnpm lockfile document {}", index + 1))?;
        preflight_pnpm_lock(document, &format!("pnpm lockfile document {}", index + 1))?;
    }
    Ok(())
}

pub fn assert_restorable_snapshot(manager: &Manager, snapshot: &ProjectSnapshot) -> Result<()> {
    if let Some(ref manifest) = snapshot.manifest {
        parse_manifest(manifest, "project package.json snapshot", manager)?;
    }
    if *manager == Manager::Npm {
        let lock: deser_value::Value = deser_json::from_str(&snapshot.lockfile)
            .map_err(|_| Error::Input("npm lockfile snapshot is not valid JSON".to_string()))?;
        assert_no_credential_urls(&lock, "npm lockfile snapshot")?;
    } else {
        preflight_pnpm_lockfile(&snapshot.lockfile)?;
    }
    for file in &snapshot.workspace_files {
        if file.path == "pnpm-workspace.yaml" && *manager == Manager::Pnpm {
            let documents = parse_yaml_documents(&file.contents, &file.path)?;
            for (index, document) in documents.iter().enumerate() {
                assert_no_credential_urls(document, &format!("{} document {}", file.path, index + 1))?;
                preflight_pnpm_config(document, &format!("{} document {}", file.path, index + 1))?;
            }
        } else if file.path.ends_with("/package.json") {
            parse_manifest(&file.contents, &file.path, manager)?;
        } else {
            return Err(Error::Input(format!("unsupported project snapshot file {}", file.path)));
        }
    }
    Ok(())
}

fn npm_workspace_paths(lock_text: &str) -> Result<std::collections::BTreeSet<String>> {
    let lock: deser_value::Value = deser_json::from_str(lock_text)
        .map_err(|_| Error::Input("npm lockfile must be an object".to_string()))?;
    let root = lock.as_map().ok_or_else(|| Error::Input("npm lockfile must be an object".to_string()))?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let packages_val = root.get("packages").unwrap_or(&null);
    if value::is_null(packages_val) {
        return Ok(Default::default());
    }
    let packages = packages_val.as_map().ok_or_else(|| Error::Input("npm lockfile packages must be an object".to_string()))?;
    let mut paths = std::collections::BTreeSet::new();
    for i in 0..packages.len() {
        let Some((_, entry_value)) = packages.get_index(i) else { continue };
        let Some(entry) = entry_value.as_map() else { continue };
        let is_link = entry.get("link").and_then(|v| v.as_bool()) == Some(true);
        let resolved = entry.get("resolved").and_then(|v| v.as_str()).unwrap_or("");
        if !is_link || resolved.is_empty() {
            continue;
        }
        let target = resolved.strip_prefix("file:").unwrap_or(resolved).strip_prefix("./").unwrap_or(resolved.strip_prefix("file:").unwrap_or(resolved));
        if target.starts_with('/') || target.contains('\\') || target.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
            continue;
        }
        paths.insert(target.to_string());
    }
    Ok(paths)
}

pub struct CaptureOptions<'a> {
    pub manager: &'a Manager,
    pub project_directory: &'a str,
    pub lockfile_text: &'a str,
    pub project: &'a ProjectRecord,
}

pub fn capture_project_snapshot(options: &CaptureOptions) -> Result<ProjectSnapshot> {
    let project_root = std::fs::canonicalize(options.project_directory)
        .map_err(|_| Error::Input(format!("could not resolve project directory {}", options.project_directory)))?;
    let manifest = read_project_file(&project_root, "package.json", true)?;
    let npmrc = read_project_file(&project_root, ".npmrc", true)?;
    let npm_install_options = parse_npmrc(npmrc.as_deref(), options.manager)?;
    if *options.manager == Manager::Pnpm {
        for hook in [".pnpmfile.cjs", ".pnpmfile.mjs"] {
            if read_project_file(&project_root, hook, true)?.is_some() {
                return Err(Error::Input(format!("unsupported pnpm project configuration: executable hook {hook}")));
            }
        }
    }
    let workspace_paths = if *options.manager == Manager::Pnpm {
        options.project.importers.iter().filter(|i| *i != ".").cloned().collect::<std::collections::BTreeSet<_>>()
    } else {
        npm_workspace_paths(options.lockfile_text)?
    };
    let mut workspace_files: Vec<SnapshotFile> = Vec::new();
    let mut sorted_paths: Vec<String> = workspace_paths.into_iter().collect();
    sorted_paths.sort();
    for workspace_path in sorted_paths {
        let relative = format!("{workspace_path}/package.json");
        if let Some(contents) = read_project_file(&project_root, &relative, true)? {
            workspace_files.push(SnapshotFile { path: relative, contents });
        }
    }
    if *options.manager == Manager::Pnpm
        && let Some(contents) = read_project_file(&project_root, "pnpm-workspace.yaml", true)? {
            workspace_files.push(SnapshotFile { path: "pnpm-workspace.yaml".to_string(), contents });
        }
    let snapshot = ProjectSnapshot {
        manifest,
        lockfile: options.lockfile_text.to_string(),
        workspace_files,
        npm_install_options,
    };
    assert_restorable_snapshot(options.manager, &snapshot)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_npmrc_options() {
        let options = parse_npmrc(Some("legacy-peer-deps=true\ninstall-links=false\n"), &Manager::Npm).unwrap();
        assert_eq!(options.legacy_peer_deps, Some(true));
        assert_eq!(options.install_links, Some(false));
        assert!(parse_npmrc(Some("registry=https://example.test/\n"), &Manager::Npm).is_err());
        assert!(parse_npmrc(Some("legacy-peer-deps=true\n"), &Manager::Pnpm).is_err());
        assert!(parse_npmrc(Some("legacy-peer-deps=1\n"), &Manager::Npm).is_err());
    }

    #[test]
    fn rejects_credential_urls() {
        assert!(assert_no_credential_urls_text("https://user:pass@registry.npmjs.org/x", "t").is_err());
        assert!(assert_no_credential_urls_text("https://registry.npmjs.org/x?token=abc", "t").is_err());
        assert!(assert_no_credential_urls_text("https://registry.npmjs.org/x", "t").is_ok());
        assert!(assert_no_credential_urls_text("not a url at all", "t").is_ok());
    }

    #[test]
    fn captures_npm_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("package.json"), r#"{"name":"demo","version":"1.0.0"}"#).unwrap();
        let lock = r#"{"lockfileVersion":3,"packages":{"":{"name":"demo"},"node_modules/a":{"version":"1.0.0"}}}"#;
        std::fs::write(root.join("package-lock.json"), lock).unwrap();
        let project = ProjectRecord {
            id: "demo".to_string(),
            manager: Manager::Npm,
            package_manager_pin: None,
            lockfile_path: "package-lock.json".to_string(),
            lockfile_version: crate::inventory::LockfileVersion::Npm(3),
            targets: vec![],
            importers: vec![".".to_string()],
            roots: vec![],
            packages: vec![],
            snapshot: None,
        };
        let snapshot = capture_project_snapshot(&CaptureOptions {
            manager: &Manager::Npm,
            project_directory: root.to_str().unwrap(),
            lockfile_text: lock,
            project: &project,
        })
        .unwrap();
        assert!(snapshot.manifest.is_some());
        assert!(snapshot.workspace_files.is_empty());
    }

    #[test]
    fn rejects_pnpm_hooks_and_registries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".pnpmfile.cjs"), "module.exports = {}").unwrap();
        let project = ProjectRecord {
            id: "demo".to_string(),
            manager: Manager::Pnpm,
            package_manager_pin: None,
            lockfile_path: "pnpm-lock.yaml".to_string(),
            lockfile_version: crate::inventory::LockfileVersion::Pnpm("9.0".to_string()),
            targets: vec![],
            importers: vec![".".to_string()],
            roots: vec![],
            packages: vec![],
            snapshot: None,
        };
        let err = capture_project_snapshot(&CaptureOptions {
            manager: &Manager::Pnpm,
            project_directory: dir.path().to_str().unwrap(),
            lockfile_text: "lockfileVersion: '9.0'\n",
            project: &project,
        })
        .unwrap_err();
        assert!(format!("{err}").contains("executable hook"));
    }
}
