#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use h12tiny_client_sync::{http, Client, ResponseBody};
use std::io::Read;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 6;
const MAX_INLINE_BODY: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct RawResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: ResponseBody,
}

pub struct RegistryHttp {
    client: Client,
}

impl RegistryHttp {
    pub fn new() -> Self {
        Self {
            client: Client::builder().connect_timeout(CONNECT_TIMEOUT).build(),
        }
    }

    pub fn get_json(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        max_body: u64,
    ) -> Result<HttpResponse> {
        let limit = max_body.min(MAX_INLINE_BODY);
        let raw = self.open(url, accept, token, 0)?;
        let status = raw.status;
        let headers = raw.headers;
        let mut body = Vec::new();
        drain(raw.body, limit, url, &mut body)?;
        Ok(HttpResponse { status, headers, body })
    }

    pub fn get_authed(
        &self,
        url: &str,
        accept: &str,
        repository: &str,
        max_body: u64,
    ) -> Result<HttpResponse> {
        let first = self.get_json(url, accept, None, max_body)?;
        if first.status != 401 {
            return Ok(first);
        }
        let Some(challenge) = first.header("www-authenticate").map(str::to_string) else {
            return Ok(first);
        };
        let Some(bearer) = parse_bearer_challenge(&challenge) else {
            return Ok(first);
        };
        let scope = bearer.scope.unwrap_or_else(|| format!("repository:{repository}:pull"));
        let mut token_url = stuff_url::Url::parse(&bearer.realm)
            .map_err(|error| Error::Network(format!("invalid realm in www-authenticate: {error}")))?;
        let query = format!(
            "service={}&scope={}",
            percent_encode(bearer.service.as_deref().unwrap_or("")),
            percent_encode(&scope)
        );
        token_url = stuff_url::Url::parse(&format!("{}?{query}", token_url.as_str().trim_end_matches(['?', '&'])))
            .map_err(|error| Error::Network(format!("invalid token URL: {error}")))?;
        let response = self.get_json(token_url.as_str(), "application/json", None, 1 << 20)?;
        if response.status != 200 {
            return Err(Error::Network(format!(
                "token endpoint returned HTTP {}",
                response.status
            )));
        }
        let value: deser_value::Value = deser_json::from_slice(&response.body)
            .map_err(|error| Error::Network(format!("token endpoint returned invalid JSON: {error}")))?;
        let token = value
            .as_map()
            .and_then(|map| {
                map.get("token")
                    .or_else(|| map.get("access_token"))
                    .and_then(|value| value.as_str())
            })
            .map(str::to_string)
            .ok_or_else(|| Error::Network("token endpoint response has no token".to_string()))?;
        self.get_json(url, accept, Some(&token), max_body)
    }

