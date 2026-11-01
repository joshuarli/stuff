#![forbid(unsafe_code)]

use std::fmt::Write as _;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryFilter {
    Cargo,
    Npm,
    Pypi,
    Oci,
}

impl RegistryFilter {
    pub fn as_str(&self) -> &'static str {
        match self {
            RegistryFilter::Cargo => "cargo",
            RegistryFilter::Npm => "npm",
            RegistryFilter::Pypi => "pypi",
            RegistryFilter::Oci => "oci",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cargo" => Some(RegistryFilter::Cargo),
            "npm" => Some(RegistryFilter::Npm),
            "pypi" => Some(RegistryFilter::Pypi),
            "oci" => Some(RegistryFilter::Oci),
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
}

impl Command {
    fn name(self) -> &'static str {
        match self {
            Command::Ls => "ls",
            Command::Search => "search",
            Command::Diff => "diff",
            Command::Doctor => "doctor",
            Command::Render => "render",
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
    pub cargo_dir: Option<String>,
    pub npm_dir: Option<String>,
    pub pypi_dir: Option<String>,
    pub oci_dir: Option<String>,
    pub output: Option<String>,
    pub expected_generation: Option<String>,
    pub positionals: Vec<String>,
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
        cargo_dir: None,
        npm_dir: None,
        pypi_dir: None,
        oci_dir: None,
        output: None,
        expected_generation: None,
        positionals: Vec::new(),
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
            "--cargo-base" | "--npm-base" | "--pypi-base" | "--oci-base" | "--cargo-dir" | "--npm-dir"
            | "--pypi-dir" | "--oci-dir" | "--output" | "--expected-generation" => {
                let value = args.next().ok_or_else(|| usage_error(format!("{arg} requires a value")))?;
                let slot = match arg.as_str() {
                    "--cargo-base" => &mut parsed.cargo_base,
                    "--npm-base" => &mut parsed.npm_base,
                    "--pypi-base" => &mut parsed.pypi_base,
                    "--oci-base" => &mut parsed.oci_base,
                    "--cargo-dir" => &mut parsed.cargo_dir,
                    "--npm-dir" => &mut parsed.npm_dir,
                    "--pypi-dir" => &mut parsed.pypi_dir,
                    "--oci-dir" => &mut parsed.oci_dir,
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
        ("--cargo-dir", parsed.cargo_dir.is_some(), allowed.local_dirs),
        ("--npm-dir", parsed.npm_dir.is_some(), allowed.local_dirs),
        ("--pypi-dir", parsed.pypi_dir.is_some(), allowed.local_dirs),
        ("--oci-dir", parsed.oci_dir.is_some(), allowed.local_dirs),
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
    }
    Ok(parsed)
}

pub fn help_text() -> String {
    "\
stuff

Usage: stuff <COMMAND> [OPTIONS]

Commands:
  ls [--registry cargo|npm|pypi|oci] [--versions] [--json]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR]
  search <query> [--registry cargo|npm|pypi|oci] [--json]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR]
  diff <before.json> <after.json> [--json]
  doctor [--registry cargo|npm|pypi|oci] [--expected-generation ID] [--sample-artifacts]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR]
  render --output FILE [--registry cargo|npm|pypi|oci]
     [--cargo-base URL] [--npm-base URL] [--pypi-base URL] [--oci-base URL]
     [--cargo-dir DIR] [--npm-dir DIR] [--pypi-dir DIR] [--oci-dir DIR]

Sources default to the live registries (no credentials needed).
Pass --cargo-dir/--npm-dir/--pypi-dir/--oci-dir (site, generation, or
catalog.json) to inspect local published output offline. Registries without
a local dir are skipped by doctor, which then refuses --expected-generation.
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
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
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
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
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
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
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
                "--cargo-dir",
                "--npm-dir",
                "--pypi-dir",
                "--oci-dir",
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
        "--cargo-dir",
        "--npm-dir",
        "--pypi-dir",
        "--oci-dir",
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
            "--cargo-base" | "--npm-base" | "--pypi-base" | "--oci-base" => Some("https://example.com"),
            "--cargo-dir" | "--npm-dir" | "--pypi-dir" | "--oci-dir" => Some("."),
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
    fn parses_other_commands() {
        assert_eq!(parse_args(&args(&["search", "foo"])).unwrap().positionals, vec!["foo".to_string()]);
        assert!(parse_args(&args(&["search"])).is_err());
        assert!(parse_args(&args(&["diff", "a.json", "b.json"])).unwrap().positionals.len() == 2);
        assert!(parse_args(&args(&["diff", "a.json"])).is_err());
        assert!(parse_args(&args(&["doctor", "--sample-artifacts"])).unwrap().sample_artifacts);
        assert!(parse_args(&args(&["render", "--output", "index.html"])).unwrap().output.as_deref() == Some("index.html"));
        assert!(parse_args(&args(&["bogus"])).is_err());
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
    fn rejects_flags_no_command_reads() {
        assert!(parse_args(&args(&["doctor", "--remote"])).is_err());
        assert!(parse_args(&args(&["render", "--output", "x.html", "--json"])).is_err());
        assert!(parse_args(&args(&["diff", "--cargo-base", "https://example.com", "a", "b"])).is_err());
        assert!(parse_args(&args(&["ls", "--sample-artifacts"])).is_err());
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

    #[test]
    fn usage_errors_exit_two() {
        let err = parse_args(&args(&["bogus"])).unwrap_err();
        assert!(matches!(err, Error::Usage(_)));
        assert_eq!(err.exit_code(), 2);
    }
}
