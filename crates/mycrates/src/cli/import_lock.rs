use crate::cli::{reject_unknown, take_value};
use crate::error::{Error, Result};
use crate::lockfile::{classify, parse_lockfile, PkgKind};
use crate::pins::{write_pins, PinEntry, Pins};
use crate::store::Store;
use std::env;
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "mycrates import-lock <Cargo.lock> [--project <name>] [--state-dir DIR]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut lock_path: Option<PathBuf> = None;
    let mut project: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            "--project" => {
                project = Some(take_value(args, &mut i, "--project")?);
                i += 1;
            }
            other if other.starts_with('-') => {
                return reject_unknown(other, USAGE);
            }
            _ => {
                if lock_path.is_some() {
                    return reject_unknown(&args[i], USAGE);
                }
                lock_path = Some(PathBuf::from(&args[i]));
                i += 1;
            }
        }
    }

    let lock_file = lock_path.ok_or_else(|| Error::Input("usage: mycrates import-lock <Cargo.lock> [--project <name>]".to_string()))?;
    if !lock_file.exists() {
        return Err(Error::Input(format!("lockfile not found: {}", lock_file.display())));
    }

    let lock_str = fs::read_to_string(&lock_file)?;
    let parsed = parse_lockfile(&lock_str)?;

    let proj_name = project.unwrap_or_else(|| {
        lock_file
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string())
    });

    let mut registry_pins = Vec::new();
    let mut unsupported = Vec::new();

    for pkg in &parsed.package {
        let classified = classify(pkg);
        match classified.kind {
            PkgKind::CratesIo => {
                let cksum = pkg.checksum.clone().ok_or_else(|| {
                    Error::Integrity(format!(
                        "package {} {} missing checksum in Cargo.lock",
                        pkg.name, pkg.version
                    ))
                })?;
                let src = pkg.source.clone().unwrap_or_default();
                registry_pins.push(PinEntry {
                    name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    checksum: cksum,
                    source: src,
                });
            }
            PkgKind::Git => {
                unsupported.push(format!("git: {} {}", pkg.name, pkg.version));
            }
            PkgKind::AlternateRegistry { ref url } => {
                unsupported.push(format!("alternate-registry ({}): {} {}", url, pkg.name, pkg.version));
            }
            PkgKind::Path => {
                unsupported.push(format!("path: {} {}", pkg.name, pkg.version));
            }
            PkgKind::Unsupported => {
                unsupported.push(format!("unsupported: {} {}", pkg.name, pkg.version));
            }
            PkgKind::Workspace => {}
        }
    }

    let pins = Pins {
        project: Some(proj_name.clone()),
        lock_path: Some(lock_file.to_string_lossy().to_string()),
        lock_content: Some(lock_str),
        registry: registry_pins,
        unsupported: unsupported.clone(),
    };

    let store = Store::new(state_dir);
    store.ensure_dirs()?;

    let out_pins_path = store.pins_dir().join(format!("{}.toml", proj_name));
    write_pins(&out_pins_path, &pins)?;

    println!(
        "imported {} registry packages for project '{}'",
        pins.registry.len(),
        proj_name
    );
    if !unsupported.is_empty() {
        println!("warning: {} unsupported non-crates.io sources:", unsupported.len());
        for u in &unsupported {
            println!("  {}", u);
        }
    }

    Ok(())
}
