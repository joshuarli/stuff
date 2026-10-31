#![forbid(unsafe_code)]

use h12tiny_client_sync::{http, Client};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};

pub const DEFAULT_CARGO_BASE: &str = "https://crates.17166969.xyz";
pub const DEFAULT_NPM_BASE: &str = "https://npm.17166969.xyz";
pub const DEFAULT_PYPI_BASE: &str = "https://pypi.17166969.xyz";

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CATALOG_BYTES: u64 = 32 * 1024 * 1024;

fn check_url_security(url: &str) -> Result<()> {
    let parsed: url::Url = url.parse().map_err(|_| Error::usage(format!("invalid URL: {url}")))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::usage(format!("credentials in URL forbidden: {url}")));
    }
    match parsed.scheme() {
        "https" => Ok(()),
        "http" => match parsed.host_str() {
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]") => Ok(()),
            _ => Err(Error::usage(format!("insecure URL forbidden (HTTPS required unless loopback): {url}"))),
        },
        _ => Err(Error::usage(format!("insecure URL forbidden (HTTPS required unless loopback): {url}"))),
    }
}

pub fn fetch_catalog(base_url: &str) -> Result<stuff_catalog::Catalog> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/current/catalog.json");
    check_url_security(&url)?;
    let client = Client::builder().connect_timeout(FETCH_TIMEOUT).build();
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(url.clone())
        .header("user-agent", "stuff/0.1.0")
        .body(Vec::new())
        .map_err(|e| Error::Network(format!("failed to build request for {url}: {e}")))?;
    let mut resp = client
        .request_with_timeout(req, Some(FETCH_TIMEOUT))
        .map_err(|e| Error::Network(format!("failed to fetch {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Network(format!("failed to fetch {url}: HTTP {}", resp.status().as_u16())));
    }
    let mut body = Vec::new();
    {
        let reader = resp.body_mut();
        let _ = reader.set_read_timeout(Some(FETCH_TIMEOUT));
        let mut buf = [0u8; 65536];
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| Error::Network(format!("stream read error from {url}: {e}")))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
            if body.len() as u64 > MAX_CATALOG_BYTES {
                return Err(Error::Limit(format!("catalog response exceeds size limit: {url}")));
            }
        }
    }
    let text =
        String::from_utf8(body).map_err(|_| Error::Corrupt(format!("catalog is not valid UTF-8: {url}")))?;
    stuff_catalog::parse_catalog_json(&text).map_err(|e| Error::Corrupt(format!("{url}: {e}")))
}

pub fn fetch_bytes(url: &str) -> Result<(u16, Vec<u8>)> {
    fetch_bytes_capped(url, MAX_CATALOG_BYTES)
}

pub fn fetch_bytes_capped(url: &str, cap: u64) -> Result<(u16, Vec<u8>)> {
    check_url_security(url)?;
    let client = Client::builder().connect_timeout(FETCH_TIMEOUT).build();
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri(url.to_string())
        .header("user-agent", "stuff/0.1.0")
        .body(Vec::new())
        .map_err(|e| Error::Network(format!("failed to build request for {url}: {e}")))?;
    let mut resp = client
        .request_with_timeout(req, Some(FETCH_TIMEOUT))
        .map_err(|e| Error::Network(format!("failed to fetch {url}: {e}")))?;
    let status = resp.status().as_u16();
    let mut body = Vec::new();
    {
        let reader = resp.body_mut();
        let _ = reader.set_read_timeout(Some(FETCH_TIMEOUT));
        let mut buf = [0u8; 65536];
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| Error::Network(format!("stream read error from {url}: {e}")))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&buf[..n]);
            if body.len() as u64 > cap {
                return Err(Error::Limit(format!("response exceeds size limit: {url}")));
            }
        }
    }
    Ok((status, body))
}

