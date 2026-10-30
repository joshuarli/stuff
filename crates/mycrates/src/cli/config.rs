use crate::cli::{reject_unknown, take_value};
use crate::config::Config;
use crate::error::{Error, Result};
use std::env;
use std::path::PathBuf;

const USAGE: &str = "mycrates config set-public-base-url <URL> [--state-dir DIR]";

fn check_url(url: &str) -> Result<()> {
    let parsed: url::Url =
        url.parse().map_err(|_| Error::Input(format!("invalid URL (HTTPS required unless loopback): {url}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::Input(format!("credentials in URL forbidden: {url}")));
    }
    match parsed.scheme() {
        "https" => Ok(()),
        "http" => match parsed.host_str() {
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]") => Ok(()),
            _ => Err(Error::Input(format!("insecure URL forbidden (HTTPS required unless loopback): {url}"))),
        },
        _ => Err(Error::Input(format!("insecure URL forbidden (HTTPS required unless loopback): {url}"))),
    }
}

pub fn run(args: &[String]) -> Result<()> {
    let mut state_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".work/state");
    let mut url: Option<String> = None;
    let mut subcommand: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(take_value(args, &mut i, "--state-dir")?);
                i += 1;
            }
            "set-public-base-url" if subcommand.is_none() => {
                subcommand = Some("set-public-base-url".to_string());
                i += 1;
                if i < args.len() && !args[i].starts_with('-') {
                    url = Some(args[i].clone());
                    i += 1;
                }
            }
            other => {
                return reject_unknown(other, USAGE);
            }
        }
    }

    match subcommand.as_deref() {
        Some("set-public-base-url") => {
            let url = url.ok_or_else(|| Error::Input(format!("missing URL\nusage: {USAGE}")))?;
            check_url(&url)?;
            let cfg_path = state_dir.join("config.json");
            let mut cfg = Config::load(&cfg_path)?;
            cfg.public_base_url = url.clone();
            cfg.save(&cfg_path)?;
            println!("set public_base_url to {url}");
            Ok(())
        }
        _ => Err(Error::Input(format!("missing subcommand\nusage: {USAGE}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_insecure_urls() {
        assert!(check_url("http://example.com/").is_err());
        assert!(check_url("https://example.com/").is_ok());
        assert!(check_url("http://127.0.0.1:8080").is_ok());
        assert!(check_url("https://user@example.com/").is_err());
    }
}
