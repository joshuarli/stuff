#![forbid(unsafe_code)]

use std::fmt::Write as _;

use crate::error::{Error, Result};
use crate::publish::{
    CargoPublish, MirrorPublish, NpmPublish, OciImage, OciSource, Options, PublishRequest,
    PypiPublish, PypiSpec, Registry, Request,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryFilter {
    Cargo,
    Npm,
    Pypi,
    Oci,
    Mirror,
}

impl RegistryFilter {
    pub fn as_str(&self) -> &'static str {
        match self {
            RegistryFilter::Cargo => "cargo",
            RegistryFilter::Npm => "npm",
            RegistryFilter::Pypi => "pypi",
            RegistryFilter::Oci => "oci",
            RegistryFilter::Mirror => "mirror",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cargo" => Some(RegistryFilter::Cargo),
            "npm" => Some(RegistryFilter::Npm),
            "pypi" => Some(RegistryFilter::Pypi),
            "oci" => Some(RegistryFilter::Oci),
            "mirror" => Some(RegistryFilter::Mirror),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Ls,
    Search,
    Diff,
    Doctor,
    Render,
    Publish,
    Republish,
    Rollback,
    Credentials,
    LockPath,
}

impl Command {
    fn name(self) -> &'static str {
        match self {
            Command::Ls => "ls",
            Command::Search => "search",
            Command::Diff => "diff",
            Command::Doctor => "doctor",
            Command::Render => "render",
            Command::Publish => "publish",
            Command::Republish => "republish",
            Command::Rollback => "rollback",
            Command::Credentials => "credentials",
            Command::LockPath => "lock-path",
        }
    }

    fn allowed_flags(self) -> AllowedFlags {
        match self {
            Command::Ls => AllowedFlags {
                registry: true,
                versions: true,
                json: true,
                base_urls: true,
                local_dirs: true,
                ..AllowedFlags::default()
            },
            Command::Search => AllowedFlags {
                registry: true,
                json: true,
                base_urls: true,
                local_dirs: true,
                ..AllowedFlags::default()
            },
            Command::Diff => AllowedFlags { json: true, ..AllowedFlags::default() },
            Command::Doctor => AllowedFlags {
                registry: true,
                sample_artifacts: true,
                base_urls: true,
                local_dirs: true,
                expected_generation: true,
                ..AllowedFlags::default()
            },
            Command::Render => AllowedFlags {
                registry: true,
                base_urls: true,
                local_dirs: true,
                output: true,
                ..AllowedFlags::default()
            },
            Command::Publish
            | Command::Republish
            | Command::Rollback
            | Command::Credentials
            | Command::LockPath => {
                AllowedFlags::default()
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct AllowedFlags {
    registry: bool,
    versions: bool,
    json: bool,
    sample_artifacts: bool,
    base_urls: bool,
    local_dirs: bool,
    output: bool,
    expected_generation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub registry: Option<RegistryFilter>,
    pub versions: bool,
    pub json: bool,
    pub sample_artifacts: bool,
    pub cargo_base: Option<String>,
    pub npm_base: Option<String>,
    pub pypi_base: Option<String>,
    pub oci_base: Option<String>,
    pub mirror_base: Option<String>,
    pub cargo_dir: Option<String>,
    pub npm_dir: Option<String>,
    pub pypi_dir: Option<String>,
    pub oci_dir: Option<String>,
    pub mirror_dir: Option<String>,
    pub output: Option<String>,
    pub expected_generation: Option<String>,
    pub positionals: Vec<String>,
    pub request: Option<Request>,
}

impl Parsed {
    pub fn registry_filter(&self) -> Option<&'static str> {
        self.registry.map(|r| r.as_str())
    }
}

fn usage_error(detail: impl Into<String>) -> Error {
    Error::usage(detail)
}

fn ensure_allowed(command: Command, flag: &str, present: bool, allowed: bool) -> Result<()> {
    if present && !allowed {
        return Err(usage_error(format!("{} does not take {flag}", command.name())));
    }
    Ok(())
}

pub fn parse_args(argv: &[String]) -> Result<Parsed> {
    match argv.first().map(String::as_str) {
        Some("publish") => parse_publish(&argv[1..]),
        Some("republish") => parse_republish(&argv[1..]),
        Some("rollback") => parse_rollback(&argv[1..]),
        Some("credentials") => parse_bucket(Command::Credentials, &argv[1..]),
        Some("lock-path") => parse_lock_path(&argv[1..]),
        _ => parse_read_only(argv),
    }
}

fn parse_read_only(argv: &[String]) -> Result<Parsed> {
    let mut args = argv.iter().peekable();
    let command = match args.next().map(|s| s.as_str()) {
        Some("ls") => Command::Ls,
        Some("search") => Command::Search,
        Some("diff") => Command::Diff,
        Some("doctor") => Command::Doctor,
        Some("render") => Command::Render,
        Some(other) => return Err(usage_error(format!("unknown command '{other}'"))),
        None => return Err(usage_error("a command is required")),
    };
    let mut parsed = Parsed {
        command,
        registry: None,
        versions: false,
        json: false,
        sample_artifacts: false,
        cargo_base: None,
        npm_base: None,
        pypi_base: None,
        oci_base: None,
        mirror_base: None,
        cargo_dir: None,
        npm_dir: None,
        pypi_dir: None,
        oci_dir: None,
        mirror_dir: None,
        output: None,
        expected_generation: None,
        positionals: Vec::new(),
        request: None,
    };
    let mut seen_registry = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--versions" => parsed.versions = true,
            "--json" => parsed.json = true,
            "--sample-artifacts" => parsed.sample_artifacts = true,
            "--registry" => {
                let value = args.next().ok_or_else(|| usage_error("--registry requires a value"))?;
                if seen_registry {
                    return Err(usage_error("--registry may only be specified once"));
                }
                seen_registry = true;
                parsed.registry = Some(
                    RegistryFilter::parse(value).ok_or_else(|| usage_error(format!("unsupported registry '{value}'")))?,
                );
            }
            "--cargo-base" | "--npm-base" | "--pypi-base" | "--oci-base" | "--mirror-base" | "--cargo-dir" | "--npm-dir"
            | "--pypi-dir" | "--oci-dir" | "--mirror-dir" | "--output" | "--expected-generation" => {
                let value = args.next().ok_or_else(|| usage_error(format!("{arg} requires a value")))?;
                let slot = match arg.as_str() {
                    "--cargo-base" => &mut parsed.cargo_base,
                    "--npm-base" => &mut parsed.npm_base,
                    "--pypi-base" => &mut parsed.pypi_base,
                    "--oci-base" => &mut parsed.oci_base,
                    "--mirror-base" => &mut parsed.mirror_base,
                    "--cargo-dir" => &mut parsed.cargo_dir,
                    "--npm-dir" => &mut parsed.npm_dir,
                    "--pypi-dir" => &mut parsed.pypi_dir,
                    "--oci-dir" => &mut parsed.oci_dir,
                    "--mirror-dir" => &mut parsed.mirror_dir,
                    "--output" => &mut parsed.output,
                    _ => &mut parsed.expected_generation,
                };
                if slot.is_some() {
                    return Err(usage_error(format!("{arg} may only be specified once")));
                }
                *slot = Some(value.clone());
            }
            other if other.starts_with('-') => return Err(usage_error(format!("unknown option: {other}"))),
            other => parsed.positionals.push(other.to_string()),
        }
    }
    let allowed = parsed.command.allowed_flags();
    for (flag, present, allowed) in [
        ("--registry", parsed.registry.is_some(), allowed.registry),
        ("--versions", parsed.versions, allowed.versions),
        ("--json", parsed.json, allowed.json),
        ("--sample-artifacts", parsed.sample_artifacts, allowed.sample_artifacts),
        ("--cargo-base", parsed.cargo_base.is_some(), allowed.base_urls),
        ("--npm-base", parsed.npm_base.is_some(), allowed.base_urls),
        ("--pypi-base", parsed.pypi_base.is_some(), allowed.base_urls),
        ("--oci-base", parsed.oci_base.is_some(), allowed.base_urls),
        ("--mirror-base", parsed.mirror_base.is_some(), allowed.base_urls),
        ("--cargo-dir", parsed.cargo_dir.is_some(), allowed.local_dirs),
        ("--npm-dir", parsed.npm_dir.is_some(), allowed.local_dirs),
        ("--pypi-dir", parsed.pypi_dir.is_some(), allowed.local_dirs),
        ("--oci-dir", parsed.oci_dir.is_some(), allowed.local_dirs),
        ("--mirror-dir", parsed.mirror_dir.is_some(), allowed.local_dirs),
        ("--output", parsed.output.is_some(), allowed.output),
        (
            "--expected-generation",
            parsed.expected_generation.is_some(),
            allowed.expected_generation,
        ),
    ] {
        ensure_allowed(parsed.command, flag, present, allowed)?;
    }
    match parsed.command {
        Command::Ls | Command::Doctor | Command::Render => {
            if !parsed.positionals.is_empty() {
                return Err(usage_error(format!(
                    "{} does not take positional arguments",
                    parsed.command.name()
                )));
            }
        }
        Command::Search => {
            if parsed.positionals.len() != 1 {
                return Err(usage_error("search requires exactly one query"));
            }
        }
        Command::Diff => {
            if parsed.positionals.len() != 2 {
                return Err(usage_error("diff requires two catalog paths"));
            }
        }
        Command::Publish
            | Command::Republish
            | Command::Rollback
            | Command::Credentials
            | Command::LockPath => {}
    }
    Ok(parsed)
}

fn parse_bucket(command: Command, args: &[String]) -> Result<Parsed> {
    match args {
        [bucket] if !bucket.is_empty() && !bucket.starts_with('-') => Ok(Parsed {
            positionals: vec![bucket.clone()],
            ..request_parsed(command, None)
        }),
        _ => Err(usage_error(format!("{} requires exactly one bucket", command.name()))),
    }
}

fn parse_lock_path(args: &[String]) -> Result<Parsed> {
    match args {
        [flag, name] if flag == "--registry" => {
            let registry = RegistryFilter::parse(name)
                .ok_or_else(|| usage_error(format!("unknown registry: {name}")))?;
            Ok(Parsed {
                registry: Some(registry),
                ..request_parsed(Command::LockPath, None)
            })
        }
        _ => parse_bucket(Command::LockPath, args),
    }
}

fn request_parsed(command: Command, request: impl Into<Option<Request>>) -> Parsed {
    Parsed {
        command,
        registry: None,
        versions: false,
        json: false,
        sample_artifacts: false,
        cargo_base: None,
        npm_base: None,
        pypi_base: None,
        oci_base: None,
        mirror_base: None,
        cargo_dir: None,
        npm_dir: None,
        pypi_dir: None,
        oci_dir: None,
        mirror_dir: None,
        output: None,
        expected_generation: None,
        positionals: Vec::new(),
        request: request.into(),
    }
}

fn publish_registry(args: &[String], command: &str) -> Result<(Registry, usize)> {
    match args.first().map(String::as_str) {
        Some(name) => {
            let registry = Registry::parse(name)
                .ok_or_else(|| usage_error(format!("unsupported {command} registry '{name}'")))?;
            Ok((registry, 1))
        }
        None => Err(usage_error(format!(
            "{command} requires a registry: cargo, npm, pypi, oci, or mirror"
        ))),
    }
}

fn next_value<'a>(args: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| usage_error(format!("{flag} requires a value")))
}

fn assign_once(slot: &mut Option<String>, flag: &str, value: &str) -> Result<()> {
    if slot.is_some() {
        return Err(usage_error(format!("{flag} may only be specified once")));
    }
    *slot = Some(value.to_string());
    Ok(())
}

fn take_common_option(arg: &str, options: &mut Options) -> bool {
    match arg {
        "--dry-run" => options.dry_run = true,
        "--json" => options.json = true,
        "--verbose" => options.verbose = true,
        _ => return false,
    }
    true
}

fn parse_publish(args: &[String]) -> Result<Parsed> {
    let (registry, mut index) = publish_registry(args, "publish")?;
    let (request, options) = match registry {
        Registry::Cargo => parse_cargo_publish(args, &mut index)?,
        Registry::Npm => {
            parse_npm_publish(args, &mut index)?
        }
        Registry::Pypi => parse_pypi_publish(args, &mut index)?,
        Registry::Oci => {
            let (oci, options) = parse_oci_publish(args, &mut index)?;
            (PublishRequest::Oci(oci), options)
        }
        Registry::Mirror => {
            let (mirror, options) = parse_mirror_publish(args, &mut index)?;
            (PublishRequest::Mirror(mirror), options)
        }
    };
    Ok(request_parsed(Command::Publish, Request::Publish { request, options }))
}

fn parse_cargo_publish(args: &[String], index: &mut usize) -> Result<(PublishRequest, Options)> {
    let mut cargo = CargoPublish { packages: Vec::new(), manifest_path: None, registry: None };
    let mut lockfiles = Vec::new();
    let mut options = Options::default();
    let mut seen_common: Vec<&str> = Vec::new();
    while *index < args.len() {
        let arg = args[*index].as_str();
        if matches!(arg, "--dry-run" | "--json" | "--verbose") {
            if seen_common.contains(&arg) {
                return Err(usage_error(format!("{arg} may only be specified once")));
            }
            seen_common.push(arg);
            take_common_option(arg, &mut options);
            *index += 1;
            continue;
        }
        match arg {
            "-p" => {
                let value = next_value(args, index, "-p")?;
                if value.is_empty() {
                    return Err(usage_error("-p requires a package name"));
                }
                cargo.packages.push(value.to_string());
            }
            "--manifest-path" => {
                let value = next_value(args, index, "--manifest-path")?;
                assign_once(&mut cargo.manifest_path, "--manifest-path", value)?;
            }
            "--registry" => {
                let value = next_value(args, index, "--registry")?;
                assign_once(&mut cargo.registry, "--registry", value)?;
            }
            "--lockfile" => {
                let value = next_value(args, index, "--lockfile")?;
                if value.is_empty() {
                    return Err(usage_error("--lockfile requires a path"));
                }
                lockfiles.push(value.to_string());
            }
            other if other.starts_with('-') => {
                return Err(usage_error(format!("unknown option: {other}")));
            }
            other => {
                return Err(usage_error(format!(
                    "publish cargo does not take positional argument '{other}'"
                )));
            }
        }
        *index += 1;
    }
    if !lockfiles.is_empty() {
        if !cargo.packages.is_empty() || cargo.manifest_path.is_some() || cargo.registry.is_some() {
            return Err(usage_error(
                "publish cargo takes either --lockfile PATH or -p NAME [--manifest-path PATH] [--registry NAME], not both",
            ));
        }
        return Ok((PublishRequest::CargoLockfiles(lockfiles), options));
    }
    if cargo.packages.is_empty() {
        return Err(usage_error("publish cargo requires at least one -p NAME or --lockfile PATH"));
    }
    Ok((PublishRequest::Cargo(cargo), options))
}

fn parse_npm_publish(args: &[String], index: &mut usize) -> Result<(PublishRequest, Options)> {
    let mut lockfile = None;
    let mut tarballs = Vec::new();
    let mut project = None;
    let mut options = Options::default();
    while *index < args.len() {
        let arg = args[*index].as_str();
        if take_common_option(arg, &mut options) {
            *index += 1;
            continue;
        }
        match arg {
            "--lockfile" => {
                let value = next_value(args, index, "--lockfile")?;
                assign_once(&mut lockfile, "--lockfile", value)?;
            }
            "--project" => {
                let value = next_value(args, index, "--project")?;
                assign_once(&mut project, "--project", value)?;
            }
            "--tarball" => {
                let value = next_value(args, index, "--tarball")?;
                tarballs.push(value.to_string());
            }
            other if other.starts_with('-') => {
                return Err(usage_error(format!("unknown option: {other}")));
            }
            other => {
                return Err(usage_error(format!(
                    "publish npm does not take positional argument '{other}'"
                )));
            }
        }
        *index += 1;
    }
    if !tarballs.is_empty() {
        if lockfile.is_some() || project.is_some() {
            return Err(usage_error(
                "publish npm takes either --tarball PATH or --lockfile PATH --project ID, not both",
            ));
        }
        return Ok((PublishRequest::NpmTarballs(tarballs), options));
    }
    let lockfile = lockfile
        .ok_or_else(|| usage_error("publish npm requires --lockfile PATH or --tarball PATH"))?;
    let project = project.ok_or_else(|| usage_error("publish npm requires --project ID"))?;
    Ok((PublishRequest::Npm(NpmPublish { lockfile, project }), options))
}

fn parse_pypi_publish(args: &[String], index: &mut usize) -> Result<(PublishRequest, Options)> {
    let mut specs = Vec::new();
    let mut wheels = Vec::new();
    let mut allow_sdist = false;
    let mut options = Options::default();
    while *index < args.len() {
        let arg = args[*index].as_str();
        if take_common_option(arg, &mut options) {
            *index += 1;
            continue;
        }
        if arg == "--allow-sdist" {
            allow_sdist = true;
            *index += 1;
            continue;
        }
        if arg == "--wheel" {
            let value = next_value(args, index, "--wheel")?;
            if value.is_empty() {
                return Err(usage_error("--wheel requires a path"));
            }
            wheels.push(value.to_string());
            *index += 1;
            continue;
        }
        if arg.starts_with('-') {
            return Err(usage_error(format!("unknown option: {arg}")));
        }
        let spec = PypiSpec::parse(arg)
            .ok_or_else(|| usage_error(format!("invalid SPEC '{arg}': expected name==version")))?;
        specs.push(spec);
        *index += 1;
    }
    if !wheels.is_empty() {
        if !specs.is_empty() || allow_sdist {
            return Err(usage_error(
                "publish pypi takes either --wheel PATH or name==version [--allow-sdist], not both",
            ));
        }
        return Ok((PublishRequest::PypiWheels(wheels), options));
    }
    if specs.is_empty() {
        return Err(usage_error("publish pypi requires at least one name==version or --wheel PATH"));
    }
    Ok((PublishRequest::Pypi(PypiPublish { specs, allow_sdist }), options))
}

fn parse_oci_publish(args: &[String], index: &mut usize) -> Result<(OciSource, Options)> {
    let mut images = Vec::new();
    let mut layout = None;
    let mut repository = None;
    let mut tag = None;
    let mut inventory = None;
    let mut mirror_lock = None;
    let mut options = Options::default();
    while *index < args.len() {
        let arg = args[*index].as_str();
        if take_common_option(arg, &mut options) {
            *index += 1;
            continue;
        }
        match arg {
            "--image" => {
                let value = next_value(args, index, "--image")?;
                let image = OciImage::parse(value).ok_or_else(|| {
                    usage_error(format!("invalid image '{value}': expected REPO=REF"))
                })?;
                images.push(image);
            }
            "--layout" => {
                let value = next_value(args, index, "--layout")?;
                assign_once(&mut layout, "--layout", value)?;
            }
            "--repository" => {
                let value = next_value(args, index, "--repository")?;
                assign_once(&mut repository, "--repository", value)?;
            }
            "--tag" => {
                let value = next_value(args, index, "--tag")?;
                assign_once(&mut tag, "--tag", value)?;
            }
            "--inventory" => {
                let value = next_value(args, index, "--inventory")?;
                assign_once(&mut inventory, "--inventory", value)?;
            }
            "--lock" => {
                let value = next_value(args, index, "--lock")?;
                assign_once(&mut mirror_lock, "--lock", value)?;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(format!("unknown option: {other}")));
            }
            other => {
                return Err(usage_error(format!(
                    "publish oci does not take positional argument '{other}'"
                )));
            }
        }
        *index += 1;
    }
    if !images.is_empty() {
        if layout.is_some()
            || repository.is_some()
            || tag.is_some()
            || inventory.is_some()
            || mirror_lock.is_some()
        {
            return Err(usage_error(
                "publish oci accepts either --image, --layout, or --inventory/--lock, not a mix",
            ));
        }
        return Ok((OciSource::Images(images), options));
    }
    if let Some(inventory) = inventory {
        let lock = mirror_lock.ok_or_else(|| {
            usage_error("publish oci --inventory requires --lock FILE")
        })?;
        if layout.is_some() || repository.is_some() || tag.is_some() {
            return Err(usage_error(
                "publish oci accepts either --inventory/--lock or --layout, not both",
            ));
        }
        return Ok((OciSource::Mirror { inventory, lock }, options));
    }
    if mirror_lock.is_some() {
        return Err(usage_error("publish oci --lock requires --inventory FILE"));
    }
    let dir = layout.ok_or_else(|| {
        usage_error("publish oci requires --image REPO=REF, --layout DIR, or --inventory FILE --lock FILE")
    })?;
    let repository = repository
        .ok_or_else(|| usage_error("publish oci --layout requires --repository NAME"))?;
    Ok((OciSource::Layout { dir, repository, tag }, options))
}

