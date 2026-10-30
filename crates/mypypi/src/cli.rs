#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Add,
    Plan,
    Build,
    Validate,
    Publish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub catalog: Option<String>,
    pub state_dir: Option<String>,
    pub config: Option<String>,
    pub output: Option<String>,
    pub platforms: Vec<String>,
    pub build_index: Option<String>,
    pub strict_build_deps: bool,
    pub build_constraints: Option<String>,
    pub require_build_hashes: bool,
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
    let mut platforms: Vec<String> = Vec::new();
    let mut build_index = None;
    let mut strict = false;
    let mut constraints = None;
    let mut require_hashes = false;
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
                _ => {}
            }
        }
        match arg {
            "--catalog" | "--state-dir" | "--config" | "--output" | "--platform"
            | "--build-index" | "--build-constraints" => {
                let value = argv.get(i + 1).ok_or_else(|| fail(format!("{arg} requires a value")))?;
                i += 2;
                match arg {
                    "--catalog" => catalog = Some(value.clone()),
                    "--state-dir" => state_dir = Some(value.clone()),
                    "--config" => config = Some(value.clone()),
                    "--output" => output = Some(value.clone()),
                    "--platform" => platforms.push(value.clone()),
                    "--build-index" => build_index = Some(value.clone()),
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
        Command::Add if positionals.is_empty() => {
            return Err(fail("add requires at least one requirement"));
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
        platforms,
        build_index,
        strict_build_deps: strict,
        build_constraints: constraints,
        require_build_hashes: require_hashes,
        positionals,
    })
}

pub fn print_help() {
    println!("mypypi");
    println!();
    println!("Usage: mypypi [OPTIONS] <COMMAND> [OPTIONS]");
    println!();
    println!("Commands:");
    println!("  add <requirement>...");
    println!("  plan [--output DIR] [--platform P]...");
    println!("  build [--output DIR] [--platform P]... [--build-index URL] [--strict-build-deps]");
    println!("  validate [--output DIR] [--platform P]...");
    println!("  publish [--output DIR] [--platform P]...");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_add_and_plan() {
        let p = parse_args(&args(&["add", "foo==1.0"])).unwrap();
        assert_eq!(p.command, Command::Add);
        let p = parse_args(&args(&["plan", "--platform", "linux-x86_64"])).unwrap();
        assert_eq!(p.platforms, vec!["linux-x86_64".to_string()]);
        assert!(parse_args(&args(&["plan", "extra"])).is_err());
    }
}
