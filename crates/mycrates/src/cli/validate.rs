use crate::cli::{reject_unknown, take_value};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::store::Store;
use crate::validate::validate_project;
use std::env;
use std::fs;
use std::path::PathBuf;

const USAGE: &str = "mycrates validate [--state-dir DIR] [--project NAME] [--project-dir DIR] [--manifest-path PATH] [--mirror-url URL] [--config PATH]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut project: Option<String> = None;
    let mut project_dir: Option<PathBuf> = None;
    let mut manifest_path: Option<PathBuf> = None;
    let mut mirror_url: Option<String> = None;
    let mut cfg_path: Option<PathBuf> = None;

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
            "--project-dir" => {
                project_dir = Some(PathBuf::from(take_value(args, &mut i, "--project-dir")?));
                i += 1;
            }
            "--manifest-path" => {
                manifest_path = Some(PathBuf::from(take_value(args, &mut i, "--manifest-path")?));
                i += 1;
            }
            "--mirror-url" => {
                mirror_url = Some(take_value(args, &mut i, "--mirror-url")?);
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

    if project_dir.is_some() && manifest_path.is_some() {
        return Err(Error::Input(
            "validate: supply only one of --project-dir and --manifest-path".to_string(),
        ));
    }
    let project_source: Option<PathBuf> = project_dir.or(manifest_path);

    let config_file = cfg_path.unwrap_or_else(|| state_dir.join("config.json"));
    let config = Config::load(&config_file).unwrap_or_default();
    let effective_mirror_url = mirror_url.unwrap_or_else(|| {
        format!("{}/current/crates-io/", config.public_base_url.trim_end_matches('/'))
    });

    let store = Store::new(state_dir.clone());
    let pins_dir = store.pins_dir();

    if !pins_dir.exists() {
        println!("validate: no pinned projects found in {}", pins_dir.display());
        return Ok(());
    }

    let mut validated = 0;
    for entry in fs::read_dir(&pins_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            if project.as_ref().is_some_and(|p| p != &stem) {
                continue;
            }
            let res = validate_project(
                &state_dir,
                &path,
                &effective_mirror_url,
                project_source.as_deref(),
            )?;
            println!(
                "project '{}': {} registry packages verified (partial: {})",
                res.project, res.registry_count, res.is_partial
            );
            validated += 1;
        }
    }

    if validated == 0 && let Some(ref p) = project {
        return Err(Error::Input(format!("project '{}' not found in pins", p)));
    }

    Ok(())
}
