#[path = "cli/init.rs"]
pub mod init;
#[path = "cli/plan.rs"]
pub mod plan;
#[path = "cli/import_lock.rs"]
pub mod import_lock;
#[path = "cli/add.rs"]
pub mod add;
#[path = "cli/fetch.rs"]
pub mod fetch;
#[path = "cli/publish.rs"]
pub mod publish;
#[path = "cli/serve_publish.rs"]
pub mod serve_publish;
#[path = "cli/verify.rs"]
pub mod verify;
#[path = "cli/doctor.rs"]
pub mod doctor;
#[path = "cli/validate.rs"]
pub mod validate;

use crate::error::{Error, Result};

#[derive(Debug)]
pub enum Command {
    Init,
    Plan,
    ImportLock,
    Add,
    Fetch,
    Publish,
    ServePublish,
    Verify,
    Doctor,
    Validate,
}

#[derive(Debug)]
pub struct Args {
    pub command: Command,
    pub args: Vec<String>,
}

fn parse_command(s: &str) -> Option<Command> {
    match s {
        "init" => Some(Command::Init),
        "plan" => Some(Command::Plan),
        "import-lock" => Some(Command::ImportLock),
        "add" => Some(Command::Add),
        "fetch" => Some(Command::Fetch),
        "publish" => Some(Command::Publish),
        "serve-publish" => Some(Command::ServePublish),
        "verify" => Some(Command::Verify),
        "doctor" => Some(Command::Doctor),
        "validate" => Some(Command::Validate),
        _ => None,
    }
}

pub fn parse_args() -> Result<Args> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(args: I) -> Result<Args>
where
    I: IntoIterator<Item = String>,
{
    let mut cmd: Option<Command> = None;
    let mut cmd_seen = false;
    let mut rest: Vec<String> = Vec::new();

    for arg in args {
        if !cmd_seen {
            if arg == "-h" || arg == "--help" {
                print_help();
                std::process::exit(0);
            }
            if arg.starts_with('-') {
                return Err(Error::Input(format!(
                    "unexpected global argument '{arg}'; expected a command"
                )));
            }
            match parse_command(&arg) {
                Some(c) => {
                    cmd = Some(c);
                    cmd_seen = true;
                }
                None => {
                    return Err(Error::Input(format!("unknown command '{arg}'")));
                }
            }
            continue;
        }
        if arg == "-h" || arg == "--help" {
            print_help();
            std::process::exit(0);
        }
        rest.push(arg);
    }

    let command = cmd.ok_or_else(|| Error::Input("missing command".to_string()))?;
    Ok(Args { command, args: rest })
}

pub(crate) fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    if *i + 1 >= args.len() {
        return Err(Error::Input(format!("{flag} requires a value")));
    }
    *i += 1;
    Ok(args[*i].clone())
}

pub(crate) fn reject_unknown(arg: &str, usage: &str) -> Result<()> {
    Err(Error::Input(format!("unexpected argument '{arg}'\nusage: {usage}")))
}

pub fn print_help() {
    println!("mycrates");
    println!();
    println!("Usage: mycrates <COMMAND> [OPTIONS]");
    println!();
    println!("Commands:");
    println!("  init [--state-dir DIR] [--site-dir DIR]");
    println!("  plan [--state-dir DIR]");
    println!("  import-lock <Cargo.lock> [--project NAME] [--state-dir DIR]");
    println!("  add <name>@<version> [--state-dir DIR]");
    println!("  fetch [--state-dir DIR] [--config PATH]");
    println!("  publish [--state-dir DIR] [--site-dir DIR] [--config PATH]");
    println!("  serve-publish [--bind ADDR] [--token-file PATH] [--state-dir DIR] [--site-dir DIR] [--config PATH]");
    println!("  verify [--state-dir DIR]");
    println!("  doctor [--state-dir DIR]");
    println!("  validate [--state-dir DIR] [--project NAME] [--project-dir DIR] [--manifest-path PATH] [--mirror-url URL] [--config PATH]");
    println!();
    println!("Options:");
    println!("  -h, --help  Print help");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn subcommand_options_reach_handlers() {
        let a = parse_args_from(args(&[
            "serve-publish",
            "--bind",
            "127.0.0.1:9999",
            "--token-file",
            "/etc/mycrates/token",
        ]))
        .unwrap();
        assert!(matches!(a.command, Command::ServePublish));
        assert_eq!(
            a.args,
            args(&["--bind", "127.0.0.1:9999", "--token-file", "/etc/mycrates/token"])
        );
    }

    #[test]
    fn unknown_command_rejected() {
        let err = parse_args_from(args(&["frobnicate"])).unwrap_err();
        assert!(format!("{err}").contains("unknown command"));
    }

    #[test]
    fn global_option_rejected() {
        let err = parse_args_from(args(&["--bind", "x"])).unwrap_err();
        assert!(format!("{err}").contains("expected a command"));
    }

    #[test]
    fn missing_command_rejected() {
        let err = parse_args_from(args(&[])).unwrap_err();
        assert!(format!("{err}").contains("missing command"));
    }
}
