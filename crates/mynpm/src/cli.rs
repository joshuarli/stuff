#![forbid(unsafe_code)]

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    ImportLock,
    Add,
    Plan,
    Fetch,
    Verify,
    Publish,
    Validate,
    Doctor,
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
    pub json: bool,
    pub smoke_executables: bool,
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
        "validate" => Some(Command::Validate),
        "doctor" => Some(Command::Doctor),
        _ => None,
    }
}

fn is_manager(s: &str) -> bool {
    s == "npm" || s == "pnpm"
}

pub fn parse_args(argv: &[String]) -> Result<Parsed> {
    let mut command: Option<Command> = None;
    let mut config = None;
    let mut state_dir = None;
    let mut site_dir = None;
    let mut project = None;
    let mut manager = None;
    let mut client = None;
    let mut json = false;
    let mut smoke = false;
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
        if ["--config", "--state-dir", "--site-dir", "--project", "--manager", "--client", "--target"]
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
    if site_dir.is_some() && command != Command::Publish {
        return Err(Error::Usage("--site-dir is only valid with publish".to_string()));
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
    }
    Ok(Parsed {
        command,
        config,
        state_dir,
        site_dir,
        project,
        manager,
        client,
        json,
        smoke_executables: smoke,
        targets,
        positionals,
    })
}

pub fn print_help() {
    println!("mynpm");
    println!();
    println!("Usage: mynpm <COMMAND> [OPTIONS]");
    println!();
    println!("Commands:");
    println!("  import-lock <lockfile> --project NAME [--target T]...");
    println!("  add <name@version>... --manager npm|pnpm [--target T]...");
    println!("  plan [--json]");
    println!("  fetch");
    println!("  verify");
    println!("  publish [--site-dir DIR]");
    println!("  validate --client npm|pnpm [--smoke-executables]");
    println!("  doctor <projectDir>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_plan_and_rejects_bad_flags() {
        let p = parse_args(&args(&["plan", "--json"])).unwrap();
        assert_eq!(p.command, Command::Plan);
        assert!(p.json);
        assert!(parse_args(&args(&["fetch", "--json"])).is_err());
        assert!(parse_args(&args(&["validate"])).is_err());
        assert!(parse_args(&args(&["validate", "--client", "npm"])).is_ok());
        assert!(parse_args(&args(&["add", "--manager", "npm", "a@1.0.0"])).is_ok());
        assert!(parse_args(&args(&["import-lock", "p.json"])).is_err());
    }
}
