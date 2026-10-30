#![forbid(unsafe_code)]

use crate::config::Config;
use crate::error::{Error, Result};
use crate::inventory::{Manager, PackageRecord, ProjectRecord};
use crate::lock_npm::{parse_npm_lock, NpmOptions};
use crate::lock_pnpm::{parse_pnpm_lock, pnpm_build_package_keys, PnpmOptions};
use std::collections::BTreeSet;
use std::path::Path;

pub struct ManagerPin {
    pub raw: String,
    pub manager: Manager,
    pub version: String,
}

pub fn load_package_manager_pin(project_dir: &Path) -> Result<Option<ManagerPin>> {
    let path = project_dir.join("package.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(Error::Operation(format!(
                "could not read package.json: {}",
                project_dir.display()
            )));
        }
    };
    let manifest: deser_value::Value = deser_json::from_str(&text)
        .map_err(|_| Error::Input(format!("package.json is not valid JSON: {}", project_dir.display())))?;
    let root = manifest.as_map().ok_or_else(|| {
        Error::Input(format!("package.json must contain an object: {}", project_dir.display()))
    })?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let pin_val = root.get("packageManager").unwrap_or(&null);
    if crate::value::is_null(pin_val) {
        return Ok(None);
    }
    let raw = pin_val.as_str().ok_or_else(|| {
        Error::Input(format!("packageManager pin is unsupported: {}", project_dir.display()))
    })?.to_string();
    for manager in [Manager::Npm, Manager::Pnpm] {
        if crate::value::valid_manager_pin(&raw, manager.as_str()) {
            let version = raw.split('@').nth(1).unwrap_or("").split('+').next().unwrap_or("").to_string();
            return Ok(Some(ManagerPin { raw, manager, version }));
        }
    }
    Err(Error::Input(format!(
        "packageManager pin is unsupported: {}",
        project_dir.display()
    )))
}

fn optional_file_exists(path: &Path) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if !meta.is_file() {
                return Err(Error::Input(format!(
                    "project lockfile is not a regular file: {}",
                    path.display()
                )));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Error::Operation(format!(
            "could not inspect project lockfile: {}",
            path.display()
        ))),
    }
}

struct SourceFinding {
    kind: &'static str,
    host: String,
}

fn source_finding(item: &PackageRecord) -> Option<SourceFinding> {
    match item.source_kind {
        crate::inventory::SourceKind::Registry => None,
        crate::inventory::SourceKind::Local => Some(SourceFinding {
            kind: "local-source",
            host: "project path".to_string(),
        }),
        crate::inventory::SourceKind::Git => Some(SourceFinding {
            kind: "git-source",
            host: "git URL".to_string(),
        }),
        crate::inventory::SourceKind::Unknown => Some(SourceFinding {
            kind: "unknown-source",
            host: "no resolved URL".to_string(),
        }),
        crate::inventory::SourceKind::ExternalUrl => {
            let mut host = "invalid URL".to_string();
            if let Some(ref resolved) = item.resolved
                && let Ok(url) = url::Url::parse(resolved) {
                    host = url.host_str().unwrap_or("invalid URL").to_lowercase();
                    host = host.strip_suffix('.').unwrap_or(&host).to_string();
                }
            let private = match host.parse::<std::net::IpAddr>() {
                Ok(ip) => crate::acquire::is_restricted_address(ip),
                Err(_) => {
                    ["localhost", "local", "internal", "lan"].iter().any(|s| host == *s || host.ends_with(&format!(".{s}")))
                        || !host.contains('.')
                }
            };
            if private {
                return Some(SourceFinding { kind: "private-host", host });
            }
            if host == "registry.npmjs.org" {
                return Some(SourceFinding { kind: "credentialed-registry-source", host });
            }
            Some(SourceFinding { kind: "third-party-download", host })
        }
    }
}