    pub fn get_to_file(
        &self,
        url: &str,
        max_bytes: u64,
        repository: &str,
        target: &std::path::Path,
    ) -> Result<u64> {
        let first = self.open(url, "application/octet-stream", None, 0)?;
        let raw = if first.status == 401 {
            drop(first.body);
            let Some(challenge) = first.headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("www-authenticate"))
                .map(|(_, value)| value.clone())
            else {
                return Err(Error::Network(format!("registry requires credentials for {url}")));
            };
            let Some(bearer) = parse_bearer_challenge(&challenge) else {
                return Err(Error::Network(format!("registry requires credentials for {url}")));
            };
            let scope = bearer.scope.unwrap_or_else(|| format!("repository:{repository}:pull"));
            let query = format!(
                "service={}&scope={}",
                percent_encode(bearer.service.as_deref().unwrap_or("")),
                percent_encode(&scope)
            );
            let token_url = format!("{}?{query}", bearer.realm.trim_end_matches(['?', '&']));
            let response = self.get_json(&token_url, "application/json", None, 1 << 20)?;
            if response.status != 200 {
                return Err(Error::Network(format!(
                    "token endpoint returned HTTP {}",
                    response.status
                )));
            }
            let value: deser_value::Value = deser_json::from_slice(&response.body)
                .map_err(|error| Error::Network(format!("token endpoint returned invalid JSON: {error}")))?;
            let token = value
                .as_map()
                .and_then(|map| {
                    map.get("token")
                        .or_else(|| map.get("access_token"))
                        .and_then(|value| value.as_str())
                })
                .map(str::to_string)
                .ok_or_else(|| Error::Network("token endpoint response has no token".to_string()))?;
            self.open(url, "application/octet-stream", Some(&token), 0)?
        } else {
            first
        };
        if raw.status == 404 {
            return Err(Error::Input(format!("{url} was not found")));
        }
        if raw.status != 200 {
            return Err(Error::Network(format!("{url} returned HTTP {}", raw.status)));
        }
        let mut reader = raw.body;
        let _ = reader.set_read_timeout(Some(REQUEST_TIMEOUT));
        let file = std::fs::File::create(target)
            .map_err(|error| Error::Operation(format!("could not create {}: {error}", target.display())))?;
        let mut writer = std::io::BufWriter::new(file);
        std::io::copy(&mut reader.by_ref().take(max_bytes.saturating_add(1)), &mut writer)
            .map_err(|error| Error::Network(format!("could not read {url}: {error}")))?;
        use std::io::Write as _;
        writer
            .flush()
            .map_err(|error| Error::Operation(format!("could not flush {}: {error}", target.display())))?;
        let written = std::fs::metadata(target)
            .map(|metadata| metadata.len())
            .map_err(|error| Error::Operation(format!("could not stat {}: {error}", target.display())))?;
        if written > max_bytes {
            let _ = std::fs::remove_file(target);
            return Err(Error::Limit(format!(
                "response from {url} exceeds the {max_bytes}-byte limit"
            )));
        }
        Ok(written)
    }

    fn open(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        depth: usize,
    ) -> Result<RawResponse> {
        let raw = self.send_with_retry(url, accept, token)?;
        if matches!(raw.status, 301 | 302 | 303 | 307 | 308) {
            let location = raw
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                .map(|(_, value)| value.clone());
            let Some(location) = location else {
                return Ok(raw);
            };
            drop(raw.body);
            if depth >= MAX_REDIRECTS {
                return Err(Error::Network(format!("too many redirects for {url}")));
            }
            let base = stuff_url::Url::parse(url)
                .map_err(|error| Error::Network(format!("invalid URL {url}: {error}")))?;
            let joined = base
                .join(&location)
                .map_err(|error| Error::Network(format!("invalid redirect {location}: {error}")))?;
            return self.open(joined.as_str(), accept, None, depth + 1);
        }
        Ok(raw)
    }

    fn send_with_retry(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
    ) -> Result<RawResponse> {
        let mut attempts = 0usize;
        loop {
            attempts += 1;
            match self.request_raw(url, accept, token) {
                Ok(raw) if is_transient_status(raw.status) && attempts < 2 => continue,
                Ok(raw) => return Ok(raw),
                Err(error @ Error::Network(_)) if attempts < 2 => {
                    let _ = error;
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn request_raw(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
    ) -> Result<RawResponse> {
        let mut builder = http::Request::builder()
            .method(http::Method::GET)
            .uri(url.to_string())
            .header("user-agent", concat!("myoci/", env!("CARGO_PKG_VERSION")))
            .header("accept", accept)
            .header("accept-encoding", "identity");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = builder
            .body(Vec::new())
            .map_err(|error| Error::Network(format!("request for {url}: {error}")))?;
        let response = self
            .client
            .request_with_timeout(request, Some(REQUEST_TIMEOUT))
            .map_err(|error| Error::Network(format!("could not reach {url}: {error}")))?;
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        Ok(RawResponse {
            status,
            headers,
            body: response.into_body(),
        })
    }
}

fn drain(mut body: ResponseBody, max_body: u64, url: &str, out: &mut Vec<u8>) -> Result<()> {
    let _ = body.set_read_timeout(Some(REQUEST_TIMEOUT));
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = body
            .read(&mut buffer)
            .map_err(|error| Error::Network(format!("could not read {url}: {error}")))?;
        if read == 0 {
            return Ok(());
        }
        if (out.len() + read) as u64 > max_body {
            return Err(Error::Limit(format!(
                "response from {url} exceeds the {max_body}-byte limit"
            )));
        }
        out.extend_from_slice(&buffer[..read]);
    }
}

pub fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

pub fn is_loopback(host: &str) -> bool {
    let host_only = host.split(':').next().unwrap_or(host);
    matches!(host_only, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

pub fn registry_base(registry: &str, repository: &str) -> String {
    let scheme = if is_loopback(registry) { "http" } else { "https" };
    let host = if registry == "docker.io" {
        "registry-1.docker.io"
    } else {
        registry
    };
    format!("{scheme}://{host}/v2/{repository}")
}

pub const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json,",
    "application/vnd.oci.image.manifest.v1+json,",
    "application/vnd.docker.distribution.manifest.list.v2+json,",
    "application/vnd.docker.distribution.manifest.v2+json"
);

struct BearerChallenge {
    realm: String,
    service: Option<String>,
    scope: Option<String>,
}

fn parse_bearer_challenge(challenge: &str) -> Option<BearerChallenge> {
    let rest = challenge.strip_prefix("Bearer ")?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for part in split_challenge_params(rest) {
        let (name, value) = part.split_once('=')?;
        let value = value.trim().trim_matches('"').to_string();
        match name.trim() {
            "realm" => realm = Some(value),
            "service" => service = Some(value),
            "scope" => scope = Some(value),
            _ => {}
        }
    }
    Some(BearerChallenge {
        realm: realm?,
        service,
        scope,
    })
}

fn split_challenge_params(rest: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in rest.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                parts.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
