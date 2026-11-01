#![forbid(unsafe_code)]

use crate::acquire::{fetch_inventory, HttpsExchange};
use crate::config::{Config, Target};
use crate::doctor::{load_package_manager_pin, run_doctor};
use crate::error::{Error, Result};
use crate::inventory::Manager;
use crate::inventory_store::{load_inventory, update_inventory};
use crate::lock_npm::{parse_npm_lock, NpmOptions};
use crate::lock_pnpm::{parse_pnpm_lock, PnpmOptions};
use crate::project_snapshot::{capture_project_snapshot, CaptureOptions};
use crate::publish::{adopt_site, publish_site, PublishOptions};
use crate::resolve::{add_packages, AddOptions};
use crate::validate::{validate_inventory, ValidateOptions};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    ImportLock,
    Add,
    Plan,
    Fetch,
    Verify,
    Publish,
    Adopt,
    Validate,
    Doctor,
    InitConfig,
    Pins,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub config: Option<String>,
    pub state_dir: Option<String>,
    pub site_dir: Option<String>,
    pub project: Option<String>,
    pub manager: Option<String>,
    pub client: Option<String>,
    pub node_version: Option<String>,
    pub public_url: Option<String>,
    pub upstream_registry: Option<String>,
    pub output: Option<String>,
    pub json: bool,
    pub smoke_executables: bool,
    pub all_targets: bool,
    pub targets: Vec<String>,
    pub positionals: Vec<String>,
}

fn is_command(s: &str) -> Option<Command> {
    match s {
        "import-lock" => Some(Command::ImportLock),
        "add" => Some(Command::Add),
        "plan" => Some(Command::Plan),
        "fetch" => Some(Command::Fetch),
        "verify" => Some(Command::Verify),
        "publish" => Some(Command::Publish),
        "adopt" => Some(Command::Adopt),
        "validate" => Some(Command::Validate),
        "doctor" => Some(Command::Doctor),
        "init-config" => Some(Command::InitConfig),
        "pins" => Some(Command::Pins),
        _ => None,
    }
}

fn is_manager(s: &str) -> bool {
    s == "npm" || s == "pnpm"
}

fn manager_of(s: &str) -> Manager {
    if s == "npm" {
        Manager::Npm
    } else {
        Manager::Pnpm
    }
}

