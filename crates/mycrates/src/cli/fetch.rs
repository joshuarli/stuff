use crate::acquire::fetch;
use crate::cli::{reject_unknown, take_value};
use crate::config::Config;
use crate::error::Result;
use std::env;
use std::path::PathBuf;

const USAGE: &str = "mycrates fetch [--state-dir DIR] [--config PATH]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut cfg_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            "--config" => {
                cfg_path = Some(PathBuf::from(take_value(args, &mut i, "--config")?));
                i += 1;
            }
            other => {
                return reject_unknown(other, USAGE);
            }
        }
    }

    let config_file = cfg_path.unwrap_or_else(|| state_dir.join("config.json"));
    let config = Config::load(&config_file)?;

    let summary = fetch(&state_dir, &config)?;
    println!(
        "fetched {} packages ({} bytes)",
        summary.fetched_count, summary.total_bytes
    );

    Ok(())
}
