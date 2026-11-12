#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Add,
    Plan,
    Build,
    Validate,
    Publish,
    Republish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub catalog: Option<String>,
    pub state_dir: Option<String>,
    pub config: Option<String>,
    pub output: Option<String>,
    pub public_url: Option<String>,
    pub platforms: Vec<String>,
    pub build_index: Option<String>,
    pub strict_build_deps: bool,
    pub build_constraints: Option<String>,
    pub require_build_hashes: bool,
    pub uv_locks: Vec<String>,
    pub requirements_files: Vec<String>,
    pub positionals: Vec<String>,
}

fn fail(detail: impl Into<String>) -> Error {
    Error::new(Kind::InvalidInput, detail, Stage::Command)
}

pub fn parse_args(argv: &[String]) -> Result<Parsed> {
    let mut command: Option<Command> = None;
    let mut catalog = None;
    let mut state_dir = None;
    let mut config = None;
    let mut output = None;
    let mut public_url = None;
    let mut platforms: Vec<String> = Vec::new();
    let mut build_index = None;
    let mut strict = false;
    let mut constraints = None;
    let mut require_hashes = false;
    let mut uv_locks: Vec<String> = Vec::new();
    let mut requirements_files: Vec<String> = Vec::new();
    let mut positionals = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        if command.is_none() {
            match arg {
                "add" => {
                    command = Some(Command::Add);
                    i += 1;
                    continue;
                }
                "plan" => {
                    command = Some(Command::Plan);
                    i += 1;
                    continue;
                }
                "build" => {
                    command = Some(Command::Build);
                    i += 1;
                    continue;
                }
                "validate" => {
                    command = Some(Command::Validate);
                    i += 1;
                    continue;
                }
                "publish" => {
                    command = Some(Command::Publish);
                    i += 1;
                    continue;
                }
                "republish" => {
                    command = Some(Command::Republish);
                    i += 1;
                    continue;
                }
                _ => {}
            }
        }
        match arg {
            "--catalog" | "--state-dir" | "--config" | "--output" | "--public-url" | "--platform"
            | "--build-index" | "--build-constraints" | "--uv-lock" | "--requirements" => {
                let value = argv.get(i + 1).ok_or_else(|| fail(format!("{arg} requires a value")))?;
                i += 2;
                match arg {
                    "--catalog" => catalog = Some(value.clone()),
                    "--state-dir" => state_dir = Some(value.clone()),
                    "--config" => config = Some(value.clone()),
                    "--output" => output = Some(value.clone()),
                    "--public-url" => public_url = Some(value.clone()),
                    "--platform" => platforms.push(value.clone()),
                    "--build-index" => build_index = Some(value.clone()),
                    "--uv-lock" => uv_locks.push(value.clone()),
                    "--requirements" => requirements_files.push(value.clone()),
                    _ => constraints = Some(value.clone()),
                }
            }
            "--strict-build-deps" => {
                strict = true;
                i += 1;
            }
            "--require-build-hashes" => {
                require_hashes = true;
                i += 1;
            }
            _ if arg.starts_with('-') => return Err(fail(format!("unknown option: {arg}"))),
            _ => {
                if command.is_none() {
                    return Err(fail(format!("unknown command: {arg}")));
                }
                positionals.push(arg.to_string());
                i += 1;
            }
        }
    }
    let command = command.ok_or_else(|| fail("a command is required"))?;
    match command {
        Command::Add if positionals.is_empty() && uv_locks.is_empty() && requirements_files.is_empty() => {
            return Err(fail("add requires at least one requirement, --uv-lock, or --requirements"));
        }
        Command::Add => {}
        _ if !positionals.is_empty() => {
            return Err(fail("this command does not take positional arguments"));
        }
        _ => {}
    }
    Ok(Parsed {
        command,
        catalog,
        state_dir,
        config,
        output,
        public_url,
        platforms,
        build_index,
        strict_build_deps: strict,
        build_constraints: constraints,
        require_build_hashes: require_hashes,
        uv_locks,
        requirements_files,
        positionals,
    })
}

