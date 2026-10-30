#![forbid(unsafe_code)]

use crate::config::{Config, Target};
use crate::error::{Error, Result};
use crate::inventory::{Manager, ProjectRecord, SourceKind};
use crate::inventory_store::update_inventory;
use crate::lock_npm::{parse_npm_lock, NpmOptions};
use crate::lock_pnpm::{parse_pnpm_lock, PnpmOptions};
use crate::process::{run_process, ProcessOptions};
use crate::project_snapshot::assert_restorable_snapshot;
use std::collections::HashMap;

pub const NPM_VERSION: &str = "12.2.1";
pub const PNPM_VERSION: &str = "12.15.0";
const OPERATION_TIMEOUT_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct RootPin {
    pub name: String,
    pub version: String,
}

pub fn parse_root_pins(specs: &[String]) -> Result<Vec<RootPin>> {
    let mut roots = Vec::with_capacity(specs.len());
    for value in specs {
        let separator = if let Some(rest) = value.strip_prefix('@') {
            rest.rfind('@').map(|i| i + 1)
        } else {
            value.find('@')
        };
        let Some(sep) = separator else {
            return Err(Error::Input(format!("add requires an exact package pin: {value}")));
        };
        if sep == 0 || sep == value.len() - 1 {
            return Err(Error::Input(format!("add requires an exact package pin: {value}")));
        }
        let name = &value[..sep];
        let version = &value[sep + 1..];
        if !crate::value::valid_package_name(name) || !crate::value::valid_version(version) {
            return Err(Error::Input(format!("add requires an exact package pin: {value}")));
        }
        roots.push(RootPin {
            name: name.to_string(),
            version: version.to_string(),
        });
    }
    roots.sort_by_key(|r| (r.name.clone(), r.version.clone()));
    let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
    let mut dedup = names.clone();
    dedup.sort();
    dedup.dedup();
    if dedup.len() != names.len() {
        return Err(Error::Input("add accepts only one exact version per root package".to_string()));
    }
    Ok(roots)
}