pub fn parse_args(argv: &[String]) -> Result<Parsed> {
    let mut command: Option<Command> = None;
    let mut config = None;
    let mut state_dir = None;
    let mut site_dir = None;
    let mut project = None;
    let mut manager = None;
    let mut client = None;
    let mut node_version = None;
    let mut public_url = None;
    let mut upstream_registry = None;
    let mut output = None;
    let mut json = false;
    let mut smoke = false;
    let mut all_targets = false;
    let mut targets: Vec<String> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        if command.is_none() && is_command(arg).is_some() {
            command = is_command(arg);
            i += 1;
            continue;
        }
        if arg == "--json" {
            if json {
                return Err(Error::Usage("--json may only be specified once".to_string()));
            }
            json = true;
            i += 1;
            continue;
        }
        if arg == "--smoke-executables" {
            if smoke {
                return Err(Error::Usage(
                    "--smoke-executables may only be specified once".to_string(),
                ));
            }
            smoke = true;
            i += 1;
            continue;
        }
        if arg == "--all-targets" {
            if all_targets {
                return Err(Error::Usage("--all-targets may only be specified once".to_string()));
            }
            all_targets = true;
            i += 1;
            continue;
        }
        if ["--config", "--state-dir", "--site-dir", "--project", "--manager", "--client", "--target", "--node-version", "--public-url", "--upstream-registry", "--output"]
            .contains(&arg)
        {
            let value = argv.get(i + 1).ok_or_else(|| Error::Usage(format!("{arg} requires a value")))?;
            if value.starts_with("--") {
                return Err(Error::Usage(format!("{arg} requires a value")));
            }
            i += 2;
            if arg == "--target" {
                targets.push(value.clone());
                continue;
            }
            if !seen.insert(arg.to_string()) {
                return Err(Error::Usage(format!("{arg} may only be specified once")));
            }
            match arg {
                "--config" => config = Some(value.clone()),
                "--state-dir" => state_dir = Some(value.clone()),
                "--site-dir" => site_dir = Some(value.clone()),
                "--project" => project = Some(value.clone()),
                "--manager" => manager = Some(value.clone()),
                "--client" => client = Some(value.clone()),
                "--node-version" => node_version = Some(value.clone()),
                "--public-url" => public_url = Some(value.clone()),
                "--upstream-registry" => upstream_registry = Some(value.clone()),
                "--output" => output = Some(value.clone()),
                _ => {}
            }
            continue;
        }
        if arg.starts_with('-') {
            return Err(Error::Usage(format!("unknown option: {arg}")));
        }
        if command.is_none() {
            return Err(Error::Usage(format!("unknown command: {arg}")));
        }
        positionals.push(arg.to_string());
        i += 1;
    }

    let command = command.ok_or_else(|| Error::Usage("a command is required".to_string()))?;
    if json && command != Command::Plan {
        return Err(Error::Usage("--json is only valid with plan".to_string()));
    }
    if project.is_some() && command != Command::ImportLock {
        return Err(Error::Usage("--project is only valid with import-lock".to_string()));
    }
    if manager.is_some() && command != Command::Add {
        return Err(Error::Usage("--manager is only valid with add".to_string()));
    }
    if client.is_some() && command != Command::Validate {
        return Err(Error::Usage("--client is only valid with validate".to_string()));
    }
    if smoke && command != Command::Validate {
        return Err(Error::Usage("--smoke-executables is only valid with validate".to_string()));
    }
    if all_targets && command != Command::InitConfig {
        return Err(Error::Usage("--all-targets is only valid with init-config".to_string()));
    }
    if site_dir.is_some() && command != Command::Publish && command != Command::Adopt {
        return Err(Error::Usage("--site-dir is only valid with publish or adopt".to_string()));
    }
    if node_version.is_some() && command != Command::InitConfig {
        return Err(Error::Usage("--node-version is only valid with init-config".to_string()));
    }
    if public_url.is_some() && command != Command::InitConfig {
        return Err(Error::Usage("--public-url is only valid with init-config".to_string()));
    }
    if upstream_registry.is_some() && command != Command::InitConfig {
        return Err(Error::Usage("--upstream-registry is only valid with init-config".to_string()));
    }
    if output.is_some() && command != Command::InitConfig {
        return Err(Error::Usage("--output is only valid with init-config".to_string()));
    }
    if config.is_some() && (command == Command::InitConfig || command == Command::Adopt || command == Command::Pins) {
        return Err(Error::Usage("--config is not valid with init-config, adopt, or pins".to_string()));
    }
    if state_dir.is_some() && (command == Command::InitConfig || command == Command::Adopt || command == Command::Pins) {
        return Err(Error::Usage("--state-dir is not valid with init-config, adopt, or pins".to_string()));
    }
    if !targets.is_empty() && command != Command::ImportLock && command != Command::Add {
        return Err(Error::Usage(
            "--target is only valid with import-lock or add".to_string(),
        ));
    }
    match command {
        Command::ImportLock => {
            if positionals.len() != 1 {
                return Err(Error::Usage("import-lock requires one lockfile path".to_string()));
            }
            if project.is_none() {
                return Err(Error::Usage("import-lock requires --project".to_string()));
            }
        }
        Command::Add => {
            if positionals.is_empty() {
                return Err(Error::Usage("add requires at least one exact package pin".to_string()));
            }
            match manager.as_deref() {
                Some(m) if is_manager(m) => {}
                _ => return Err(Error::Usage("add requires --manager npm or pnpm".to_string())),
            }
        }
        Command::Plan | Command::Fetch | Command::Verify | Command::Publish | Command::Validate => {
            if !positionals.is_empty() {
                let name = match command {
                    Command::Plan => "plan",
                    Command::Fetch => "fetch",
                    Command::Verify => "verify",
                    Command::Publish => "publish",
                    _ => "validate",
                };
                return Err(Error::Usage(format!("{name} does not take positional arguments")));
            }
            if command == Command::Validate {
                match client.as_deref() {
                    Some(c) if is_manager(c) => {}
                    _ => {
                        return Err(Error::Usage(
                            "validate requires --client npm or pnpm".to_string(),
                        ));
                    }
                }
            }
        }
        Command::Doctor => {
            if positionals.len() != 1 {
                return Err(Error::Usage("doctor requires one project path".to_string()));
            }
        }
        Command::Adopt => {
            if !positionals.is_empty() {
                return Err(Error::Usage("adopt does not take positional arguments".to_string()));
            }
            if site_dir.is_none() {
                return Err(Error::Usage("adopt requires --site-dir".to_string()));
            }
        }
        Command::InitConfig => {
            if !positionals.is_empty() {
                return Err(Error::Usage("init-config does not take positional arguments".to_string()));
            }
            if node_version.is_none() {
                return Err(Error::Usage("init-config requires --node-version".to_string()));
            }
        }
        Command::Pins => {
            if !positionals.is_empty() {
                return Err(Error::Usage("pins does not take positional arguments".to_string()));
            }
        }
    }
    Ok(Parsed {
        command,
        config,
        state_dir,
        site_dir,
        project,
        manager,
        client,
        node_version,
        public_url,
        upstream_registry,
        output,
        json,
        smoke_executables: smoke,
        all_targets,
        targets,
        positionals,
    })
}