pub fn print_help() {
    println!("mypypi");
    println!();
    println!("Usage: mypypi [OPTIONS] <COMMAND> [OPTIONS]");
    println!();
    println!("Commands:");
    println!("  add [--uv-lock FILE]... [--requirements FILE]... [<requirement>...]");
    println!("  plan [--output DIR] [--platform P]...");
    println!("  build [--output DIR] [--platform P]... [--build-index URL] [--strict-build-deps]");
    println!("  validate [--output DIR] [--platform P]...");
    println!("  publish [--output DIR] [--platform P]...");
    println!("  republish --output DIR --public-url URL");
}

pub fn print_command_help(command: &Command) {
    match command {
        Command::Add => {
            println!("mypypi add [--uv-lock FILE]... [--requirements FILE]... [<requirement>...]");
            println!("resolve exact package pins, including registry packages of uv.lock files, into the catalog");
        }
        Command::Plan => {
            println!("mypypi plan [--output DIR] [--platform P]...");
            println!("show the wheel work required by the catalog");
        }
        Command::Build => {
            println!("mypypi build [--output DIR] [--platform P]... [--build-index URL] [--strict-build-deps] [--build-constraints PATH] [--require-build-hashes]");
            println!("acquire or build missing wheels");
        }
        Command::Validate => {
            println!("mypypi validate [--output DIR] [--platform P]...");
            println!("validate pending wheels");
        }
        Command::Publish => {
            println!("mypypi publish --output DIR [--platform P]...");
            println!("publish validated wheels to a local index");
        }
        Command::Republish => {
            println!("mypypi republish --output DIR --public-url URL");
            println!("render the published index again under the current renderer, adding no wheels");
        }
    }
}

// --- Dispatch ---

use crate::acquire::{Release, fetch_release, validate_wheel_architectures, validate_wheel_metadata};
use crate::build::{BuildOptions, acquire_or_build, require_native_target, validate_build_index};
use crate::catalog::{Catalog, parse_catalog};
use crate::config::{ProjectSettings, load_project_settings};
use crate::models::{PackageVersion, Target, WheelIdentity};
use crate::planner::{
    ArtifactRecord, PlanOutcome, UpstreamArtifact, UpstreamSdist, UpstreamWheel, plan,
    validate_catalog_targets,
};
use crate::platforms::target_python_version;
use crate::publish::{collect_validated, real_directory, selected_targets, sha256_of, wheel_paths};
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const DEFAULT_BUILD_INDEX: &str = "https://pypi.org/simple";

pub fn runtime_description(settings: &ProjectSettings) -> Result<String> {
    let probed = std::process::Command::new(crate::platforms::python_binary())
        .args(["-c", "import sys,sysconfig; print(sys.implementation.name); print(f'{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}'); print(sys.implementation.cache_tag); print(sysconfig.get_platform())"])
        .output()
        .map_err(|e| {
            Error::configuration(format!(
                "mypypi requires CPython {}; could not probe a Python interpreter: {e}",
                settings.python_version
            ))
        })?;
    if !probed.status.success() {
        return Err(Error::configuration(format!(
            "mypypi requires CPython {}; running unknown interpreter",
            settings.python_version
        )));
    }
    let probed_text = String::from_utf8_lossy(&probed.stdout).into_owned();
    let lines: Vec<&str> = probed_text.lines().map(|l| l.trim()).collect::<Vec<_>>();
    let (impl_name, actual, abi, platform_tag) = match lines.as_slice() {
        [a, b, c, d] => (*a, *b, *c, *d),
        _ => {
            return Err(Error::configuration(format!(
                "mypypi requires CPython {}; running unknown interpreter",
                settings.python_version
            )));
        }
    };
    let actual_version: pep440_rs::Version = actual.parse().map_err(|_| {
        Error::configuration(format!(
            "mypypi requires CPython {}; running {impl_name} {actual}",
            settings.python_version
        ))
    })?;
    if impl_name != "cpython" || !settings.contains_python(&actual_version) {
        return Err(Error::configuration(format!(
            "mypypi requires CPython {}; running {impl_name} {actual}",
            settings.python_version
        )));
    }
    let exe = std::process::Command::new(crate::platforms::python_binary())
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| crate::platforms::python_binary());
    Ok(format!("using CPython {actual} ABI {abi} on {platform_tag}; executable {exe}"))
}

