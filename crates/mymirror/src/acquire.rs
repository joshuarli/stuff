use h12tiny_client_sync::{http, Client};
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};

const USER_AGENT: &str = "mymirror/0.1.0";
const MAX_REDIRECTS: u32 = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub trait Acquire {
    fn fetch(&self, url: &str) -> Result<Vec<u8>>;
}

pub struct HttpsAcquire {
    client: Client,
}

impl HttpsAcquire {
    pub fn new() -> Self {
        Self {
            client: Client::builder().connect_timeout(CONNECT_TIMEOUT).build(),
        }
    }
}

impl Default for HttpsAcquire {
    fn default() -> Self {
        Self::new()
    }
}

impl Acquire for HttpsAcquire {
    fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        let mut current = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            current = validate_url(&current)?;
            let request = http::Request::builder()
                .method(http::Method::GET)
                .uri(current.as_str())
                .header("user-agent", USER_AGENT)
                .body(Vec::new())
                .map_err(|error| Error::transport(format!("{current}: {error}")))?;
            let mut response = self
                .client
                .request_with_timeout(request, Some(READ_TIMEOUT))
                .map_err(|error| Error::transport(format!("{current}: {error}")))?;
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .headers()
                    .get(http::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| Error::transport(format!("{current}: redirect without Location")))?;
                current = join_location(&current, &location)?;
                continue;
            }
            if status != 200 {
                return Err(Error::transport(format!("{current}: HTTP {status}")));
            }
            let mut body = Vec::new();
            {
                let reader = response.body_mut();
                let _ = reader.set_read_timeout(Some(READ_TIMEOUT));
                let mut chunk = [0u8; 65536];
                loop {
                    let read = reader
                        .read(&mut chunk)
                        .map_err(|error| Error::transport(format!("{current}: {error}")))?;
                    if read == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..read]);
                    if body.len() as u64 > MAX_BYTES {
                        return Err(Error::transport(format!("{current}: response exceeds {MAX_BYTES} bytes")));
                    }
                }
            }
            return Ok(body);
        }
        Err(Error::transport(format!("{url}: too many redirects")))
    }
}

fn validate_url(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::transport(format!("{url}: only https urls may be fetched")))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains('@') {
        return Err(Error::transport(format!("{url}: invalid host")));
    }
    let bare = host.split(':').next().unwrap_or("");
    if bare.parse::<IpAddr>().is_ok() {
        return Err(Error::transport(format!("{url}: refusing to fetch an IP literal host")));
    }
    let mut addrs: Vec<IpAddr> = (bare, 443)
        .to_socket_addrs()
        .map_err(|error| Error::transport(format!("could not resolve {bare}: {error}")))?
        .map(|socket| socket.ip())
        .collect();
    addrs.sort();
    addrs.dedup();
    if addrs.is_empty() {
        return Err(Error::transport(format!("could not resolve {bare}")));
    }
    if addrs.iter().any(is_restricted_address) {
        return Err(Error::transport(format!("{bare} resolved to a non-public address")));
    }
    Ok(url.to_string())
}

fn join_location(base: &str, location: &str) -> Result<String> {
    if location.starts_with("https://") {
        return Ok(location.to_string());
    }
    if location.starts_with('/') {
        let scheme_and_host_end = base
            .find("://")
            .and_then(|index| base[index + 3..].find('/').map(|slash| index + 3 + slash))
            .unwrap_or(base.len());
        return Ok(format!("{}{location}", &base[..scheme_and_host_end]));
    }
    Err(Error::transport(format!("{base}: unsupported relative redirect '{location}'")))
}

fn is_restricted_address(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let n = u32::from(*v4);
            const SUBNETS: &[(u32, u32)] = &[
                (0x00000000, 8),
                (0x0A000000, 8),
                (0x64400000, 10),
                (0x7F000000, 8),
                (0xA9FE0000, 16),
                (0xAC100000, 12),
                (0xC0000000, 24),
                (0xC0000200, 24),
                (0xC0586300, 24),
                (0xC0A80000, 16),
                (0xC6120000, 15),
                (0xCB007100, 24),
                (0xE0000000, 4),
                (0xF0000000, 4),
            ];
            SUBNETS.iter().any(|(base, prefix)| {
                let mask = if *prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
                (n & mask) == (base & mask)
            })
        }
        IpAddr::V6(v6) => {
            if v6.to_ipv4_mapped().is_some() {
                return true;
            }
            let n = u128::from(*v6);
            const SUBNETS: &[(u128, u32)] = &[
                (0x0064FF9B000000000000000000000000, 96),
                (0xFC000000000000000000000000000000, 7),
                (0xFE800000000000000000000000000000, 10),
                (0xFF000000000000000000000000000000, 8),
            ];
            SUBNETS.iter().any(|(base, prefix)| {
                let mask = if *prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
                (n & mask) == (base & mask)
            })
        }
    }
}

/// Fetches `url`, verifying the bytes against `expected_sha256`, reusing
/// and refreshing a content-addressed cache in `cache_dir`.
pub fn fetch_verified(
    acquire: &dyn Acquire,
    cache_dir: Option<&Path>,
    url: &str,
    expected_sha256: &str,
) -> Result<Vec<u8>> {
    let cached = cache_dir.map(|dir| dir.join(expected_sha256));
    if let Some(path) = &cached
        && let Ok(bytes) = std::fs::read(path)
        && sha256_hex(&bytes) == expected_sha256
    {
        return Ok(bytes);
    }
    let bytes = acquire.fetch(url)?;
    let actual = sha256_hex(&bytes);
    if actual != expected_sha256 {
        return Err(Error::integrity(format!(
            "{url}: sha256 mismatch (expected {expected_sha256}, got {actual})"
        )));
    }
    if let Some(path) = &cached {
        store_cached(path, &bytes)?;
    }
    Ok(bytes)
}

fn store_cached(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::operation("cache path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

pub(crate) fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    lower_hex(Sha256::digest(bytes))
}
