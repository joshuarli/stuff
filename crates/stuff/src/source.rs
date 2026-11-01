#![forbid(unsafe_code)]

use h12tiny_client_sync::{http, Client};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl FetchResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

pub fn fetch_response(url: &str, accept: Option<&str>) -> Result<FetchResponse> {
    check_url_security(url)?;
    let client = Client::builder().connect_timeout(FETCH_TIMEOUT).build();
    let mut builder = http::Request::builder()
        .method(http::Method::GET)
        .uri(url.to_string())
        .header("user-agent", "stuff/0.1.0");
    if let Some(accept) = accept {
        builder = builder.header("accept", accept);
    }
    let req = builder
        .body(Vec::new())
        .map_err(|e| Error::Network(format!("failed to build request for {url}: {e}")))?;
    let mut resp = client
        .request_with_timeout(req, Some(FETCH_TIMEOUT))
        .map_err(|e| Error::Network(format!("failed to fetch {url}: {e}")))?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
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
                return Err(Error::Limit(format!("response exceeds size limit: {url}")));
            }
        }
    }
    Ok(FetchResponse { status, headers, body })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSite {
    pub site_root: PathBuf,
    pub generation_dir: PathBuf,
    catalog_file: PathBuf,
}

impl LocalSite {
    pub fn resolve(input: &Path) -> Result<Self> {
        let meta = std::fs::symlink_metadata(input)
            .map_err(|_| Error::usage(format!("catalog path does not exist: {}", input.display())))?;
        if meta.file_type().is_file() {
            let generation_dir = input.parent().unwrap_or_else(|| Path::new("."));
            let generation_dir = canonical_if_symlink(generation_dir);
            let site_root = site_root_for(&generation_dir);
            return Ok(Self { site_root, generation_dir, catalog_file: input.to_path_buf() });
        }
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
            return Err(Error::usage(format!(
                "catalog path must be a file or a real directory: {}",
                input.display()
            )));
        }
        let current = input.join("current");
        if std::fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
            let generation_dir = input.join("generations").join(current_generation_id(input)?);
            let catalog_file = generation_dir.join("catalog.json");
            return Ok(Self { site_root: input.to_path_buf(), generation_dir, catalog_file });
        }
        let catalog_file = input.join("catalog.json");
        if std::fs::symlink_metadata(&catalog_file).is_ok_and(|m| m.file_type().is_file()) {
            return Ok(Self {
                site_root: site_root_for(input),
                generation_dir: input.to_path_buf(),
                catalog_file,
            });
        }
        Err(Error::usage(format!(
            "directory has neither a current link nor a catalog.json: {}",
            input.display()
        )))
    }

    pub fn load_catalog(&self) -> Result<stuff_catalog::Catalog> {
        load_catalog_file(&self.catalog_file)
    }
}

fn canonical_if_symlink(path: &Path) -> PathBuf {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn site_root_for(generation_dir: &Path) -> PathBuf {
    let parent = generation_dir.parent();
    let parent_is_generations = parent
        .and_then(Path::file_name)
        .is_some_and(|name| name == "generations");
    if parent_is_generations {
        parent
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| generation_dir.to_path_buf())
    } else {
        generation_dir.to_path_buf()
    }
}

fn current_generation_id(site: &Path) -> Result<String> {
    let current = site.join("current");
    let target = std::fs::read_link(&current)
        .map_err(|e| Error::usage(format!("could not read current link {}: {e}", current.display())))?;
    let target_str = target.to_string_lossy().to_string();
    let id = target_str
        .strip_prefix("generations/")
        .ok_or_else(|| Error::usage(format!("current link points outside generations: {target_str}")))?;
    stuff_store::parse_generation_id(id)
        .ok_or_else(|| Error::usage(format!("current link has an unsafe generation target: {target_str}")))?;
    Ok(id.to_string())
}

pub fn load_local_catalog(dir_or_file: &str) -> Result<stuff_catalog::Catalog> {
    LocalSite::resolve(Path::new(dir_or_file))?.load_catalog()
}

fn load_catalog_file(file: &Path) -> Result<stuff_catalog::Catalog> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| Error::Operation(format!("could not read catalog {}: {e}", file.display())))?;
    stuff_catalog::parse_catalog_json(&text).map_err(|e| Error::Corrupt(format!("{}: {e}", file.display())))
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
