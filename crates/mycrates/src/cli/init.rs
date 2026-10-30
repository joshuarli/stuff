use crate::cli::{reject_unknown, take_value};
use crate::config::Config;
use crate::error::Result;
use crate::store::Store;
use std::env;
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "mycrates init [--state-dir DIR] [--site-dir DIR]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut site_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/site");

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            "--site-dir" => {
                site_dir = PathBuf::from(take_value(args, &mut i, "--site-dir")?);
                i += 1;
            }
            other => {
                return reject_unknown(other, USAGE);
            }
        }
    }

    let store = Store::new(state_dir.clone());
    store.ensure_dirs()?;
    fs::create_dir_all(&site_dir)?;

    let cfg_path = state_dir.join("config.json");
    if !cfg_path.exists() {
        let cfg = Config::default();
        cfg.save(&cfg_path)?;
    }

    println!("initialized state at {}", state_dir.display());
    Ok(())
}