fn parse_mirror_publish(args: &[String], index: &mut usize) -> Result<(MirrorPublish, Options)> {
    let mut manifest = None;
    let mut allow_drops = false;
    let mut options = Options::default();
    while *index < args.len() {
        let arg = args[*index].as_str();
        if take_common_option(arg, &mut options) {
            *index += 1;
            continue;
        }
        match arg {
            "--manifest" => {
                let value = next_value(args, index, "--manifest")?;
                if value.is_empty() {
                    return Err(usage_error("--manifest requires a path"));
                }
                assign_once(&mut manifest, "--manifest", value)?;
            }
            "--allow-drops" => allow_drops = true,
            other if other.starts_with('-') => {
                return Err(usage_error(format!("unknown option: {other}")));
            }
            other => {
                return Err(usage_error(format!(
                    "publish mirror does not take positional argument '{other}'"
                )));
            }
        }
        *index += 1;
    }
    Ok((MirrorPublish { manifest, allow_drops }, options))
}

fn parse_republish(args: &[String]) -> Result<Parsed> {
    let (registry, mut index) = publish_registry(args, "republish")?;
    let mut allow_drops = false;
    let mut manifest = None;
    let mut options = Options::default();
    while index < args.len() {
        let arg = args[index].as_str();
        if take_common_option(arg, &mut options) {
            index += 1;
            continue;
        }
        match arg {
            "--allow-drops" => allow_drops = true,
            "--manifest" => {
                if registry != Registry::Mirror {
                    return Err(usage_error("--manifest only applies to the mirror registry"));
                }
                let value = next_value(args, &mut index, "--manifest")?;
                if value.is_empty() {
                    return Err(usage_error("--manifest requires a path"));
                }
                assign_once(&mut manifest, "--manifest", value)?;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(format!("unknown option: {other}")));
            }
            other => {
                return Err(usage_error(format!(
                    "republish does not take positional argument '{other}'"
                )));
            }
        }
        index += 1;
    }
    Ok(request_parsed(
        Command::Republish,
        Request::Republish { registry, allow_drops, options, manifest },
    ))
}

