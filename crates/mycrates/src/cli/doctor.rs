use crate::cli::{reject_unknown, take_value};
use crate::error::Result;
use crate::pins::Pins;
use crate::store::Store;
use std::env;
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "mycrates doctor [--state-dir DIR]";

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

    if !state_dir.exists() {
        println!("doctor: state directory does not exist at {}", state_dir.display());
        return Ok(());
    }

    let store = Store::new(state_dir.clone());
    let mut issues = 0;

    let pins_dir = store.pins_dir();
    if pins_dir.exists() {
        for entry in fs::read_dir(&pins_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                let content = fs::read_to_string(&path)?;
                if let Ok(pins) = crate::toml::from_str::<Pins>(&content) {
                    let proj = pins.project.unwrap_or_else(|| path.display().to_string());
                    if !pins.unsupported.is_empty() {
                        println!("project '{}' has {} unsupported sources:", proj, pins.unsupported.len());
                        for u in &pins.unsupported {
                            println!("  [unsupported] {}", u);
                            issues += 1;
                        }
                    }
                    for reg in &pins.registry {
                        let blob = store.blobs_dir().join(format!("{}.crate", reg.checksum));
                        if !blob.exists() {
                            println!("  [missing blob] {} {} ({})", reg.name, reg.version, reg.checksum);
                            issues += 1;
                        }
                    }
                }
            }
        }
    }

    if issues == 0 {
        println!("doctor: system healthy, no missing artifacts or unsupported sources detected");
    } else {
        println!("doctor: detected {} issues", issues);
    }

    Ok(())
}