pub fn print_help() {
    println!("mynpm");
    println!();
    println!("Usage: mynpm [--config path] [--state-dir path] [--site-dir path] [--target id] command");
    println!();
    println!("Commands:");
    println!("  init-config --node-version V [--all-targets] [--public-url URL] [--upstream-registry URL] [--output FILE]");
    println!("  import-lock <lockfile> --project NAME [--target T]...");
    println!("  add <name@version>... --manager npm|pnpm [--target T]...");
    println!("  plan [--json]");
    println!("  fetch");
    println!("  verify");
    println!("  publish [--site-dir DIR]");
    println!("  adopt --site-dir DIR");
    println!("  validate --client npm|pnpm [--smoke-executables]");
    println!("  doctor <projectDir>");
    println!("  pins");
}

fn current_target_id(config: &Config) -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => Some("darwin"),
        "linux" => Some("linux"),
        _ => None,
    };
    let cpu = match std::env::consts::ARCH {
        "aarch64" => Some("arm64"),
        "x86_64" => Some("x64"),
        _ => None,
    };
    let libc: Option<&str> = if os == Some("linux") {
        Some(if cfg!(target_env = "musl") { "musl" } else { "glibc" })
    } else {
        None
    };
    config
        .targets
        .iter()
        .find(|t| {
            Some(t.os()) == os && Some(t.cpu()) == cpu
                && match t {
                    Target::Linux(l) => Some(l.libc.as_str()) == libc,
                    Target::Darwin(_) => libc.is_none(),
                }
        })
        .map(|t| t.id().to_string())
        .ok_or_else(|| {
            Error::Input(format!(
                "the current platform is not configured as a target: {}/{}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                libc.unwrap_or("")
            ))
        })
}