pub fn targets_for(config: &Config, requested: &[String]) -> Result<Vec<Target>> {
    let ids: Vec<String> = if requested.is_empty() {
        config.targets.iter().map(|t| t.id().to_string()).collect()
    } else {
        requested.to_vec()
    };
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != ids.len() {
        return Err(Error::Input("target IDs must be unique".to_string()));
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in sorted {
        let target = config
            .targets
            .iter()
            .find(|t| t.id() == id)
            .ok_or_else(|| Error::Input(format!("target is not configured: {id}")))?;
        out.push(target.clone());
    }
    Ok(out)
}

fn tool_environment(
    workspace: &str,
    target: &Target,
    registry: &str,
    path: &str,
    locale: &str,
    tmpdir: &str,
) -> HashMap<String, String> {
    let home = format!("{workspace}/home");
    let cache = format!("{workspace}/cache");
    let store = format!("{workspace}/store");
    let npmrc = format!("{workspace}/npmrc");
    let mut env = HashMap::from([
        ("PATH".to_string(), path.to_string()),
        ("LANG".to_string(), locale.to_string()),
        ("LC_ALL".to_string(), "C".to_string()),
        ("CI".to_string(), "true".to_string()),
        ("HOME".to_string(), home),
        ("TMPDIR".to_string(), tmpdir.to_string()),
        ("npm_config_userconfig".to_string(), npmrc),
        ("npm_config_cache".to_string(), cache),
        ("npm_config_registry".to_string(), registry.to_string()),
        ("npm_config_audit".to_string(), "false".to_string()),
        ("npm_config_fund".to_string(), "false".to_string()),
        ("npm_config_ignore_scripts".to_string(), "true".to_string()),
        ("npm_config_update_notifier".to_string(), "false".to_string()),
        ("npm_config_fetch_timeout".to_string(), "600000".to_string()),
        ("npm_config_os".to_string(), target.os().to_string()),
        ("npm_config_cpu".to_string(), target.cpu().to_string()),
        ("npm_config_node_version".to_string(), target.node().to_string()),
        ("pnpm_config_store_dir".to_string(), store),
        ("COREPACK_HOME".to_string(), format!("{workspace}/corepack")),
    ]);
    if let crate::config::Target::Linux(t) = target {
        env.insert("npm_config_libc".to_string(), t.libc.clone());
    }
    env
}

fn verify_root_selections(project: &ProjectRecord, roots: &[RootPin], target_id: &str, manager: &Manager) -> Result<()> {
    let observed: Vec<_> = project
        .roots
        .iter()
        .filter(|r| r.importer == "." && r.group == "dependencies")
        .collect();
    if *manager == Manager::Npm && observed.len() != roots.len() {
        return Err(Error::Conflict(format!("resolved root set differs for target {target_id}")));
    }
    for expected in roots {
        let selected: Vec<_> = observed.iter().filter(|r| r.requested_name == expected.name).collect();
        if selected.is_empty()
            || selected.iter().any(|r| r.package_name != expected.name || r.package_version.as_deref() != Some(expected.version.as_str()))
        {
            return Err(Error::Conflict(format!(
                "resolver did not select {}@{} for target {target_id}",
                expected.name, expected.version
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_target(
    manager: &Manager,
    pin: &str,
    roots: &[RootPin],
    target: &Target,
    config: &Config,
    workspace: &str,
    path: &str,
    locale: &str,
    tmpdir: &str,
) -> Result<ProjectRecord> {
    let project_dir = format!("{workspace}/{}", target.id());
    std::fs::create_dir_all(&project_dir)
        .map_err(|e| Error::Operation(format!("could not create directory {project_dir}: {e}")))?;
    let dependencies: String = roots
        .iter()
        .map(|r| format!("\"{}\": \"{}\"", r.name, r.version))
        .collect::<Vec<_>>()
        .join(",\n    ");
    let manifest_text = format!(
        "{{\n  \"name\": \"mynpm-resolution\",\n  \"version\": \"0.0.0\",\n  \"private\": true,\n  \"packageManager\": \"{pin}\",\n  \"dependencies\": {{\n    {dependencies}\n  }}\n}}\n"
    );
    let resolution_text = if *manager == Manager::Npm {
        manifest_text.clone()
    } else {
        format!(
            "{{\n  \"name\": \"mynpm-resolution\",\n  \"version\": \"0.0.0\",\n  \"private\": true,\n  \"dependencies\": {{\n    {dependencies}\n  }}\n}}\n"
        )
    };
    let manifest_path = format!("{project_dir}/package.json");
    std::fs::write(&manifest_path, &resolution_text)
        .map_err(|e| Error::Operation(format!("could not write package manifest: {manifest_path}: {e}")))?;
    let environment = tool_environment(workspace, target, &config.upstream_registry, path, locale, tmpdir);
    for dir in [&environment["HOME"], &environment["npm_config_cache"], &environment["pnpm_config_store_dir"]] {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Operation(format!("could not create directory {dir}: {e}")))?;
    }
    std::fs::write(
        &environment["npm_config_userconfig"],
        format!("registry={}\naudit=false\nfund=false\nignore-scripts=true\n", config.upstream_registry),
    )
    .map_err(|e| Error::Operation(format!("could not write package manager configuration: {}: {e}", environment["npm_config_userconfig"])))?;
    let manager_bin = manager.as_str();
    let version_out = run_process(
        manager_bin,
        &["--version".to_string()],
        &ProcessOptions {
            cwd: project_dir.clone(),
            env: environment.clone(),
            timeout_ms: 30_000,
            stdout_mode: crate::process::StdoutMode::Tail(16384),
        },
    )
    .map_err(|e| match e {
        Error::Input(_) => e,
        other => Error::Operation(format!("{other}")),
    })?;
    let found = version_out.stdout.split_whitespace().next().unwrap_or("").to_string();
    let expected = if *manager == Manager::Npm { NPM_VERSION } else { PNPM_VERSION };
    if found != expected {
        return Err(Error::Operation(format!(
            "{manager_bin} {expected} is required for add; found {}",
            if found.is_empty() { "no version output".to_string() } else { found }
        )));
    }
    let mut args: Vec<String> = if *manager == Manager::Npm {
        ["install", "--package-lock-only", "--ignore-scripts", "--no-audit", "--no-fund", "--save-exact"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        ["install", "--lockfile-only", "--ignore-scripts"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    for root in roots {
        args.push(format!("{}@{}", root.name, root.version));
    }
    run_process(
        manager_bin,
        &args,
        &ProcessOptions {
            cwd: project_dir.clone(),
            env: environment,
            timeout_ms: OPERATION_TIMEOUT_MS,
            stdout_mode: crate::process::StdoutMode::Tail(16384),
        },
    )?;
    let lockfile_name = if *manager == Manager::Npm { "package-lock.json" } else { "pnpm-lock.yaml" };
    let lockfile_path = format!("{project_dir}/{lockfile_name}");
    let lock_text = std::fs::read_to_string(&lockfile_path)
        .map_err(|_| Error::Operation(format!("{manager_bin} did not create {lockfile_name}")))?;
    let snapshot = crate::inventory::ProjectSnapshot {
        manifest: Some(manifest_text),
        lockfile: lock_text.clone(),
        workspace_files: Vec::new(),
        npm_install_options: crate::inventory::NpmInstallOptions {
            legacy_peer_deps: None,
            install_links: None,
        },
    };
    assert_restorable_snapshot(manager, &snapshot)
        .map_err(|e| Error::Input(format!("generated {manager_bin} project is not restorable: {e}")))?;
    let target_ids = vec![target.id().to_string()];
    let project = if *manager == Manager::Npm {
        parse_npm_lock(
            &lock_text,
            NpmOptions {
                project_id: format!("add-{manager_bin}-{}", target.id()),
                package_manager_pin: Some(pin.to_string()),
                lockfile_path: format!("add/{manager_bin}/{}/{lockfile_name}", target.id()),
                target_ids,
                snapshot: Some(snapshot),
            },
        )
        .map_err(|e| Error::Input(format!("could not parse {manager_bin} lockfile: {e}")))?
    } else {
        parse_pnpm_lock(
            &lock_text,
            PnpmOptions {
                project_id: format!("add-{manager_bin}-{}", target.id()),
                package_manager_pin: Some(pin.to_string()),
                lockfile_path: format!("add/{manager_bin}/{}/{lockfile_name}", target.id()),
                target_ids,
                snapshot: Some(snapshot),
            },
        )
        .map_err(|e| match e {
            Error::Input(_) => e,
            other => Error::Input(format!("could not parse {manager_bin} lockfile: {other}")),
        })?
    };
    verify_root_selections(&project, roots, target.id(), manager).map_err(|e| {
        Error::Input(format!("resolved project did not match requested package pins: {e}"))
    })?;
    if let Some(unsupported) = project.packages.iter().find(|p| p.source_kind != SourceKind::Registry) {
        let version_suffix = unsupported.version.clone().map(|v| format!("@{v}")).unwrap_or_default();
        return Err(Error::Input(format!(
            "add rejected {} source {}{version_suffix}",
            unsupported.source_kind.as_str(),
            unsupported.name
        )));
    }
    Ok(project)
}

pub struct AddOptions {
    pub manager: Manager,
    pub specs: Vec<String>,
    pub requested_targets: Vec<String>,
    pub config: Config,
    pub state_directory: String,
    pub cwd: String,
}

pub fn add_packages(options: &AddOptions) -> Result<()> {
    let roots = parse_root_pins(&options.specs).map_err(|e| Error::Input(format!("add requires exact package pins: {e}")))?;
    let targets = targets_for(&options.config, &options.requested_targets)
        .map_err(|e| Error::Input(format!("requested targets are invalid: {e}")))?;
    let path = std::env::var("PATH").unwrap_or_default();
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".to_string());
    let scratch_root = format!("{}/.work/tmp", options.cwd);
    std::fs::create_dir_all(&scratch_root)
        .map_err(|e| Error::Operation(format!("could not create resolver workspace under {scratch_root}: {e}")))?;
    let workspace = format!("{}/mynpm-add-{}-{}", scratch_root, std::process::id(), nanos());
    std::fs::create_dir_all(&workspace)
        .map_err(|e| Error::Operation(format!("could not create resolver workspace under {scratch_root}: {e}")))?;
    let result = (|| -> Result<Vec<ProjectRecord>> {
        let mut projects = Vec::new();
        let manager_bin = options.manager.as_str();
        let pin = format!("{manager_bin}@{}", if options.manager == Manager::Npm { NPM_VERSION } else { PNPM_VERSION });
        for target in &targets {
            projects.push(resolve_target(
                &options.manager,
                &pin,
                &roots,
                target,
                &options.config,
                &workspace,
                &path,
                &locale,
                &scratch_root,
            )?);
        }
        Ok(projects)
    })();
    let _ = std::fs::remove_dir_all(&workspace);
    let projects = result?;
    let manager_bin = options.manager.as_str();
    let pin = format!("{manager_bin}@{}", if options.manager == Manager::Npm { NPM_VERSION } else { PNPM_VERSION });
    update_inventory(std::path::Path::new(&options.state_directory), |inv| {
        let mut acc = inv.clone();
        for project in &projects {
            acc = crate::inventory::merge_project(&acc, project.clone())?;
        }
        Ok(acc)
    })?;
    println!(
        "Added {} root pins for {} with {pin}.",
        roots.len(),
        targets.iter().map(|t| t.id()).collect::<Vec<_>>().join(", ")
    );
    Ok(())
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root_pins() {
        let roots = parse_root_pins(&["b@1.0.0".to_string(), "@scope/a@2.3.4".to_string()]).unwrap();
        assert_eq!(roots[0].name, "@scope/a");
        assert!(parse_root_pins(&["b@^1.0.0".to_string()]).is_err());
        assert!(parse_root_pins(&["b@1.0.0".to_string(), "b@2.0.0".to_string()]).is_err());
    }

    #[test]
    fn rejects_unknown_targets() {
        let config = Config {
            public_base_url: "https://example.test/mirror/".to_string(),
            upstream_registry: "https://registry.npmjs.org/".to_string(),
            targets: vec![crate::config::Target::Darwin(crate::config::DarwinTarget {
                id: "mac-arm64".to_string(),
                cpu: "arm64".to_string(),
                node: "26.12.0".to_string(),
            })],
            policy: crate::config::Policy { allow_legacy_sha1_only: false },
        };
        assert!(targets_for(&config, &["nope".to_string()]).is_err());
        assert_eq!(targets_for(&config, &[]).unwrap().len(), 1);
    }
}
