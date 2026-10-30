#![forbid(unsafe_code)]

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryFilter {
    Cargo,
    Npm,
    Pypi,
}

impl RegistryFilter {
    pub fn as_str(&self) -> &'static str {
        match self {
            RegistryFilter::Cargo => "cargo",
            RegistryFilter::Npm => "npm",
            RegistryFilter::Pypi => "pypi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cargo" => Some(RegistryFilter::Cargo),
            "npm" => Some(RegistryFilter::Npm),
            "pypi" => Some(RegistryFilter::Pypi),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ls,
    Search,
    Diff,
    Doctor,
    Render,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub registry: Option<RegistryFilter>,
    pub versions: bool,
    pub json: bool,
    pub remote: bool,
    pub cargo_base: Option<String>,
    pub npm_base: Option<String>,
    pub pypi_base: Option<String>,
    pub cargo_dir: Option<String>,
    pub npm_dir: Option<String>,
    pub pypi_dir: Option<String>,
    pub output: Option<String>,
    pub expected_generation: Option<String>,
    pub positionals: Vec<String>,
}

impl Parsed {
    pub fn registry_filter(&self) -> Option<&'static str> {
        self.registry.map(|r| r.as_str())
    }
}

fn usage_error(detail: impl Into<String>) -> String {
    detail.into()
}