fn load_catalog(path: &Path) -> Result<Catalog> {
    let text = std::fs::read_to_string(path)
        .map_err(|_| Error::io(format!("could not read catalog at {}", path.display()), Stage::Configuration))?;
    parse_catalog(&text)
}

type ArtifactKey = (PackageVersion, Target);

fn published_inventory(output: &Path, settings: &ProjectSettings) -> Result<FxHashMap<ArtifactKey, ArtifactRecord>> {
    let md = match std::fs::symlink_metadata(output) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FxHashMap::default()),
        Err(_) => {
            return Err(Error::io(
                format!("could not inspect published index at {}", output.display()),
                Stage::Planning,
            ));
        }
        Ok(m) => m,
    };
    if md.file_type().is_symlink() || !md.file_type().is_dir() {
        return Err(Error::invalid_input(
            format!("published index root must be a real directory: {}", output.display()),
            Stage::Planning,
        ));
    }
    let artifacts = crate::store::published_artifacts(output).map_err(|e| {
        Error::new(Kind::Integrity, e.detail.clone(), Stage::Planning)
    })?;
    let configured: Vec<Target> = settings.targets();
    let known_map: FxHashMap<&str, &Target> = configured.iter().map(|t| (t.platform.as_str(), t)).collect();
    let mut inventory: FxHashMap<ArtifactKey, ArtifactRecord> = FxHashMap::default();
    let mut ranks: FxHashMap<ArtifactKey, (usize, String)> = FxHashMap::default();
    for artifact in &artifacts {
        for target in artifact.targets.clone() {
            if target.python_version != settings.python_version {
                return Err(Error::integrity(
                    "published index manifest targets do not match project Python",
                    Stage::Planning,
                ));
            }
            let Some(expected) = known_map.get(target.platform.as_str()) else {
                return Err(Error::integrity(
                    format!("manifest target is not configured: {}", target.platform),
                    Stage::Planning,
                ));
            };
            let preference = crate::platforms::tag_preference(expected, &settings.linux_repair_policy)
                .map_err(|e| Error::integrity(e.detail, Stage::Planning))?;
            let rank = crate::platforms::wheel_rank(&artifact.wheel.filename, &preference).ok_or_else(|| {
                Error::integrity(
                    format!("published wheel tags do not cover recorded target {}", target.platform),
                    Stage::Planning,
                )
            })?;
            let key = (artifact.wheel.package.clone(), (*expected).clone());
            let record = ArtifactRecord { wheel: artifact.wheel.clone(), sha256: artifact.sha256.clone() };
            match ranks.get(&key) {
                None => {
                    ranks.insert(key.clone(), (rank, artifact.wheel.filename.clone()));
                    inventory.insert(key, record);
                }
                Some((er, ef)) if (rank, artifact.wheel.filename.clone()) < (*er, ef.clone()) => {
                    ranks.insert(key.clone(), (rank, artifact.wheel.filename.clone()));
                    inventory.insert(key, record);
                }
                _ => {}
            }
        }
    }
    Ok(inventory)
}