fn package_download_risks(name: &str) -> Vec<&'static str> {
    let mut risks = Vec::new();
    if name == "electron" || name == "@electron/get" {
        risks.push("browser or runtime download");
    }
    if ["playwright", "playwright-core", "@playwright/test", "puppeteer", "puppeteer-core", "cypress"].contains(&name) {
        risks.push("browser download");
    }
    if name == "prisma" || name == "@prisma/client" || name == "@prisma/engines" {
        risks.push("native engine download");
    }
    if ["node-gyp", "node-pre-gyp", "@mapbox/node-pre-gyp"].contains(&name) {
        risks.push("native build or Node.js header download");
    }
    if name == "esbuild"
        || name.starts_with("@esbuild/")
        || name.starts_with("@swc/")
        || name == "sharp"
        || name.starts_with("@img/sharp-")
        || name.starts_with("@next/swc-")
        || name.starts_with("@napi-rs/")
        || name == "@parcel/watcher"
    {
        risks.push("native binary or build");
    }
    risks
}

fn npm_lock_has_install_script(lock_text: &str, key: &str) -> Result<bool> {
    let lock: deser_value::Value = deser_json::from_str(lock_text)
        .map_err(|_| Error::Input("npm lockfile packages must be an object".to_string()))?;
    let root = lock.as_map().ok_or_else(|| Error::Input("npm lockfile packages must be an object".to_string()))?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let packages = root.get("packages").unwrap_or(&null).as_map().ok_or_else(|| {
        Error::Input("npm lockfile packages must be an object".to_string())
    })?;
    let entry = packages.get(key).ok_or_else(|| {
        Error::Input(format!("npm lockfile package entry is malformed: {key}"))
    })?;
    let entry_map = entry.as_map().ok_or_else(|| {
        Error::Input(format!("npm lockfile package entry is malformed: {key}"))
    })?;
    Ok(entry_map.get("hasInstallScript").and_then(|v| v.as_bool()) == Some(true))
}