fn target_ids_for(config: &Config, requested: &[String]) -> Result<Vec<String>> {
    let ids = if requested.is_empty() {
        vec![current_target_id(config)?]
    } else {
        requested.to_vec()
    };
    let available: BTreeSet<&str> = config.targets.iter().map(|t| t.id()).collect();
    for id in &ids {
        if !available.contains(id.as_str()) {
            return Err(Error::Input(format!("target is not configured: {id}")));
        }
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != ids.len() {
        return Err(Error::Input("target IDs must be unique".to_string()));
    }
    Ok(sorted)
}

type TargetSpec = (&'static str, &'static str, &'static str, Option<&'static str>);

const DEFAULT_TARGETS: &[TargetSpec] = &[
    ("mac-arm64", "darwin", "arm64", None),
    ("linux-x64-gnu", "linux", "x64", Some("glibc")),
];

const ALL_TARGETS: &[TargetSpec] = &[
    ("mac-arm64", "darwin", "arm64", None),
    ("mac-x64", "darwin", "x64", None),
    ("linux-x64-gnu", "linux", "x64", Some("glibc")),
    ("linux-x64-musl", "linux", "x64", Some("musl")),
    ("linux-arm64-gnu", "linux", "arm64", Some("glibc")),
    ("linux-arm64-musl", "linux", "arm64", Some("musl")),
];

fn run_init_config(parsed: &Parsed) -> Result<()> {
    let node = parsed.node_version.clone().unwrap_or_default();
    if node.split('.').count() != 3
        || !node.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0')))
    {
        return Err(Error::Input(format!("--node-version must be an exact major.minor.patch version: {node}")));
    }
    let public_url = parsed.public_url.clone().unwrap_or_else(|| "https://npm.17166969.xyz".to_string());
    let upstream = parsed.upstream_registry.clone().unwrap_or_else(|| "https://registry.npmjs.org/".to_string());
    if !public_url.starts_with("https://") || public_url.contains('@') || public_url.contains('?') || public_url.contains('#') {
        return Err(Error::Input("--public-url must be an HTTPS URL without credentials, query, or fragment".to_string()));
    }
    let base = format!("{}/", public_url.trim_end_matches('/'));
    let targets = if parsed.all_targets { ALL_TARGETS } else { DEFAULT_TARGETS };
    let target_lines: Vec<String> = targets
        .iter()
        .map(|(id, os, cpu, libc)| match libc {
            Some(libc) => format!("    {{\"id\": \"{id}\", \"os\": \"{os}\", \"cpu\": \"{cpu}\", \"libc\": \"{libc}\", \"node\": \"{node}\"}}"),
            None => format!("    {{\"id\": \"{id}\", \"os\": \"{os}\", \"cpu\": \"{cpu}\", \"node\": \"{node}\"}}"),
        })
        .collect();
    let text = format!(
        "{{\n  \"schemaVersion\": 1,\n  \"publicBaseUrl\": \"{base}\",\n  \"upstreamRegistry\": \"{upstream}\",\n  \"targets\": [\n{}\n  ],\n  \"policy\": {{\n    \"externalSources\": \"reject\",\n    \"remoteInstallScripts\": \"report\",\n    \"allowLegacySha1Only\": false\n  }}\n}}\n",
        target_lines.join(",\n")
    );
    crate::config::parse_config(&text)?;
    let output = parsed.output.clone().unwrap_or_else(|| "mynpm.json".to_string());
    std::fs::write(&output, text.as_bytes())
        .map_err(|e| Error::Operation(format!("could not write configuration file {output}: {e}")))?;
    println!("Wrote {output}.");
    Ok(())
}

fn run_import_lock(parsed: &Parsed, config: &Config, cwd: &Path) -> Result<()> {
    let lockfile_arg = parsed.positionals[0].clone();
    let lockfile_path = if Path::new(&lockfile_arg).is_absolute() {
        PathBuf::from(&lockfile_arg)
    } else {
        cwd.join(&lockfile_arg)
    };
    let text = std::fs::read_to_string(&lockfile_path)
        .map_err(|e| Error::Input(format!("lockfile does not exist: {}: {e}", lockfile_path.display())))?;
    let project_dir = lockfile_path.parent().unwrap_or(cwd);
    let pin = load_package_manager_pin(project_dir)?;
    let lock_manager = if lockfile_arg.ends_with(".yaml") || lockfile_arg.ends_with(".yml") {
        Manager::Pnpm
    } else {
        Manager::Npm
    };
    if let Some(pin) = &pin
        && pin.manager != lock_manager {
            return Err(Error::Input(format!(
                "packageManager pin {} does not match {} lockfile",
                pin.raw,
                lock_manager.as_str()
            )));
        }
    let project_id = parsed.project.clone().unwrap_or_default();
    let targets = target_ids_for(config, &parsed.targets)?;
    let lockfile_rel = lockfile_path
        .strip_prefix(cwd)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| lockfile_arg.clone());
    if lock_manager == Manager::Npm {
        let initial = parse_npm_lock(
            &text,
            NpmOptions {
                project_id: project_id.clone(),
                package_manager_pin: pin.as_ref().map(|p| p.raw.clone()),
                lockfile_path: lockfile_rel.clone(),
                target_ids: targets.clone(),
                snapshot: None,
            },
        )?;
        let snapshot = capture_project_snapshot(&CaptureOptions {
            manager: &Manager::Npm,
            project_directory: project_dir.to_str().unwrap_or(""),
            lockfile_text: &text,
            project: &initial,
        })?;
        let project = parse_npm_lock(
            &text,
            NpmOptions {
                project_id: project_id.clone(),
                package_manager_pin: pin.as_ref().map(|p| p.raw.clone()),
                lockfile_path: lockfile_rel,
                target_ids: targets.clone(),
                snapshot: Some(snapshot),
            },
        )?;
        let state_dir = cwd.join(parsed.state_dir.clone().unwrap_or_else(|| "state".to_string()));
        update_inventory(&state_dir, |inv| {
            let mut acc = inv.clone();
            acc = crate::inventory::merge_project(&acc, project.clone())?;
            Ok(acc)
        })?;
        println!(
            "Imported {} package entries for {} ({}).",
            project.packages.len(),
            project.id,
            targets.join(", ")
        );
    } else {
        let initial = parse_pnpm_lock(
            &text,
            PnpmOptions {
                project_id: project_id.clone(),
                package_manager_pin: pin.as_ref().map(|p| p.raw.clone()),
                lockfile_path: lockfile_rel.clone(),
                target_ids: targets.clone(),
                snapshot: None,
            },
        )?;
        let snapshot = capture_project_snapshot(&CaptureOptions {
            manager: &Manager::Pnpm,
            project_directory: project_dir.to_str().unwrap_or(""),
            lockfile_text: &text,
            project: &initial,
        })?;
        let project = parse_pnpm_lock(
            &text,
            PnpmOptions {
                project_id: project_id.clone(),
                package_manager_pin: pin.as_ref().map(|p| p.raw.clone()),
                lockfile_path: lockfile_rel,
                target_ids: targets.clone(),
                snapshot: Some(snapshot),
            },
        )?;
        let state_dir = cwd.join(parsed.state_dir.clone().unwrap_or_else(|| "state".to_string()));
        update_inventory(&state_dir, |inv| {
            let mut acc = inv.clone();
            acc = crate::inventory::merge_project(&acc, project.clone())?;
            Ok(acc)
        })?;
        println!(
            "Imported {} package entries for {} ({}).",
            project.packages.len(),
            project.id,
            targets.join(", ")
        );
    }
    if let Some(pin) = &pin {
        println!("Package manager pin: {}", pin.raw);
    }
    Ok(())
}

