#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::import::{import_from, import_layout};
use crate::inspect::inspect;
use crate::state::State;
use stuff_oci::{RepositoryName, Tag};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Init,
    Import,
    Inspect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub command: Command,
    pub state_dir: String,
    pub layout: Option<String>,
    pub from: Option<String>,
    pub repository: Option<String>,
    pub tag: Option<String>,
    pub json: bool,
}

pub fn parse_args(argv: &[String]) -> Result<Parsed> {
    let Some(command) = argv.first() else {
        return Err(Error::Usage("a command is required".to_string()));
    };
    let command = match command.as_str() {
        "init" => Command::Init,
        "import" => Command::Import,
        "inspect" => Command::Inspect,
        other => return Err(Error::Usage(format!("unknown command {other}"))),
    };
    let mut parsed = Parsed {
        command,
        state_dir: String::new(),
        layout: None,
        from: None,
        repository: None,
        tag: None,
        json: false,
    };
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--state-dir" => parsed.state_dir = take(argv, &mut i, "--state-dir")?,
            "--layout" => parsed.layout = Some(take(argv, &mut i, "--layout")?),
            "--from" => parsed.from = Some(take(argv, &mut i, "--from")?),
            "--repository" => parsed.repository = Some(take(argv, &mut i, "--repository")?),
            "--tag" => parsed.tag = Some(take(argv, &mut i, "--tag")?),
            "--json" => parsed.json = true,
            other => return Err(Error::Usage(format!("unknown option {other}"))),
        }
        i += 1;
    }
    if parsed.state_dir.is_empty() {
        return Err(Error::Usage("--state-dir is required".to_string()));
    }
    match command {
        Command::Init => {
            if parsed.layout.is_some() || parsed.from.is_some() || parsed.repository.is_some() || parsed.tag.is_some() || parsed.json {
                return Err(Error::Usage("init accepts only --state-dir".to_string()));
            }
        }
        Command::Import => {
            if parsed.repository.is_none() {
                return Err(Error::Usage("import requires --repository".to_string()));
            }
            if parsed.layout.is_some() == parsed.from.is_some() {
                return Err(Error::Usage(
                    "import requires exactly one of --layout or --from".to_string(),
                ));
            }
        }
        Command::Inspect => {
            if parsed.layout.is_some() || parsed.from.is_some() || parsed.repository.is_some() || parsed.tag.is_some() {
                return Err(Error::Usage("inspect accepts only --state-dir [--json]".to_string()));
            }
        }
    }
    Ok(parsed)
}

fn take(argv: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    argv.get(*i)
        .cloned()
        .ok_or_else(|| Error::Usage(format!("{flag} requires a value")))
}

pub fn run(argv: &[String]) -> Result<()> {
    let parsed = parse_args(argv)?;
    let state = State::new(PathBuf::from(&parsed.state_dir));
    match parsed.command {
        Command::Init => {
            state.init()?;
            println!("initialized {}", state.root().display());
        }
        Command::Import => {
            let repository = RepositoryName::parse(parsed.repository.as_deref().unwrap())
                .map_err(|error| Error::Input(format!("--repository: {error}")))?;
            let tag = parsed
                .tag
                .as_deref()
                .map(Tag::parse)
                .transpose()
                .map_err(|error| Error::Input(format!("--tag: {error}")))?;
            let summary = match (&parsed.layout, &parsed.from) {
                (Some(layout), None) => import_layout(&state, std::path::Path::new(layout), &repository, tag.as_ref())?,
                (None, Some(from)) => import_from(&state, from, &repository, tag.as_ref())?,
                _ => unreachable!("validated by parse_args"),
            };
            println!(
                "imported {} as {}:{} ({} manifests, {} blobs, {} referrers)",
                summary.root,
                repository.as_str(),
                summary.tag.as_str(),
                summary.manifests,
                summary.blobs,
                summary.referrers
            );
        }
        Command::Inspect => {
            let inspection = inspect(&state)?;
            if parsed.json {
                print!("{}", inspection.render_json());
            } else {
                println!("tags: {}", inspection.tags.join(" "));
                println!("manifests: {}", inspection.manifests.join(" "));
                println!("blobs: {}", inspection.blobs.join(" "));
                println!("referrers: {}", inspection.referrers.join(" "));
            }
        }
    }
    Ok(())
}

pub fn print_help() {
    println!(
        "myoci {version}\n\
         \n\
         usage: myoci <command> --state-dir DIR [options]\n\
         \n\
         commands:\n\
         \x20 init                         create the state directories\n\
         \x20 import --layout DIR          import an OCI image layout\n\
         \x20 import --from REFERENCE      import through oras {oras}\n\
         \x20 inspect [--json]             list tags, manifests, blobs, referrers\n\
         \n\
         import options:\n\
         \x20 --repository NAME            destination repository (required)\n\
         \x20 --tag TAG                    tag to assign (default: layout ref name or latest)",
        version = env!("CARGO_PKG_VERSION"),
        oras = crate::import::ORAS_VERSION
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_commands_and_rejects_ambiguous_imports() {
        let parsed = parse_args(&args(&[
            "import",
            "--state-dir",
            "state",
            "--layout",
            "layout",
            "--repository",
            "example/app",
            "--tag",
            "stable",
        ]))
        .unwrap();
        assert_eq!(parsed.command, Command::Import);
        assert_eq!(parsed.repository.as_deref(), Some("example/app"));
        assert!(parse_args(&args(&["import", "--state-dir", "s", "--repository", "a/b"])).is_err());
        assert!(parse_args(&args(&[
            "import",
            "--state-dir",
            "s",
            "--layout",
            "l",
            "--from",
            "f",
            "--repository",
            "a/b"
        ]))
        .is_err());
        assert!(parse_args(&args(&["init"])).is_err());
        assert!(parse_args(&args(&["inspect", "--state-dir", "s", "--json"])).is_ok());
    }
}
