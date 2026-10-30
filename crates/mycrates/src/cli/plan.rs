use crate::acquire::plan;
use crate::cli::{reject_unknown, take_value};
use crate::error::Result;
use std::env;
use std::path::PathBuf;

const USAGE: &str = "mycrates plan [--state-dir DIR]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            other => {
                return reject_unknown(other, USAGE);
            }
        }
    }

    let items = plan(&state_dir)?;
    if items.is_empty() {
        println!("plan: no packages to fetch");
    } else {
        println!("plan: {} packages to fetch:", items.len());
        for it in items {
            println!("  {}@{} ({})", it.name, it.version, it.checksum);
        }
    }

    Ok(())
}
