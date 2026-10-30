use crate::cli::{reject_unknown, take_value};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::serve::{run_server, ServeConfig};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

const USAGE: &str = "mycrates serve-publish [--bind ADDR] [--token-file PATH] [--state-dir DIR] [--site-dir DIR] [--config PATH]";

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut site_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/site");
    let mut bind = "127.0.0.1:8080".to_string();
    let mut token_file: Option<PathBuf> = None;
    let mut cfg_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            "--site-dir" => {
                site_dir = PathBuf::from(take_value(args, &mut i, "--site-dir")?);
                i += 1;
            }
            "--bind" => {
                bind = take_value(args, &mut i, "--bind")?;
                i += 1;
            }
            "--token-file" => {
                token_file = Some(PathBuf::from(take_value(args, &mut i, "--token-file")?));
                i += 1;
            }
            "--config" => {
                cfg_path = Some(PathBuf::from(take_value(args, &mut i, "--config")?));
                i += 1;
            }
            _ => {
                return reject_unknown(&args[i], USAGE);
            }
        }
    }

    let config_file = cfg_path.unwrap_or_else(|| state_dir.join("config.json"));
    let config = Config::load(&config_file)?;

    let token = if let Some(ref tf) = token_file {
        if !tf.exists() {
            return Err(Error::Input(format!("token file not found: {}", tf.display())));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(tf)?.permissions().mode();
            if mode & 0o077 != 0 {
                return Err(Error::Input(format!(
                    "token file permissions must be 0600 (got {:o})",
                    mode & 0o777
                )));
            }
        }
        Some(fs::read_to_string(tf)?.trim().to_string())
    } else {
        env::var("MYCRATES_TOKEN").ok()
    };

    let serve_cfg = ServeConfig {
        bind: bind.clone(),
        token,
        auth_required: config.auth_required,
        state_dir,
        site_dir,
        public_base_url: config.public_base_url,
        limits: config.limits,
    };

    if serve_cfg.token.as_ref().is_none_or(|t| t.is_empty()) {
        return Err(Error::Input(
            "refusing to serve the publish API without a token: set --token-file or MYCRATES_TOKEN".to_string(),
        ));
    }

    println!("listening on {}", bind);
    let stop = Arc::new(AtomicBool::new(false));
    run_server(serve_cfg, stop)
}