fn parse_rollback(args: &[String]) -> Result<Parsed> {
    let (registry, mut index) = publish_registry(args, "rollback")?;
    let mut generation = None;
    let mut options = Options::default();
    while index < args.len() {
        let arg = args[index].as_str();
        if take_common_option(arg, &mut options) {
            index += 1;
            continue;
        }
        match arg {
            other if other.starts_with('-') => {
                return Err(usage_error(format!("unknown option: {other}")));
            }
            other => {
                if generation.is_some() {
                    return Err(usage_error("rollback takes exactly one generation"));
                }
                generation = Some(other.to_string());
            }
        }
        index += 1;
    }
    let generation =
        generation.ok_or_else(|| usage_error("rollback requires a generation"))?;
    Ok(request_parsed(
        Command::Rollback,
        Request::Rollback { registry, generation, options },
    ))
}

pub fn help_text() -> String {
    "\
stuff

Usage: stuff <COMMAND> [OPTIONS]

Commands:
  ls [--registry cargo|npm|pypi|oci|mirror] [--versions] [--json]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL] [--mirror-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR] [--mirror-dir DIR]
  search <query> [--registry cargo|npm|pypi|oci|mirror] [--json]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL] [--mirror-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR] [--mirror-dir DIR]
  diff <before.json> <after.json> [--json]
  doctor [--registry cargo|npm|pypi|oci|mirror] [--expected-generation ID] [--sample-artifacts]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL] [--mirror-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR] [--mirror-dir DIR]
  render --output FILE [--registry cargo|npm|pypi|oci|mirror]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL] [--mirror-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR] [--mirror-dir DIR]
  credentials <bucket>
     print shell exports of short-lived R2 S3 credentials for one bucket,
     minted through the configured cf_profile; use with eval
  lock-path <bucket> | lock-path --registry cargo|npm|pypi|oci|mirror
     print the publish lock file of a registry, or of the registry that owns
     a configured bucket; maintenance helpers hold it while they write