fn pending_inventory(
    paths: &[PathBuf],
    catalog: &Catalog,
    targets: &[Target],
    settings: &ProjectSettings,
    published: &FxHashMap<ArtifactKey, ArtifactRecord>,
) -> Result<(FxHashMap<ArtifactKey, ArtifactRecord>, FxHashMap<String, PathBuf>)> {
    let entries = catalog.by_package();
    let mut candidates: FxHashMap<ArtifactKey, Vec<(usize, ArtifactRecord, PathBuf)>> = FxHashMap::default();
    let mut local_filenames: FxHashMap<String, ArtifactRecord> = FxHashMap::default();
    let mut local_paths: FxHashMap<String, PathBuf> = FxHashMap::default();
    for path in paths {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let identity = WheelIdentity::parse(name).map_err(|e| {
            Error::integrity(format!("invalid wheel filename {name}: {}", e.detail), Stage::Planning)
        })?;
        if !entries.contains_key(&identity.package) {
            return Err(Error::invalid_input(
                format!("wheel is not listed in the catalog: {}", identity.package),
                Stage::Planning,
            )
            .with_package(&identity.package));
        }
        let record = ArtifactRecord { wheel: identity.clone(), sha256: sha256_of(path)? };
        if let Some(previous) = local_filenames.get(&identity.filename)
            && previous.sha256 != record.sha256 {
                return Err(Error::conflict(
                    format!("wheel filename has conflicting bytes: {}", identity.filename),
                    Stage::Planning,
                )
                .with_package(&identity.package));
            }
        local_filenames.insert(identity.filename.clone(), record.clone());
        local_paths.insert(identity.filename.clone(), path.clone());
        for target in targets {
            let preference = crate::platforms::tag_preference(target, &settings.linux_repair_policy)
                .map_err(|e| Error::integrity(e.detail, Stage::Planning))?;
            let Some(rank) = crate::platforms::wheel_rank(&identity.filename, &preference) else {
                continue;
            };
            let key = (identity.package.clone(), target.clone());
            if let Some(pub_record) = published.get(&key)
                && pub_record.wheel.filename == identity.filename && pub_record.sha256 != record.sha256 {
                    return Err(Error::conflict(
                        format!("wheel filename differs from published bytes: {}", identity.filename),
                        Stage::Planning,
                    )
                    .with_package(&identity.package)
                    .with_target(&target.python_version.to_string(), &target.platform));
                }
            candidates.entry(key).or_default().push((rank, record.clone(), path.clone()));
        }
    }
    let mut pending = FxHashMap::default();
    for (key, mut options) in candidates {
        options.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.wheel.filename.cmp(&b.1.wheel.filename)));
        let (_, record, _) = options.into_iter().next().unwrap();
        pending.insert(key, record);
    }
    Ok((pending, local_paths))
}

pub struct PlanData {
    pub outcomes: Vec<PlanOutcome>,
    pub releases: FxHashMap<PackageVersion, Release>,
    pub local_paths: FxHashMap<String, PathBuf>,
}

