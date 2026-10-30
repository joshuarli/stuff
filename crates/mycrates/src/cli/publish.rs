use crate::cli::{reject_unknown, take_value};
use crate::config::Config;
use crate::error::Result;
use crate::publish::Publisher;
use crate::store::Store;
use std::env;
use std::path::PathBuf;

const USAGE: &str = "mycrates publish [--state-dir DIR] [--site-dir DIR] [--config PATH]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut site_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/site");
    let mut cfg_path: Option<PathBuf> = None;

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

    let store = Store::new(state_dir);
    let publisher = Publisher::new(&store, site_dir, config.public_base_url);
    let res = publisher.publish()?;

    println!(
        "published generation {} (noop: {})",
        res.generation_id, res.is_noop
    );
    Ok(())
}