Sources default to the live registries (no credentials needed).
Pass --cargo-dir/--npm-dir/--pypi-dir/--oci-dir/--mirror-dir (site, generation, or
catalog.json) to inspect local published output offline. Registries without
a local dir are skipped by doctor, which then refuses --expected-generation;
naming one with --registry needs its --*-dir or --*-base, never the live default.
--output FILE ending in .txt or .md writes the aggregate llms.txt; any
other name writes HTML.
"
    .to_string()
}

pub fn print_help() {
    print!("{}", help_text());
}

pub fn render_table(catalogs: &[stuff_catalog::Catalog], show_versions: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{:<8} {:<24} {:<9} {:<8} LATEST", "REGISTRY", "PACKAGE", "SOURCE", "VERSIONS");
    let mut total_packages = 0usize;
    let mut total_versions = 0usize;
    for catalog in catalogs {
        for package in &catalog.packages {
            total_packages += 1;
            total_versions += package.versions.len();
            let _ = writeln!(
                out,
                "{:<8} {:<24} {:<9} {:<8} {}",
                catalog.registry.as_str(),
                package.name,
                package.source.as_deref().unwrap_or("-"),
                package.versions.len(),
                package.latest
            );
            if show_versions {
                for v in &package.versions {
                    let mark = if v.version == package.latest {
                        " (latest)"
                    } else if v.yanked {
                        " (yanked)"
                    } else {
                        ""
                    };
                    let _ = writeln!(out, "{:<8} {:<24} {:<9} {:<8} {}{}", "", "", "", "", v.version, mark);
                }
            }
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{total_packages} {} · {total_versions} {}",
        if total_packages == 1 { "package" } else { "packages" },
        if total_versions == 1 { "version" } else { "versions" },
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[(&str, &[&str])] = &[
        (
            "ls",
            &[
                "--registry",
                "--versions",
                "--json",
                "--cargo-base",
                "--npm-base",
                "--pypi-base",
                "--oci-base",
                "--mirror-base",
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
                "--mirror-dir",
            ],
        ),
        (
            "search",
            &[
                "--registry",
                "--json",
                "--cargo-base",
                "--npm-base",
                "--pypi-base",
                "--oci-base",
                "--mirror-base",
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
                "--mirror-dir",
            ],
        ),
        ("diff", &["--json"]),
        (
            "doctor",
            &[
                "--registry",
                "--expected-generation",
                "--sample-artifacts",
                "--cargo-base",
                "--npm-base",
                "--pypi-base",
                "--oci-base",
                "--mirror-base",
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
                "--mirror-dir",
            ],
        ),
        (
            "render",
            &[
                "--output",
                "--registry",
                "--cargo-base",
                "--npm-base",
                "--pypi-base",
                "--oci-base",
                "--mirror-base",
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
                "--mirror-dir",
            ],
        ),
    ];

    const ALL_FLAGS: &[&str] = &[
        "--registry",
        "--versions",
        "--json",
        "--sample-artifacts",
        "--cargo-base",
        "--npm-base",
        "--pypi-base",
        "--oci-base",
        "--mirror-base",
        "--cargo-dir",
        "--npm-dir",
        "--pypi-dir",
        "--oci-dir",
        "--mirror-dir",
        "--output",
        "--expected-generation",
        "--remote",
    ];

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn base_args(command: &str) -> Vec<&'static str> {
        match command {
            "ls" => vec!["ls"],
            "search" => vec!["search", "query"],
            "diff" => vec!["diff", "before.json", "after.json"],
            "doctor" => vec!["doctor"],
            "render" => vec!["render"],
            _ => unreachable!(),
        }
    }

    fn flag_value(flag: &str) -> Option<&'static str> {
        match flag {
            "--registry" => Some("npm"),
            "--cargo-base" | "--npm-base" | "--pypi-base" | "--oci-base" | "--mirror-base" => Some("https://example.com"),
            "--cargo-dir" | "--npm-dir" | "--pypi-dir" | "--oci-dir" | "--mirror-dir" => Some("."),
            "--output" => Some("out.html"),
            "--expected-generation" => Some("gen1"),
            _ => None,
        }
    }

    fn supports(command: &str, flag: &str) -> bool {
        COMMANDS
            .iter()
            .find(|(name, _)| *name == command)
            .is_some_and(|(_, flags)| flags.contains(&flag))
    }

    #[test]
    fn parses_ls_variants() {
        let p = parse_args(&args(&["ls"])).unwrap();
        assert_eq!(p.command, Command::Ls);
        let p = parse_args(&args(&["ls", "--registry", "npm", "--versions", "--json"])).unwrap();
        assert_eq!(p.registry, Some(RegistryFilter::Npm));
        assert!(p.versions && p.json);
        assert_eq!(
            parse_args(&args(&["ls", "--registry", "oci"])).unwrap().registry,
            Some(RegistryFilter::Oci)
        );
        assert!(parse_args(&args(&["ls", "--registry", "bogus"])).is_err());
        assert!(parse_args(&args(&["ls", "extra"])).is_err());
    }

    #[test]
    fn every_command_rejects_each_unsupported_flag() {
        for (command, _) in COMMANDS {
            for flag in ALL_FLAGS.iter().copied() {
                let mut argv = base_args(command);
                argv.push(flag);
                if let Some(value) = flag_value(flag) {
                    argv.push(value);
                }
                let result = parse_args(&args(&argv));
                if supports(command, flag) {
                    assert!(result.is_ok(), "{command} rejected supported {flag}: {:?}", result.err());
                } else {
                    let err = result.expect_err(&format!("{command} accepted unsupported {flag}"));
                    assert!(matches!(err, Error::Usage(_)), "{command} {flag}: {err}");
                    assert_eq!(err.exit_code(), 2);
                }
            }
        }
    }

    #[test]
    fn help_advertises_only_accepted_flags_and_positionals() {
        let help = help_text();
        assert!(!help.contains("[sources...]"), "{help}");
        assert!(!help.contains("--remote"), "{help}");
        let mut command: Option<&str> = None;
        let mut advertised: Vec<String> = Vec::new();
        let mut checked = 0;
        for line in help.lines() {
            let trimmed = line.trim();
            let first = trimmed.split_whitespace().next().unwrap_or("");
            if let Some((name, _)) = COMMANDS.iter().find(|(name, _)| *name == first) {
                if let Some(previous) = command {
                    check_advertised(previous, &advertised);
                    checked += 1;
                }
                command = Some(name);
                advertised.clear();
            }
            if command.is_some() && trimmed.is_empty() {
                break;
            }
            if command.is_some() {
                for token in trimmed.split_whitespace() {
                    let token = token.trim_matches(|c| c == '[' || c == ']');
                    if let Some(flag) = token.strip_prefix("--") {
                        advertised.push(format!("--{flag}"));
                    }
                }
            }
        }
        if let Some(previous) = command {
            check_advertised(previous, &advertised);
            checked += 1;
        }
        assert_eq!(checked, COMMANDS.len());
    }

    fn publish_request(argv: &[&str]) -> (PublishRequest, Options) {
        match parse_args(&args(argv)).unwrap().request.unwrap() {
            Request::Publish { request, options } => (request, options),
            other => panic!("{other:?}"),
        }
    }

    fn usage_rejected(argv: &[&str]) {
        let error = parse_args(&args(argv)).unwrap_err();
        assert!(matches!(error, Error::Usage(_)), "{argv:?}: {error}");
        assert_eq!(error.exit_code(), 2, "{argv:?}");
    }

    #[test]
    fn parses_publish_cargo_grammar() {
        let (request, options) = publish_request(&[
            "publish",
            "cargo",
            "-p",
            "alpha",
            "-p",
            "beta",
            "--manifest-path",
            "Cargo.toml",
            "--registry",
            "local",
            "--dry-run",
            "--json",
            "--verbose",
        ]);
        let PublishRequest::Cargo(cargo) = request else { panic!("{request:?}") };
        assert_eq!(cargo.packages, vec!["alpha", "beta"]);
        assert_eq!(cargo.manifest_path.as_deref(), Some("Cargo.toml"));
        assert_eq!(cargo.registry.as_deref(), Some("local"));
        assert!(options.dry_run && options.json && options.verbose);
    }

    #[test]
    fn parses_publish_npm_grammar() {
        let (request, options) =
            publish_request(&["publish", "npm", "--lockfile", "pnpm-lock.yaml", "--project", "site", "--json"]);
        let PublishRequest::Npm(npm) = request else { panic!("{request:?}") };
        assert_eq!(npm.lockfile, "pnpm-lock.yaml");
        assert_eq!(npm.project, "site");
        assert!(options.json && !options.dry_run);
    }

    #[test]
    fn parses_publish_npm_tarballs() {
        let (request, options) =
            publish_request(&["publish", "npm", "--tarball", "a.tgz", "--tarball", "b.tgz", "--dry-run"]);
        let PublishRequest::NpmTarballs(tarballs) = request else { panic!("{request:?}") };
        assert_eq!(tarballs, vec!["a.tgz", "b.tgz"]);
        assert!(options.dry_run);
    }

    #[test]
    fn parses_publish_pypi_grammar() {
        let (request, options) =
            publish_request(&["publish", "pypi", "alpha==1.0.0", "beta==2.0.0", "--allow-sdist", "--dry-run"]);
        let PublishRequest::Pypi(pypi) = request else { panic!("{request:?}") };
        assert_eq!(pypi.specs.len(), 2);
        assert_eq!(pypi.specs[0].name, "alpha");
        assert_eq!(pypi.specs[0].version, "1.0.0");
        assert_eq!(pypi.specs[1].name, "beta");
        assert!(pypi.allow_sdist);
        assert!(options.dry_run);
    }

    #[test]
    fn parses_publish_pypi_wheels() {
        let (request, options) =
            publish_request(&["publish", "pypi", "--wheel", "a.whl", "--wheel", "b.whl", "--json"]);
        let PublishRequest::PypiWheels(wheels) = request else { panic!("{request:?}") };
        assert_eq!(wheels, vec!["a.whl", "b.whl"]);
        assert!(options.json);
    }

    #[test]
    fn parses_publish_mirror_grammar() {
        let (request, options) = publish_request(&["publish", "mirror"]);
        let PublishRequest::Mirror(mirror) = request else { panic!("{request:?}") };
        assert_eq!(mirror.manifest, None);
        assert!(!mirror.allow_drops);
        assert!(!options.dry_run);

        let (request, options) = publish_request(&[
            "publish",
            "mirror",
            "--manifest",
            "custom.toml",
            "--allow-drops",
            "--dry-run",
            "--json",
            "--verbose",
        ]);
        let PublishRequest::Mirror(mirror) = request else { panic!("{request:?}") };
        assert_eq!(mirror.manifest.as_deref(), Some("custom.toml"));
        assert!(mirror.allow_drops);
        assert!(options.dry_run && options.json && options.verbose);
    }

    #[test]
    fn republish_accepts_manifest_only_for_the_mirror() {
        match parse_args(&args(&["republish", "mirror", "--manifest", "custom.toml", "--allow-drops"]))
            .unwrap()
            .request
            .unwrap()
        {
            Request::Republish { registry, allow_drops, manifest, .. } => {
                assert_eq!(registry, Registry::Mirror);
                assert_eq!(manifest.as_deref(), Some("custom.toml"));
                assert!(allow_drops);
            }
            other => panic!("{other:?}"),
        }
        for argv in [
            vec!["republish", "cargo", "--manifest", "custom.toml"],
            vec!["republish", "npm", "--manifest", "custom.toml"],
            vec!["republish", "mirror", "--manifest"],
        ] {
            usage_rejected(&argv);
        }
    }

    #[test]
    fn parses_both_publish_oci_forms() {
        let (request, _) =
            publish_request(&["publish", "oci", "--image", "repo/a=1.0", "--image", "repo/b=2.0"]);
        let PublishRequest::Oci(OciSource::Images(images)) = request else { panic!("{request:?}") };
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].repository, "repo/a");
        assert_eq!(images[0].reference, "1.0");

        let (request, options) = publish_request(&[
            "publish",
            "oci",
            "--layout",
            "dir",
            "--repository",
            "repo/name",
            "--tag",
            "latest",
            "--dry-run",
        ]);
        let PublishRequest::Oci(OciSource::Layout { dir, repository, tag }) = request else {
            panic!("{request:?}")
        };
        assert_eq!(dir, "dir");
        assert_eq!(repository, "repo/name");
        assert_eq!(tag.as_deref(), Some("latest"));
        assert!(options.dry_run);

        let (request, options) = publish_request(&[
            "publish",
            "oci",
            "--inventory",
            "mirror.toml",
            "--lock",
            "mirror.lock",
            "--json",
        ]);
        let PublishRequest::Oci(OciSource::Mirror { inventory, lock }) = request else {
            panic!("{request:?}")
        };
        assert_eq!(inventory, "mirror.toml");
        assert_eq!(lock, "mirror.lock");
        assert!(options.json);
    }

    #[test]
    fn publish_rejects_bad_grammar_with_exit_two() {
        for argv in [
            vec!["publish"],
            vec!["publish", "bogus"],
            vec!["publish", "cargo"],
            vec!["publish", "cargo", "-p"],
            vec!["publish", "cargo", "-p", "x", "--allow-drops"],
            vec!["publish", "cargo", "-p", "x", "extra"],
            vec!["publish", "npm"],
            vec!["publish", "npm", "--lockfile", "l"],
            vec!["publish", "npm", "--project", "p"],
            vec!["publish", "npm", "--lockfile", "l", "--project", "p", "--tag", "t"],
            vec!["publish", "npm", "--tarball"],
            vec!["publish", "npm", "--tarball", "a.tgz", "--lockfile", "l"],
            vec!["publish", "npm", "--tarball", "a.tgz", "--project", "p"],
            vec!["publish", "pypi"],
            vec!["publish", "pypi", "not-a-spec"],
            vec!["publish", "pypi", "name=="],
            vec!["publish", "pypi", "==1.0"],
            vec!["publish", "pypi", "--wheel"],
            vec!["publish", "pypi", "--wheel", "a.whl", "alpha==1.0"],
            vec!["publish", "pypi", "--wheel", "a.whl", "--allow-sdist"],
            vec!["publish", "oci"],
            vec!["publish", "oci", "--layout", "dir"],
            vec!["publish", "oci", "--repository", "repo"],
            vec!["publish", "oci", "--tag", "latest"],
            vec!["publish", "oci", "--image", "bad"],
            vec!["publish", "oci", "--image", "repo="],
            vec!["publish", "oci", "--image", "=ref"],
            vec!["publish", "oci", "--image", "repo=ref", "--layout", "dir"],
            vec!["publish", "oci", "--image", "repo=ref", "--inventory", "mirror.toml"],
            vec!["publish", "oci", "--image", "repo=ref", "--repository", "repo"],
            vec!["publish", "oci", "--image", "repo=ref", "--tag", "latest"],
            vec!["publish", "oci", "--layout", "dir", "--repository", "repo", "extra"],
            vec!["publish", "oci", "--inventory", "mirror.toml"],
            vec!["publish", "oci", "--lock", "mirror.lock"],
            vec!["publish", "oci", "--inventory", "mirror.toml", "--layout", "dir"],
            vec!["publish", "oci", "--inventory", "mirror.toml", "--lock", "mirror.lock", "--repository", "repo"],
            vec!["publish", "oci", "--inventory", "mirror.toml", "--lock", "mirror.lock", "--tag", "t"],
            vec!["publish", "oci", "--inventory", "mirror.toml", "--lock", "mirror.lock", "--image", "repo=ref"],
            vec!["publish", "mirror", "extra"],
            vec!["publish", "mirror", "--bogus"],
            vec!["publish", "mirror", "--manifest"],
            vec!["publish", "mirror", "--registry", "cargo"],
        ] {
            usage_rejected(&argv);
        }
    }

    #[test]
    fn parses_republish_and_rollback_grammar() {
        match parse_args(&args(&["republish", "cargo", "--allow-drops", "--dry-run", "--json"]))
            .unwrap()
            .request
            .unwrap()
        {
            Request::Republish { registry, allow_drops, options, manifest } => {
                assert_eq!(registry, Registry::Cargo);
                assert_eq!(manifest, None);
                assert!(allow_drops && options.dry_run && options.json && !options.verbose);
            }
            other => panic!("{other:?}"),
        }
        match parse_args(&args(&["rollback", "oci", "gen-42", "--json", "--verbose"]))
            .unwrap()
            .request
            .unwrap()
        {
            Request::Rollback { registry, generation, options } => {
                assert_eq!(registry, Registry::Oci);
                assert_eq!(generation, "gen-42");
                assert!(options.json && options.verbose && !options.dry_run);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn republish_and_rollback_reject_bad_grammar_with_exit_two() {
        for argv in [
            vec!["republish"],
            vec!["republish", "bogus"],
            vec!["republish", "cargo", "extra"],
            vec!["republish", "cargo", "--image", "repo=ref"],
            vec!["republish", "mirror", "--image", "repo=ref"],
            vec!["rollback"],
            vec!["rollback", "bogus", "gen"],
            vec!["rollback", "cargo"],
            vec!["rollback", "cargo", "gen-1", "gen-2"],
            vec!["rollback", "cargo", "gen", "--allow-drops"],
        ] {
            usage_rejected(&argv);
        }
    }

    fn check_advertised(command: &str, advertised: &[String]) {
        for flag in advertised {
            assert!(supports(command, flag), "{command} help advertises unsupported {flag}");
            let mut argv = base_args(command);
            argv.push(flag);
            if let Some(value) = flag_value(flag) {
                argv.push(value);
            }
            assert!(parse_args(&args(&argv)).is_ok(), "{command} help advertises rejected {flag}");
        }
        let (_, supported) = COMMANDS.iter().find(|(name, _)| *name == command).unwrap();
        for flag in supported.iter() {
            assert!(
                advertised.iter().any(|advertised| advertised == flag),
                "{command} help does not advertise {flag}"
            );
        }
    }

}
