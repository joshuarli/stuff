use crate::cli::{reject_unknown, take_value};
use crate::error::{Error, Result};
use crate::model::{CrateName, Version};
use crate::pins::{write_pins, PinEntry, Pins};
use crate::store::Store;
use std::env;
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "mycrates add <name>@<version> [--state-dir DIR]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut target: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            other if other.starts_with('-') => {
                return reject_unknown(other, USAGE);
            }
            _ => {
                if target.is_some() {
                    return reject_unknown(&args[i], USAGE);
                }
                target = Some(args[i].clone());
                i += 1;
            }
        }
    }

    let target_str = target.ok_or_else(|| Error::Input("usage: mycrates add <name>@<version>".to_string()))?;
    let (name_str, ver_str) = target_str
        .split_once('@')
        .ok_or_else(|| Error::Input("expected <name>@<version> format".to_string()))?;

    let crate_name = CrateName::new(name_str)?;
    let version = Version::new(ver_str)?;

    let store = Store::new(state_dir);
    store.ensure_dirs()?;

    let pins_path = store.pins_dir().join("default.toml");
    let mut pins = if pins_path.exists() {
        let content = fs::read_to_string(&pins_path)?;
        crate::toml::from_str(&content).map_err(|e| Error::Input(e.to_string()))?
    } else {
        Pins::default()
    };

    if !pins
        .registry
        .iter()
        .any(|p| p.name == crate_name.as_str() && p.version == version.as_str())
    {
        pins.registry.push(PinEntry {
            name: crate_name.as_str().to_string(),
            version: version.as_str().to_string(),
            checksum: String::new(),
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        });
        write_pins(&pins_path, &pins)?;
        println!("added {}@{}", crate_name.as_str(), version.as_str());
    } else {
        println!("already pinned {}@{}", crate_name.as_str(), version.as_str());
    }

    Ok(())
}