pub fn create_plan(
    catalog: &Catalog,
    settings: &ProjectSettings,
    targets: &[Target],
    output: &Path,
    state_dir: &Path,
) -> Result<PlanData> {
    let configured = settings.targets();
    validate_catalog_targets(catalog, &configured)?;
    let published = published_inventory(output, settings)?;
    let (pending, local_paths) = pending_inventory(&wheel_paths(state_dir)?, catalog, targets, settings, &published)?;
    let mut upstream: FxHashMap<ArtifactKey, Vec<UpstreamArtifact>> = FxHashMap::default();
    let mut releases: FxHashMap<PackageVersion, Release> = FxHashMap::default();
    for entry in &catalog.entries {
        for target in targets {
            if let Some(platforms) = entry.platforms.as_ref()
                && !platforms.contains(&target.platform)
            {
                continue;
            }
            let key = (entry.package.clone(), target.clone());
            if entry.excluded_platforms.iter().any(|(p, _)| p == &target.platform) {
                continue;
            }
            if published.contains_key(&key) || pending.contains_key(&key) {
                continue;
            }
            let release = if let Some(r) = releases.get(&entry.package) {
                r.clone()
            } else {
                let r = fetch_release(&entry.package, entry.yanked_reason.as_deref()).map_err(|e| {
                    let kind = match &e.kind {
                    Kind::InvalidInput => Kind::InvalidInput,
                    Kind::Configuration => Kind::Configuration,
                    Kind::MissingArtifact => Kind::MissingArtifact,
                    Kind::Integrity => Kind::Integrity,
                    Kind::Incompatible => Kind::Incompatible,
                    Kind::Dependency => Kind::Dependency,
                    Kind::Build => Kind::Build,
                    Kind::Validation => Kind::Validation,
                    Kind::Conflict => Kind::Conflict,
                    Kind::Limit => Kind::Limit,
                    Kind::Network => Kind::Network,
                    Kind::Io => Kind::Io,
                };
                let mut err = Error::new(kind, e.detail.clone(), Stage::Planning);
                    err.package = e.package.clone();
                    err.target = e.target.clone();
                    err
                })?;
                releases.insert(entry.package.clone(), r.clone());
                r
            };
            let execution = target_python_version(target);
            let spec_ok = release.requires_python.trim().is_empty()
                || release.requires_python.parse::<pep440_rs::VersionSpecifiers>().is_ok_and(|s| s.contains(&execution));
            if !spec_ok {
                return Err(Error::incompatible(
                    format!("Requires-Python {} excludes Python {execution}", release.requires_python),
                    Stage::Planning,
                )
                .with_package(&entry.package)
                .with_target(&target.python_version.to_string(), &target.platform));
            }
            let wheels = crate::acquire::compatible_wheels(&release, target, &settings.linux_repair_policy)
                .map_err(|e| Error::incompatible(e.detail, Stage::Planning))?;
            let mut candidates: Vec<UpstreamArtifact> = wheels
                .into_iter()
                .map(|w| {
                    let identity = WheelIdentity::parse(&w.filename).expect("compatible wheel parses");
                    UpstreamArtifact::Wheel(UpstreamWheel { wheel: identity, url: w.url.clone(), sha256: w.sha256.clone() })
                })
                .collect();
            for sdist in &release.sdists {
                let ok = match &sdist.requires_python {
                    None => true,
                    Some(rp) => rp.trim().is_empty() || rp.parse::<pep440_rs::VersionSpecifiers>().is_ok_and(|s| s.contains(&execution)),
                };
                if ok {
                    candidates.push(UpstreamArtifact::Sdist(UpstreamSdist { url: sdist.url.clone(), sha256: sdist.sha256.clone() }));
                }
            }
            upstream.insert(key, candidates);
        }
    }
    let outcomes = plan(catalog, targets, &published, &upstream, &pending, Some(&configured))?;
    Ok(PlanData { outcomes, releases, local_paths })
}

pub fn outcome_text(outcome: &PlanOutcome) -> String {
    match outcome {
        PlanOutcome::AlreadyPublished { package, target, .. } => {
            format!("published {package} for {}", target.platform)
        }
        PlanOutcome::DownloadWheel { wheel, target, .. } => {
            format!("download {} for {}", wheel.filename, target.platform)
        }
        PlanOutcome::BuildSdist { package, target, .. } => {
            format!("build {package} for {}", target.platform)
        }
        PlanOutcome::ExcludedTarget { package, target, reason } => {
            format!("skip {package} for {}: {reason}", target.platform)
        }
        PlanOutcome::PendingValidation { wheel, target, .. } => {
            format!("validate {} for {}", wheel.filename, target.platform)
        }
        PlanOutcome::ArtifactConflict { filename, target, .. } => {
            format!("conflict {filename} for {}", target.platform)
        }
    }
}

pub fn format_error(error: &Error, settings: Option<&ProjectSettings>) -> String {
    let mut details = Vec::new();
    if let Some(pkg) = &error.package {
        details.push(pkg.clone());
    }
    if let Some(target) = &error.target {
        details.push(target.clone());
    } else if let Some(s) = settings {
        details.push(format!("Python {}", s.python_version));
    }
    details.push(format!("stage {}", error.stage.as_str()));
    format!("mypypi: {} ({})", error.detail, details.join(", "))
}

#[derive(Debug)]
pub struct ResolvedPaths {
    pub catalog: PathBuf,
    pub state_dir: PathBuf,
    pub config: PathBuf,
    pub output: PathBuf,
}