fn run_plan(parsed: &Parsed, config: &Config, cwd: &Path) -> Result<()> {
    let state_dir = cwd.join(parsed.state_dir.clone().unwrap_or_else(|| "state".to_string()));
    let inventory = load_inventory(&state_dir)?;
    if parsed.json {
        let mut map = deser_value::Map::new();
        map.insert("schemaVersion".to_string(), deser_value::Value::from(1u64));
        map.insert("publicBaseUrl".to_string(), deser_value::Value::from(config.public_base_url.clone()));
        let mut targets = deser_value::Seq::new();
        let mut ids: Vec<String> = config.targets.iter().map(|t| t.id().to_string()).collect();
        ids.sort();
        for id in &ids {
            targets.push(deser_value::Value::from(id.clone()));
        }
        map.insert("targets".to_string(), deser_value::Value::from(targets));
        map.insert("projectCount".to_string(), deser_value::Value::from(inventory.projects.len() as u64));
        let identities = crate::inventory::package_identities(&inventory);
        map.insert("packageCount".to_string(), deser_value::Value::from(identities.len() as u64));
        let mut packages = deser_value::Seq::new();
        for (name, version, kinds, targets) in &identities {
            let mut entry = deser_value::Map::new();
            entry.insert("name".to_string(), deser_value::Value::from(name.clone()));
            entry.insert("version".to_string(), deser_value::Value::from(version.clone()));
            let mut kinds_seq = deser_value::Seq::new();
            let mut kinds = kinds.clone();
            kinds.sort();
            for kind in &kinds {
                kinds_seq.push(deser_value::Value::from(kind.clone()));
            }
            entry.insert("sourceKinds".to_string(), deser_value::Value::from(kinds_seq));
            let mut targets_seq = deser_value::Seq::new();
            let mut targets = targets.clone();
            targets.sort();
            for target in &targets {
                targets_seq.push(deser_value::Value::from(target.clone()));
            }
            entry.insert("targets".to_string(), deser_value::Value::from(targets_seq));
            packages.push(deser_value::Value::from(entry));
        }
        map.insert("packages".to_string(), deser_value::Value::from(packages));
        let mut blocked = deser_value::Seq::new();
        for (name, version, kinds, targets) in &identities {
            if kinds.iter().any(|k| k != "registry" && k != "local") {
                let mut entry = deser_value::Map::new();
                entry.insert("name".to_string(), deser_value::Value::from(name.clone()));
                entry.insert("version".to_string(), deser_value::Value::from(version.clone()));
                let mut kinds_seq = deser_value::Seq::new();
                let mut kinds = kinds.clone();
                kinds.sort();
                for kind in &kinds {
                    kinds_seq.push(deser_value::Value::from(kind.clone()));
                }
                entry.insert("sourceKinds".to_string(), deser_value::Value::from(kinds_seq));
                let mut targets_seq = deser_value::Seq::new();
                let mut targets = targets.clone();
                targets.sort();
                for target in &targets {
                    targets_seq.push(deser_value::Value::from(target.clone()));
                }
                entry.insert("targets".to_string(), deser_value::Value::from(targets_seq));
                blocked.push(deser_value::Value::from(entry));
            }
        }
        map.insert("blockedSources".to_string(), deser_value::Value::from(blocked));
        let mut value = deser_value::Value::from(map);
        crate::inventory::sort_canonical_value(&mut value);
        print!("{}", crate::inventory::render_pretty(&value));
        println!();
        return Ok(());
    }
    let identities = crate::inventory::package_identities(&inventory);
    let mut ids: Vec<String> = config.targets.iter().map(|t| t.id().to_string()).collect();
    ids.sort();
    let blocked = identities.iter().filter(|(_, _, kinds, _)| kinds.iter().any(|k| k != "registry" && k != "local")).count();
    println!("Static registry: {}", config.public_base_url);
    println!("Configured targets: {}", ids.join(", "));
    println!("Imported projects: {}", inventory.projects.len());
    println!("Observed package versions: {}", identities.len());
    println!("Sources requiring review: {blocked}");
    Ok(())
}