pub fn parse_args(argv: &[String]) -> Result<Parsed, String> {
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
        remote: false,
        cargo_base: None,
        npm_base: None,
        pypi_base: None,
        cargo_dir: None,
        npm_dir: None,
        pypi_dir: None,
        output: None,
        expected_generation: None,
        positionals: Vec::new(),
    };
    let mut seen_registry = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--versions" => parsed.versions = true,
            "--json" => parsed.json = true,
            "--remote" => parsed.remote = true,
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
            "--cargo-base" | "--npm-base" | "--pypi-base" | "--cargo-dir" | "--npm-dir" | "--pypi-dir" | "--output"
            | "--expected-generation" => {
                let value = args.next().ok_or_else(|| usage_error(format!("{arg} requires a value")))?;
                let slot = match arg.as_str() {
                    "--cargo-base" => &mut parsed.cargo_base,
                    "--npm-base" => &mut parsed.npm_base,
                    "--pypi-base" => &mut parsed.pypi_base,
                    "--cargo-dir" => &mut parsed.cargo_dir,
                    "--npm-dir" => &mut parsed.npm_dir,
                    "--pypi-dir" => &mut parsed.pypi_dir,
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
    match parsed.command {
        Command::Ls => {
            if !parsed.positionals.is_empty() {
                return Err(usage_error("ls does not take positional arguments"));
            }
            if parsed.output.is_some() {
                return Err(usage_error("--output is only valid with render"));
            }
            if parsed.expected_generation.is_some() {
                return Err(usage_error("--expected-generation is only valid with doctor"));
            }
        }
        Command::Search => {
            if parsed.positionals.len() != 1 {
                return Err(usage_error("search requires exactly one query"));
            }
            if parsed.versions {
                return Err(usage_error("--versions is only valid with ls"));
            }
            if parsed.output.is_some() {
                return Err(usage_error("--output is only valid with render"));
            }
            if parsed.expected_generation.is_some() {
                return Err(usage_error("--expected-generation is only valid with doctor"));
            }
        }
        Command::Diff => {
            if parsed.positionals.len() != 2 {
                return Err(usage_error("diff requires two catalog paths"));
            }
            if parsed.registry.is_some() || parsed.versions {
                return Err(usage_error("diff does not take --registry or --versions"));
            }
            if parsed.output.is_some() {
                return Err(usage_error("--output is only valid with render"));
            }
            if parsed.expected_generation.is_some() {
                return Err(usage_error("--expected-generation is only valid with doctor"));
            }
            if parsed.cargo_dir.is_some() || parsed.npm_dir.is_some() || parsed.pypi_dir.is_some() {
                return Err(usage_error("diff reads catalog files directly; --*-dir is not valid here"));
            }
        }
        Command::Doctor => {
            if !parsed.positionals.is_empty() {
                return Err(usage_error("doctor does not take positional arguments"));
            }
            if parsed.versions || parsed.json {
                return Err(usage_error("doctor does not take --versions or --json"));
            }
            if parsed.output.is_some() {
                return Err(usage_error("--output is only valid with render"));
            }
        }
        Command::Render => {
            if parsed.command == Command::Render && !parsed.positionals.is_empty() {
                return Err(usage_error("render does not take positional arguments"));
            }
            if parsed.versions {
                return Err(usage_error("--versions is only valid with ls"));
            }
            if parsed.expected_generation.is_some() {
                return Err(usage_error("--expected-generation is only valid with doctor"));
            }
        }
    }
    if parsed.remote
        && (parsed.cargo_dir.is_some() || parsed.npm_dir.is_some() || parsed.pypi_dir.is_some())
    {
        return Err(usage_error("--remote cannot be combined with --*-dir"));
    }
    Ok(parsed)
}

pub fn print_help() {
    println!("stuff");
    println!();
    println!("Usage: stuff <COMMAND> [OPTIONS]");
    println!();
    println!("Commands:");
    println!("  ls [--registry cargo|npm|pypi] [--versions] [--json]");
    println!("     [--remote [--cargo-base URL --npm-base URL --pypi-base URL]]");
    println!("     [--cargo-dir DIR --npm-dir DIR --pypi-dir DIR]");
    println!("  search <query> [--registry cargo|npm|pypi] [--json] [sources...]");
    println!("  diff <before.json> <after.json> [--json]");
    println!("  doctor [--remote|--cargo-dir DIR ...] [--expected-generation ID]");
    println!("  render --output index.html [sources...]");
    println!();
    println!("Sources default to the live registries (no credentials needed).");
    println!("Pass --cargo-dir/--npm-dir/--pypi-dir (site, generation, or catalog.json)");
    println!("to inspect local published output offline.");
}

pub fn render_table(catalogs: &[stuff_catalog::Catalog], show_versions: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{:<8} {:<24} {:<8} {}", "REGISTRY", "PACKAGE", "VERSIONS", "LATEST");
    let mut total_packages = 0usize;
    let mut total_versions = 0usize;
    for catalog in catalogs {
        for package in &catalog.packages {
            total_packages += 1;
            total_versions += package.versions.len();
            let _ = writeln!(
                out,
                "{:<8} {:<24} {:<8} {}",
                catalog.registry.as_str(),
                package.name,
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
                    let _ = writeln!(out, "{:<8} {:<24} {:<8} {}{}", "", "", "", v.version, mark);
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

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_ls_variants() {
        let p = parse_args(&args(&["ls"])).unwrap();
        assert_eq!(p.command, Command::Ls);
        let p = parse_args(&args(&["ls", "--registry", "npm", "--versions", "--json"])).unwrap();
        assert_eq!(p.registry, Some(RegistryFilter::Npm));
        assert!(p.versions && p.json);
        assert!(parse_args(&args(&["ls", "--registry", "bogus"])).is_err());
        assert!(parse_args(&args(&["ls", "extra"])).is_err());
    }

    #[test]
    fn parses_other_commands() {
        assert_eq!(parse_args(&args(&["search", "foo"])).unwrap().positionals, vec!["foo".to_string()]);
        assert!(parse_args(&args(&["search"])).is_err());
        assert!(parse_args(&args(&["diff", "a.json", "b.json"])).unwrap().positionals.len() == 2);
        assert!(parse_args(&args(&["diff", "a.json"])).is_err());
        assert!(parse_args(&args(&["doctor", "--remote"])).unwrap().remote);
        assert!(parse_args(&args(&["render", "--output", "index.html"])).unwrap().output.as_deref() == Some("index.html"));
        assert!(parse_args(&args(&["bogus"])).is_err());
    }

    #[test]
    fn rejects_mixed_sources() {
        assert!(parse_args(&args(&["ls", "--remote", "--cargo-dir", "x"])).is_err());
    }
}