pub fn resolve_paths(parsed: &Parsed) -> Result<ResolvedPaths> {
    if parsed.command == Command::Publish && parsed.output.is_none() {
        return Err(Error::usage("the following arguments are required: --output"));
    }
    if parsed.command == Command::Add {
        if !parsed.platforms.is_empty() {
            return Err(Error::usage("unrecognized arguments: --platform"));
        }
        if parsed.output.is_some() {
            return Err(Error::usage("unrecognized arguments: --output"));
        }
    }
    if parsed.command != Command::Add && (!parsed.uv_locks.is_empty() || !parsed.requirements_files.is_empty()) {
        return Err(Error::usage("--uv-lock and --requirements apply only to add"));
    }
    if parsed.command != Command::Build
        && (parsed.build_index.is_some()
            || parsed.strict_build_deps
            || parsed.build_constraints.is_some()
            || parsed.require_build_hashes)
    {
        return Err(Error::usage("unrecognized build dependency arguments for this command"));
    }
    Ok(ResolvedPaths {
        catalog: parsed.catalog.as_deref().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("packages.toml")),
        state_dir: parsed.state_dir.as_deref().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".mypypi")),
        config: parsed.config.as_deref().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("pyproject.toml")),
        output: parsed.output.as_deref().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("dist")),
    })
}

fn command_republish(parsed: &Parsed) -> Result<i32> {
    let output = parsed
        .output
        .as_deref()
        .ok_or_else(|| Error::usage("the following arguments are required: --output"))?;
    let public_url = parsed
        .public_url
        .as_deref()
        .ok_or_else(|| Error::usage("the following arguments are required: --public-url"))?;
    let summary = crate::publish::republish(&crate::publish::RepublishOptions {
        output: Path::new(output),
        public_url,
    })?;
    println!("republished {}", summary.generation.display());
    Ok(0)
}

pub fn run(parsed: &Parsed) -> Result<i32> {
    if parsed.command == Command::Republish {
        return command_republish(parsed);
    }
    if matches!(parsed.command, Command::Add | Command::Build | Command::Validate) {
        crate::build::require_uv_version()?;
    }
    let paths = resolve_paths(parsed)?;
    let settings = load_project_settings(&paths.config)?;
    // runtime gate runs for every command (mirrors cli.main calling it first)
    let description = runtime_description(&settings)?;
    println!("{description}");
    let catalog = load_catalog(&paths.catalog)?;
    match parsed.command {
        Command::Add => command_add(parsed, &settings, &paths),
        Command::Plan => command_plan(parsed, &settings, &paths, &catalog),
        Command::Build => command_build(parsed, &settings, &paths, &catalog),
        Command::Validate => command_validate(&settings, &paths, &catalog, &parsed.platforms),
        Command::Publish => command_publish(&settings, &paths, &catalog, &parsed.platforms),
        Command::Republish => command_republish(parsed),
    }
}

fn read_input(path: &str) -> Result<String> {
    std::fs::read_to_string(path)
        .map_err(|e| Error::io(format!("could not read {path}: {e}"), Stage::Command))
}

fn command_add(parsed: &Parsed, settings: &ProjectSettings, paths: &ResolvedPaths) -> Result<i32> {
    let mut requirements = parsed.positionals.clone();
    for path in &parsed.uv_locks {
        requirements.extend(crate::add::pins_from_uv_lock(&read_input(path)?)?);
    }
    for path in &parsed.requirements_files {
        requirements.extend(crate::add::pins_from_requirements(&read_input(path)?));
    }
    let resolved = crate::add::add_packages(
        &paths.catalog,
        &requirements,
        &settings.targets(),
        &crate::platforms::python_binary(),
    )?;
    println!("catalog contains {} resolved package pins", resolved.len());
    Ok(0)
}

