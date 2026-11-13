#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use h12tiny_client_sync::{http, Client};
use std::io::Read;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 6;
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

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
        let limit = max_body.min(MAX_BODY_BYTES);
        self.get_with_redirects(url, accept, token, limit, 0)
    }

    fn get_with_redirects(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        max_body: u64,
        depth: usize,
    ) -> Result<HttpResponse> {
        let response = self.send_with_retry(url, accept, token, max_body)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = response.header("location")
        {
            if depth >= MAX_REDIRECTS {
                return Err(Error::Network(format!("too many redirects for {url}")));
            }
            let base = stuff_url::Url::parse(url)
                .map_err(|error| Error::Network(format!("invalid URL {url}: {error}")))?;
            let joined = base
                .join(location)
                .map_err(|error| Error::Network(format!("invalid redirect {location}: {error}")))?;
            let next_accept = accept;
            return self.get_with_redirects(joined.as_str(), next_accept, None, max_body, depth + 1);
        }
        Ok(response)
    }

    fn send_with_retry(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        max_body: u64,
    ) -> Result<HttpResponse> {
        let mut attempts = 0usize;
        loop {
            attempts += 1;
            match self.request_once(url, accept, token, max_body) {
                Ok(response) if is_transient_status(response.status) && attempts < 2 => continue,
                Ok(response) => return Ok(response),
                Err(error @ Error::Network(_)) if attempts < 2 => {
                    let _ = error;
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn request_once(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        max_body: u64,
    ) -> Result<HttpResponse> {
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
        let mut response = self
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
        let mut body = Vec::new();
        let reader = response.body_mut();
        let _ = reader.set_read_timeout(Some(REQUEST_TIMEOUT));
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| Error::Network(format!("could not read {url}: {error}")))?;
            if read == 0 {
                break;
            }
            if (body.len() + read) as u64 > max_body {
                return Err(Error::Limit(format!(
                    "response from {url} exceeds the {max_body}-byte limit"
                )));
            }
            body.extend_from_slice(&buffer[..read]);
        }
        Ok(HttpResponse { status, headers, body })
    }
}

pub fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

pub fn registry_base(registry: &str, repository: &str) -> String {
    let host_only = registry.split(':').next().unwrap_or(registry);
    let is_loopback = matches!(host_only, "localhost" | "127.0.0.1" | "[::1]" | "::1");
    let scheme = if is_loopback { "http" } else { "https" };
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

impl RegistryHttp {
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
}

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