pub fn run_doctor(project_path: &Path, config: &Config) -> Result<()> {
    let pin = load_package_manager_pin(project_path)?;
    let npm_lock = project_path.join("package-lock.json");
    let pnpm_lock = project_path.join("pnpm-lock.yaml");
    let has_npm = optional_file_exists(&npm_lock)?;
    let has_pnpm = optional_file_exists(&pnpm_lock)?;
    let mut manager = pin.as_ref().map(|p| p.manager);
    if manager.is_none() {
        if has_npm == has_pnpm {
            return Err(Error::Input(format!(
                "project must contain exactly one npm or pnpm lockfile: {}",
                project_path.display()
            )));
        }
        manager = Some(if has_npm { Manager::Npm } else { Manager::Pnpm });
    }
    let manager = manager.expect("manager");
    let (lockfile_path, has_lock) = if manager == Manager::Npm {
        (npm_lock, has_npm)
    } else {
        (pnpm_lock, has_pnpm)
    };
    if !has_lock {
        let want = pin.as_ref().map(|p| p.raw.as_str()).unwrap_or(manager.as_str());
        return Err(Error::Input(format!(
            "project {want} does not have its matching lockfile: {}",
            project_path.display()
        )));
    }
    let text = std::fs::read_to_string(&lockfile_path)
        .map_err(|_| Error::Operation(format!("could not read project lockfile: {}", lockfile_path.display())))?;
    let lockfile_name = if manager == Manager::Npm { "package-lock.json" } else { "pnpm-lock.yaml" };
    let options_targets = vec!["doctor".to_string()];
    let project: ProjectRecord = if manager == Manager::Npm {
        parse_npm_lock(
            &text,
            NpmOptions {
                project_id: "doctor".to_string(),
                package_manager_pin: pin.as_ref().map(|p| p.raw.clone()),
                lockfile_path: lockfile_name.to_string(),
                target_ids: options_targets,
                snapshot: None,
            },
        )?
    } else {
        parse_pnpm_lock(
            &text,
            PnpmOptions {
                project_id: "doctor".to_string(),
                package_manager_pin: pin.as_ref().map(|p| p.raw.clone()),
                lockfile_path: lockfile_name.to_string(),
                target_ids: options_targets,
                snapshot: None,
            },
        )?
    };
    let script_keys: BTreeSet<String> = if manager == Manager::Npm {
        project
            .packages
            .iter()
            .filter(|item| npm_lock_has_install_script(&text, &item.key).unwrap_or(false))
            .map(|item| item.key.clone())
            .collect()
    } else {
        pnpm_build_package_keys(&text)?.into_iter().collect()
    };
    let mut findings: Vec<(&PackageRecord, SourceFinding)> = Vec::new();
    for item in &project.packages {
        if let Some(finding) = source_finding(item) {
            findings.push((item, finding));
        }
    }
    let root_manifest = load_project_manifest(project_path)?;
    let install_scripts: Vec<&PackageRecord> = project
        .packages
        .iter()
        .filter(|item| script_keys.contains(&item.key))
        .collect();
    let mut risks: Vec<(&PackageRecord, Vec<&str>)> = Vec::new();
    for item in &project.packages {
        let item_risks = package_download_risks(&item.name);
        if !item_risks.is_empty() {
            risks.push((item, item_risks));
        }
    }
    println!("Project: {}", project_path.display());
    if let Some(pin) = &pin {
        println!("Package manager pin: {}", pin.raw);
    }
    if findings.is_empty() {
        println!("No lockfile sources require migration review.");
    } else {
        println!("Lockfile sources requiring migration review: {}", findings.len());
        for (item, finding) in &findings {
            println!(
                "- {}: {}{} ({})",
                finding.kind,
                item.name,
                item.version.clone().map(|v| format!("@{v}")).unwrap_or_default(),
                finding.host
            );
        }
    }
    let lifecycle_names: Vec<String> = manifest_lifecycle_scripts(&root_manifest);
    if lifecycle_names.is_empty() {
        println!("Root lifecycle scripts: none");
    } else {
        println!("Root lifecycle scripts: {}", lifecycle_names.join(", "));
    }
    println!("Dependency install/build hooks in lockfile: {}", install_scripts.len());
    for item in &install_scripts {
        println!(
            "- install/build hook: {}{}",
            item.name,
            item.version.clone().map(|v| format!("@{v}")).unwrap_or_default()
        );
    }
    println!("Known browser/native download risks: {}", risks.len());
    for (item, item_risks) in &risks {
        println!(
            "- {}{}: {}",
            item.name,
            item.version.clone().map(|v| format!("@{v}")).unwrap_or_default(),
            item_risks.join(", ")
        );
    }
    let command = if manager == Manager::Npm {
        format!(
            "npm install --package-lock-only --ignore-scripts --no-audit --registry={}current/ --omit-lockfile-registry-resolved",
            config.public_base_url.trim_end_matches('/')
        )
    } else {
        format!(
            "pnpm install --lockfile-only --ignore-scripts --registry={}current/",
            config.public_base_url.trim_end_matches('/')
        )
    };
    let tool = pin.as_ref().map(|p| p.raw.as_str()).unwrap_or(manager.as_str());
    println!("To review migration safely, copy the project under .work and run {tool} in that copy:\n{command}");
    if manager == Manager::Npm {
        println!("npm replace-registry-host defaults to npmjs hosts; it does not rewrite private or third-party hosts. omit-lockfile-registry-resolved removes registry resolved URLs but does not prove the selected graph stayed exact.");
    } else {
        println!("pnpm may retain explicit resolution.tarball URLs from the lockfile; inspect every rewritten source and compare the selected package/version/integrity set.");
    }
    println!("Compare exact package/version/integrity selections and run a fresh-cache install before considering any change to the original project.");
    Ok(())
}

fn load_project_manifest(project_path: &Path) -> Result<deser_value::Map> {
    let path = project_path.join("package.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".to_string(),
        Err(_) => {
            return Err(Error::Operation(format!("could not read package.json: {}", project_path.display())));
        }
    };
    let manifest: deser_value::Value = deser_json::from_str(&text)
        .map_err(|_| Error::Input(format!("package.json is not valid JSON: {}", project_path.display())))?;
    manifest.as_map().cloned().ok_or_else(|| {
        Error::Input(format!("package.json must contain an object: {}", project_path.display()))
    })
}

fn manifest_has_script(manifest: &deser_value::Map, name: &str) -> bool {
    let null = deser_value::Value::from(deser_value::Kind::Null);
    manifest
        .get("scripts")
        .unwrap_or(&null)
        .as_map()
        .is_some_and(|scripts| scripts.get(name).is_some_and(|v| v.as_str().is_some()))
}

fn manifest_lifecycle_scripts(manifest: &deser_value::Map) -> Vec<String> {
    ["preinstall", "install", "postinstall", "prepare"]
        .into_iter()
        .filter(|name| manifest_has_script(manifest, name))
        .map(|s| s.to_string())
        .collect()
}