fn print_pins() {
    for manager in [Manager::Npm, Manager::Pnpm] {
        println!("{} {}", manager.as_str(), manager.pinned_version());
    }
}

fn print_publish_summary(summary: &crate::publish::PublishSummary) {
    println!(
        "{} generation {} ({} packages, {} versions, {} metadata-only versions).",
        if summary.no_op { "Already published" } else { "Published" },
        summary.generation_id,
        summary.package_count,
        summary.version_count,
        summary.metadata_only_count
    );
}

pub fn run(argv: &[String]) -> Result<()> {
    let parsed = parse_args(argv)?;
    if parsed.command == Command::Pins {
        print_pins();
        return Ok(());
    }
    if parsed.command == Command::InitConfig {
        return run_init_config(&parsed);
    }
    if parsed.command == Command::Adopt {
        let summary = adopt_site(Path::new(parsed.site_dir.as_deref().unwrap_or_default()))?;
        print_publish_summary(&summary);
        return Ok(());
    }
    let cwd = std::env::current_dir().map_err(|e| Error::Operation(format!("could not resolve working directory: {e}")))?;
    let config_path = cwd.join(parsed.config.clone().unwrap_or_else(|| "mynpm.json".to_string()));
    let config_text = std::fs::read_to_string(&config_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Input(format!("configuration file does not exist: {}", config_path.display()))
        } else {
            Error::Operation(format!("could not read configuration file: {}", config_path.display()))
        }
    })?;
    let config = crate::config::parse_config(&config_text)?;
    let state_dir = cwd.join(parsed.state_dir.clone().unwrap_or_else(|| "state".to_string()));
    match parsed.command {
        Command::InitConfig => run_init_config(&parsed),
        Command::Plan => run_plan(&parsed, &config, &cwd),
        Command::ImportLock => run_import_lock(&parsed, &config, &cwd),
        Command::Doctor => {
            let project_path = if Path::new(&parsed.positionals[0]).is_absolute() {
                PathBuf::from(&parsed.positionals[0])
            } else {
                cwd.join(&parsed.positionals[0])
            };
            run_doctor(&project_path, &config)
        }
        Command::Add => {
            let manager = manager_of(parsed.manager.as_deref().unwrap_or(""));
            add_packages(&AddOptions {
                manager,
                specs: parsed.positionals.clone(),
                requested_targets: parsed.targets.clone(),
                config: config.clone(),
                state_directory: state_dir.to_string_lossy().to_string(),
                cwd: cwd.to_string_lossy().to_string(),
            })
        }
        Command::Fetch => {
            let summary = fetch_inventory(&state_dir, &config, &HttpsExchange::new())?;
            println!(
                "Verified {} package artifacts; downloaded {} bytes, reused {}; {} metadata-only versions ({} newly recorded).",
                summary.verified_count,
                summary.downloaded_bytes,
                summary.reused_count,
                summary.metadata_only_count,
                summary.metadata_only_recorded
            );
            Ok(())
        }
        Command::Verify => {
            let verified = crate::acquire::verify_inventory(&state_dir, &config)?;
            println!(
                "Verified {} package artifacts and {} metadata-only versions.",
                verified.artifact_count, verified.metadata_only_count
            );
            Ok(())
        }
        Command::Adopt => unreachable!("adopt returns before the configuration is read"),
        Command::Pins => unreachable!("pins returns before the configuration is read"),
        Command::Publish => {
            let site_dir = cwd.join(parsed.site_dir.clone().unwrap_or_else(|| ".work/site".to_string()));
            let summary = publish_site(&PublishOptions {
                state_directory: &state_dir,
                site_directory: &site_dir,
                config: &config,
                render_base_url: None,
            })?;
            print_publish_summary(&summary);
            Ok(())
        }
        Command::Validate => {
            let client = manager_of(parsed.client.as_deref().unwrap_or(""));
            let summary = validate_inventory(&ValidateOptions {
                client,
                state_directory: &state_dir,
                config: &config,
                cwd: &cwd,
                smoke_executables: parsed.smoke_executables,
            })?;
            println!(
                "Restored {}@{} on Node {} for {}: {} package selections across {} projects; skipped {} platform-inapplicable optional instances; served {} local HTTP requests.",
                summary.client,
                summary.version,
                summary.node_version,
                summary.target,
                summary.package_count,
                summary.project_count,
                summary.platform_skipped_count,
                summary.request_count
            );
            for (name, output) in &summary.executables {
                println!("Executed {name}: {output}");
            }
            Ok(())
        }
    }
}