pub fn load_local_catalog(dir_or_file: &str) -> Result<stuff_catalog::Catalog> {
    let path = PathBuf::from(dir_or_file);
    let file = resolve_catalog_file(&path)?;
    let text = std::fs::read_to_string(&file)
        .map_err(|e| Error::Operation(format!("could not read catalog {}: {e}", file.display())))?;
    stuff_catalog::parse_catalog_json(&text).map_err(|e| Error::Corrupt(format!("{}: {e}", file.display())))
}

fn resolve_catalog_file(path: &Path) -> Result<PathBuf> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| Error::usage(format!("catalog path does not exist: {}", path.display())))?;
    if meta.file_type().is_file() {
        return Ok(path.to_path_buf());
    }
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        return Err(Error::usage(format!(
            "catalog path must be a file or a real directory: {}",
            path.display()
        )));
    }
    let direct = path.join("catalog.json");
    if let Ok(md) = std::fs::symlink_metadata(&direct)
        && md.file_type().is_file() {
            let current = path.join("current");
            if std::fs::symlink_metadata(&current).is_err() {
                return Ok(direct);
            }
        }
    let current = path.join("current");
    match std::fs::symlink_metadata(&current) {
        Ok(md) if md.file_type().is_symlink() => {
            let target = std::fs::read_link(&current)
                .map_err(|e| Error::usage(format!("could not read current link {}: {e}", current.display())))?;
            let target_str = target.to_string_lossy().to_string();
            let id = target_str.strip_prefix("generations/").ok_or_else(|| {
                Error::usage(format!("current link points outside generations: {target_str}"))
            })?;
            if id.is_empty() || id.contains('/') || id.contains('\0') {
                return Err(Error::usage(format!("current link has an unsafe generation target: {target_str}")));
            }
            let file = path.join("generations").join(id).join("catalog.json");
            if std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_file()) {
                return Ok(file);
            }
            Err(Error::usage(format!("current generation has no catalog.json: {}", file.display())))
        }
        _ => {
            if std::fs::symlink_metadata(&direct).is_ok_and(|m| m.file_type().is_file()) {
                Ok(direct)
            } else {
                Err(Error::usage(format!(
                    "directory has neither a current link nor a catalog.json: {}",
                    path.display()
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_generation_dir_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let text = "{\"schema_version\":1,\"registry\":\"npm\",\"generation\":\"g\",\"packages\":[{\"name\":\"a\",\"source\":null,\"latest\":\"1.0.0\",\"versions\":[{\"version\":\"1.0.0\",\"yanked\":false}]}]}\n";
        std::fs::write(dir.path().join("catalog.json"), text).unwrap();
        let catalog = load_local_catalog(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(catalog.packages.len(), 1);
        let file = dir.path().join("catalog.json");
        let same = load_local_catalog(file.to_str().unwrap()).unwrap();
        assert_eq!(same, catalog);
    }

    #[test]
    fn resolves_site_dir_through_current() {
        let dir = tempfile::tempdir().unwrap();
        let text = "{\"schema_version\":1,\"registry\":\"cargo\",\"generation\":\"gen1\",\"packages\":[]}\n";
        let gen_dir = dir.path().join("generations").join("gen1");
        std::fs::create_dir_all(&gen_dir).unwrap();
        std::fs::write(gen_dir.join("catalog.json"), text).unwrap();
        std::os::unix::fs::symlink("generations/gen1", dir.path().join("current")).unwrap();
        let catalog = load_local_catalog(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(catalog.generation, "gen1");
    }

    #[test]
    fn rejects_broken_paths() {
        assert!(load_local_catalog("/nonexistent-xyz/catalog.json").is_err());
    }

    #[test]
    fn corrupt_catalogs_exit_three() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("catalog.json");
        std::fs::write(&file, "{").unwrap();
        let err = load_local_catalog(file.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, Error::Corrupt(_)), "{err}");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn bad_source_urls_are_usage_errors() {
        let err = fetch_catalog("http://example.com").unwrap_err();
        assert!(matches!(err, Error::Usage(_)), "{err}");
        assert_eq!(err.exit_code(), 2);
    }
}
