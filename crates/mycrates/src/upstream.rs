#![forbid(unsafe_code)]

use crate::config::LimitsConfig;
use crate::crate_file::{scan_crate_staged, ScanLimits};
use crate::error::{Error, Result};
use crate::index::shard_for_name;
use crate::model::IndexEntry;
use h12tiny_client_sync::{http, Client};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::time::Duration;

pub struct UpstreamClient {
    pub index_url: String,
    pub limits: LimitsConfig,
    client: Client,
}

impl UpstreamClient {
    pub fn new(index_url: String, limits: LimitsConfig) -> Result<Self> {
        check_url_security(&index_url)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(limits.request_timeout_secs))
            .build();
        Ok(Self { index_url, limits, client })
    }

    fn get(&self, url: &str) -> Result<(String, Vec<u8>)> {
        check_url_security(url)?;
        let timeout = Duration::from_secs(self.limits.request_timeout_secs);
        let max_redirects = self.limits.redirects as usize;
        let mut current = url.to_string();
        for _ in 0..=max_redirects {
            check_url_security(&current)?;
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri(current.clone())
                .header("user-agent", "mycrates/0.1.0")
                .body(Vec::new())
                .map_err(|e| Error::Network(format!("failed to build request for {current}: {e}")))?;
            let mut resp = self
                .client
                .request_with_timeout(req, Some(timeout))
                .map_err(|e| Error::Network(format!("failed to fetch {current}: {e}")))?;
            let status = resp.status();
            if status.is_redirection() {
                let loc = resp
                    .headers()
                    .get(http::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        Error::Network(format!("redirect without Location from {current}"))
                    })?
                    .to_string();
                current = resolve_url(&current, &loc)?;
                continue;
            }
            if !status.is_success() {
                return Err(Error::Network(format!(
                    "failed to fetch {current}: HTTP {}",
                    status.as_u16()
                )));
            }
            let mut body = Vec::new();
            {
                let body_reader = resp.body_mut();
                let _ = body_reader.set_read_timeout(Some(timeout));
                let mut buf = [0u8; 65536];
                loop {
                    let n = body_reader
                        .read(&mut buf)
                        .map_err(|e| Error::Network(format!("stream read error: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                    if body.len() as u64 > self.limits.max_compressed + 10 * 1024 * 1024 {
                        return Err(Error::Limit("response exceeds size limit".to_string()));
                    }
                }
            }
            validate_response_url(&current, url)?;
            return Ok((current, body));
        }
        Err(Error::Network(format!("too many redirects fetching {url}")))
    }

    pub fn fetch_dl_template(&self) -> Result<String> {
        let url = format!("{}/config.json", self.index_url.trim_end_matches('/'));
        let (_final_url, body) = self.get(&url)?;
        let text = String::from_utf8(body)
            .map_err(|e| Error::Integrity(format!("invalid config.json from upstream: {e}")))?;
        let val: crate::json::Value = crate::json::from_str(&text)
            .map_err(|e| Error::Integrity(format!("invalid config.json from upstream: {e}")))?;
        let dl = val
            .get("dl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Integrity("missing dl in upstream config.json".to_string()))?;
        Ok(dl.to_string())
    }

    pub fn expand_dl_url(
        &self,
        dl_template: &str,
        name: &str,
        vers: &str,
        cksum: &str,
    ) -> Result<String> {
        let shard = shard_for_name(&name.to_lowercase());
        let prefix = if let Some((p, _)) = shard.rsplit_once('/') { p } else { "" };
        let lowerprefix = prefix.to_lowercase();

        let mut tmpl = if dl_template.contains("{crate}")
            || dl_template.contains("{version}")
            || dl_template.contains("{prefix}")
            || dl_template.contains("{lowerprefix}")
            || dl_template.contains("{sha256-checksum}")
        {
            dl_template.to_string()
        } else {
            format!("{}/{{crate}}/{{version}}/download", dl_template.trim_end_matches('/'))
        };

        tmpl = tmpl.replace("{crate}", name);
        tmpl = tmpl.replace("{version}", vers);
        tmpl = tmpl.replace("{prefix}", prefix);
        tmpl = tmpl.replace("{lowerprefix}", &lowerprefix);
        tmpl = tmpl.replace("{sha256-checksum}", cksum);

        if tmpl.contains('{') && tmpl.contains('}') {
            return Err(Error::Input(format!("unsupported placeholder in dl template: {tmpl}")));
        }

        check_url_security(&tmpl)?;
        Ok(tmpl)
    }

    pub fn fetch_index_entry(&self, name: &str, vers: &str) -> Result<(String, IndexEntry)> {
        let shard = shard_for_name(&name.to_lowercase());
        let url = format!("{}/{}", self.index_url.trim_end_matches('/'), shard);
        let (_final_url, body) = self.get(&url)?;
        let body_str = String::from_utf8(body)
            .map_err(|e| Error::Network(format!("failed to read index response: {e}")))?;

        for line in body_str.lines() {
            let line_trimmed = line.trim();
            if line_trimmed.is_empty() {
                continue;
            }
            if let Ok(entry) = crate::json::from_str::<IndexEntry>(line_trimmed)
                && entry.name == name
                && entry.vers == vers
            {
                return Ok((line_trimmed.to_string(), entry));
            }
        }

        Err(Error::Input(format!("crate {name} version {vers} not found in upstream index")))
    }

    pub fn download_crate(
        &self,
        dl_url: &str,
        expected_checksum: &str,
        name: &str,
        vers: &str,
    ) -> Result<tempfile::NamedTempFile> {
        check_url_security(dl_url)?;
        let timeout = Duration::from_secs(self.limits.request_timeout_secs);
        let max_redirects = self.limits.redirects as usize;
        let mut current = dl_url.to_string();
        let mut spill = tempfile::NamedTempFile::new().map_err(|e| Error::Operation(e.to_string()))?;
        let mut hasher = Sha256::new();
        let mut counted: u64 = 0;
        let mut final_url = String::new();
        let mut done = false;
        for _ in 0..=max_redirects {
            check_url_security(&current)?;
            let req = http::Request::builder()
                .method(http::Method::GET)
                .uri(current.clone())
                .header("user-agent", "mycrates/0.1.0")
                .body(Vec::new())
                .map_err(|e| Error::Network(format!("failed to build request for {current}: {e}")))?;
            let mut resp = self
                .client
                .request_with_timeout(req, Some(timeout))
                .map_err(|e| Error::Network(format!("failed to download crate from {current}: {e}")))?;
            let status = resp.status();
            if status.is_redirection() {
                let loc = resp
                    .headers()
                    .get(http::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        Error::Network(format!("redirect without Location from {current}"))
                    })?
                    .to_string();
                current = resolve_url(&current, &loc)?;
                continue;
            }
            if !status.is_success() {
                return Err(Error::Network(format!(
                    "failed to download crate from {current}: HTTP {}",
                    status.as_u16()
                )));
            }
            final_url = current.clone();
            {
                let body_reader = resp.body_mut();
                let _ = body_reader.set_read_timeout(Some(timeout));
                let mut buf = [0u8; 65536];
                loop {
                    let n = body_reader
                        .read(&mut buf)
                        .map_err(|e| Error::Network(format!("stream read error: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    counted += n as u64;
                    if counted > self.limits.max_compressed {
                        return Err(Error::Limit(
                            "downloaded crate exceeds max_compressed limit".to_string(),
                        ));
                    }
                    hasher.update(&buf[..n]);
                    std::io::Write::write_all(&mut spill, &buf[..n])
                        .map_err(|e| Error::Operation(e.to_string()))?;
                }
            }
            done = true;
            break;
        }
        if !done {
            return Err(Error::Network(format!("too many redirects downloading {dl_url}")));
        }

        validate_response_url(&final_url, dl_url)?;

        let actual_checksum = format!("{:x}", hasher.finalize_reset());
        if actual_checksum != expected_checksum {
            return Err(Error::Integrity(format!(
                "download checksum mismatch for {} {}: expected {}, got {}",
                name, vers, expected_checksum, actual_checksum
            )));
        }

        let scan_limits = ScanLimits {
            max_compressed: self.limits.max_compressed,
            max_expanded: self.limits.max_expanded,
            max_entries: self.limits.max_entries,
            max_ratio: self.limits.max_ratio,
            max_manifest_bytes: 1024 * 1024,
        };

        let staged: std::fs::File = spill
            .as_file()
            .try_clone()
            .map_err(|e| Error::Operation(e.to_string()))?;
        let scan_res = scan_crate_staged(
            staged,
            counted,
            self.limits.max_compressed,
            Some(expected_checksum),
            name,
            vers,
            &scan_limits,
        )?;
        if scan_res.checksum != expected_checksum {
            return Err(Error::Integrity(format!(
                "scanned archive checksum mismatch for {} {}",
                name, vers
            )));
        }

        Ok(spill)
    }
}

fn resolve_url(base: &str, location: &str) -> Result<String> {
    if location.contains("://") {
        return Ok(location.to_string());
    }
    let base_url: url::Url = base
        .parse()
        .map_err(|_| Error::Network(format!("invalid redirect base: {base}")))?;
    base_url
        .join(location)
        .map(|u| u.to_string())
        .map_err(|_| Error::Network(format!("invalid redirect target: {location} (from {base})")))
}

fn validate_response_url(final_url: &str, original: &str) -> Result<()> {
    check_url_security(final_url).map_err(|_| {
        Error::Network(format!("redirect target forbidden: {final_url} (from {original})"))
    })
}

fn check_url_security(url: &str) -> Result<()> {
    let parsed: url::Url = url.parse().map_err(|_| {
        Error::Input(format!("invalid URL (HTTPS required unless loopback): {url}"))
    })?;
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(Error::Input(format!("credentials in URL forbidden: {url}")));
    }
    match parsed.scheme() {
        "https" => Ok(()),
        "http" => match parsed.host_str() {
            Some("127.0.0.1") | Some("localhost") | Some("::1") | Some("[::1]") => Ok(()),
            _ => Err(Error::Input(format!(
                "insecure URL forbidden (HTTPS required unless loopback): {url}"
            ))),
        },
        _ => Err(Error::Input(format!(
            "insecure URL forbidden (HTTPS required unless loopback): {url}"
        ))),
    }
}