fn command_plan(parsed: &Parsed, settings: &ProjectSettings, paths: &ResolvedPaths, catalog: &Catalog) -> Result<i32> {
    let targets = selected_targets(settings, &parsed.platforms)?;
    let data = create_plan(catalog, settings, &targets, &paths.output, &paths.state_dir)?;
    if data.outcomes.is_empty() {
        println!("no package targets are configured");
    } else {
        for outcome in &data.outcomes {
            println!("{}", outcome_text(outcome));
        }
    }
    Ok(0)
}

fn command_build(
    parsed: &Parsed,
    settings: &ProjectSettings,
    paths: &ResolvedPaths,
    catalog: &Catalog,
) -> Result<i32> {
    let targets = selected_targets(settings, &parsed.platforms)?;
    let build_index = validate_build_index(
        parsed.build_index.as_deref().unwrap_or(DEFAULT_BUILD_INDEX),
        parsed.strict_build_deps,
    )?;
    let build_constraints: Option<PathBuf> = match &parsed.build_constraints {
        None => None,
        Some(raw) => {
            let resolved = PathBuf::from(raw).canonicalize().map_err(|_| {
                Error::configuration(format!("could not read build constraints at {raw}"))
            })?;
            let md = std::fs::symlink_metadata(&resolved).map_err(|_| {
                Error::configuration(format!("could not read build constraints at {raw}"))
            })?;
            if md.file_type().is_symlink() || !md.file_type().is_file() {
                return Err(Error::configuration(format!("build constraints must be a regular file: {raw}")));
            }
            Some(resolved)
        }
    };
    let prebuild_cwd = std::env::current_dir()
        .map_err(|e| Error::io(format!("could not determine working directory: {e}"), Stage::Command))?;
    real_directory(&paths.state_dir, true)?;
    real_directory(&paths.state_dir.join("wheels"), true)?;
    let data = create_plan(catalog, settings, &targets, &paths.output, &paths.state_dir)?;
    let mut conflicts = Vec::new();
    for outcome in &data.outcomes {
        if matches!(outcome, PlanOutcome::ArtifactConflict { .. }) {
            conflicts.push(outcome);
        }
    }
    if let Some(first) = conflicts.first() {
        let (package, target) = match first {
            PlanOutcome::ArtifactConflict { package, target, .. } => (package, target),
            _ => unreachable!(),
        };
        return Err(Error::conflict(outcome_text(first), Stage::Planning)
            .with_package(package)
            .with_target(&target.python_version.to_string(), &target.platform));
    }
    for outcome in &data.outcomes {
        if let PlanOutcome::BuildSdist { target, package, .. } = outcome {
            require_native_target(target).map_err(|e| {
                let mut err = e;
                if err.package.is_none() {
                    err = err.with_package(package);
                }
                err
            })?;
        }
    }
    let mut completed: HashMap<PackageVersion, Vec<PathBuf>> = HashMap::new();
    let mut prepared: FxHashMap<String, (PathBuf, &'static str)> = FxHashMap::default();
    let entries = catalog.by_package();
    for outcome in &data.outcomes {
        let (package, target) = match outcome {
            PlanOutcome::DownloadWheel { package, target, .. } => (package, target),
            PlanOutcome::BuildSdist { package, target, .. } => (package, target),
            _ => continue,
        };
        let entry = entries.get(package).expect("catalog entry exists");
        let release = data.releases.get(package).expect("release fetched");
        let ignore: Vec<String> = entry.build.likely_binary_ignore.iter().map(|(p, _)| p.clone()).collect();
        let prebuild: Vec<(String, String)> = entry.build.custom_prebuild.clone();
        let opts = BuildOptions {
            index_url: &build_index,
            likely_binary_ignore: &ignore,
            custom_prebuild: &prebuild,
            linux_repair_policy: &settings.linux_repair_policy,
            build_constraints: build_constraints.as_deref(),
            require_build_hashes: parsed.require_build_hashes,
            env: None,
            prebuild_cwd: Some(&prebuild_cwd),
        };
        let mut path: Option<PathBuf> = None;
        if let Some(existing) = completed.get(package) {
            for candidate in existing {
                if validate_wheel_metadata(candidate, package, target, &settings.linux_repair_policy, true).is_err() {
                    continue;
                }
                if validate_wheel_architectures(candidate, target).is_err() {
                    continue;
                }
                path = Some(candidate.clone());
                break;
            }
        }
        let path = match path {
            Some(p) => p,
            None => {
                let built = acquire_or_build(release, target, &paths.state_dir.join("wheels"), &opts, entry.yanked_reason.as_deref())?;
                completed.entry(package.clone()).or_default().push(built.clone());
                built
            }
        };
        if !prepared.contains_key(path.file_name().and_then(|s| s.to_str()).unwrap_or("")) {
            let origin = if release.wheels.iter().any(|w| {
                path.file_name().and_then(|s| s.to_str()) == Some(w.filename.as_str())
            }) {
                "upstream wheel"
            } else {
                "source build"
            };
            prepared.insert(
                path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
                (path, origin),
            );
        }
    }
    let pending_count = data.outcomes.iter().filter(|o| matches!(o, PlanOutcome::PendingValidation { .. })).count();
    let mut prepared_names: Vec<&String> = prepared.keys().collect();
    prepared_names.sort_unstable();
    for name in prepared_names {
        let (_, origin) = &prepared[name];
        println!("prepared {name} from {origin}");
    }
    println!("prepared {} wheel(s); {pending_count} pending validation", prepared.len());
    Ok(0)
}

fn command_validate(
    settings: &ProjectSettings,
    paths: &ResolvedPaths,
    catalog: &Catalog,
    platforms: &[String],
) -> Result<i32> {
    let set = collect_validated(settings, &paths.output, &paths.state_dir, platforms, catalog)?;
    println!(
        "validated {} new wheel(s) from the local candidate snapshot; {} wheel-target pair(s) had no importable modules",
        set.artifacts.len(),
        set.no_imports
    );
    Ok(0)
}

fn command_publish(
    settings: &ProjectSettings,
    paths: &ResolvedPaths,
    catalog: &Catalog,
    platforms: &[String],
) -> Result<i32> {
    let summary = crate::publish::publish(&crate::publish::PublishOptions {
        settings,
        output: &paths.output,
        state_dir: &paths.state_dir,
        catalog,
        platforms,
    })?;
    println!(
        "published {} new wheel(s) at {}; {} wheel-target pair(s) had no importable modules",
        summary.new_wheels,
        summary.generation.display(),
        summary.no_imports
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_text_covers_all_outcomes() {
        let pkg = PackageVersion::parse("foo", "1.0").unwrap();
        let target = Target::new("3.15", "macos-arm64").unwrap();
        let wheel = WheelIdentity::parse("foo-1.0-py3-none-any.whl").unwrap();
        assert!(outcome_text(&PlanOutcome::AlreadyPublished { package: pkg.clone(), target: target.clone(), wheel: wheel.clone() }).starts_with("published "));
        assert!(outcome_text(&PlanOutcome::DownloadWheel { package: pkg.clone(), target: target.clone(), wheel: wheel.clone(), url: "https://example.test/x".to_string(), sha256: "0".repeat(64) }).starts_with("download "));
        assert!(outcome_text(&PlanOutcome::BuildSdist { package: pkg.clone(), target: target.clone(), url: "https://example.test/x".to_string(), sha256: "0".repeat(64) }).starts_with("build "));
        assert!(outcome_text(&PlanOutcome::ExcludedTarget { package: pkg.clone(), target: target.clone(), reason: "why".to_string() }).starts_with("skip "));
        assert!(outcome_text(&PlanOutcome::PendingValidation { package: pkg.clone(), target: target.clone(), wheel: wheel.clone() }).starts_with("validate "));
        assert!(outcome_text(&PlanOutcome::ArtifactConflict { package: pkg, target, filename: "f".to_string(), existing_sha256: "a".to_string(), candidate_sha256: "b".to_string() }).starts_with("conflict "));
    }
}
