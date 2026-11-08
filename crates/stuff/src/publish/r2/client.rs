#![forbid(unsafe_code)]

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use h12tiny_client_sync::{http, Client};
use md5::{Digest as _, Md5};

use crate::publish::PublishError;

use super::sigv4::{self, SigningMaterial};
use super::xml::{self, ListedObject};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_ATTEMPTS: u32 = 6;
const DEFAULT_LIST_PAGE: usize = 1000;
const MAX_BACKOFF_SECONDS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
    pub session_token: Option<String>,
}

impl Credentials {
    pub fn from_env<F>(env: F) -> Result<Self, PublishError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let required = |name: &str| {
            env(name)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| PublishError::Config(format!("{name} is not set")))
        };
        Ok(Credentials {
            endpoint: required("R2_S3_ENDPOINT")?
                .trim_end_matches('/')
                .to_string(),
            access_key_id: required("R2_S3_ACCESS_KEY_ID")?,
            secret_access_key: required("R2_S3_SECRET_ACCESS_KEY")?,
            region: env("R2_S3_REGION")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "auto".to_string()),
            session_token: env("R2_S3_SESSION_TOKEN").filter(|value| !value.is_empty()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub size: u64,
    pub etag: String,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    None,
    IfMatch(String),
    IfNoneMatchAny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RawResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct RawRequest<'a> {
    pub method: &'a str,
    pub bucket: &'a str,
    pub key: &'a str,
    pub query: Vec<(&'a str, String)>,
    pub body: &'a [u8],
    pub content_type: Option<&'a str>,
    pub extra_headers: Vec<(&'a str, &'a str)>,
}

pub struct R2Client {
    credentials: Credentials,
    http: Client,
    sleeper: Arc<dyn Fn(Duration) + Send + Sync>,
    attempts: u32,
    list_page: usize,
    request_timeout: Duration,
}

impl R2Client {
    pub fn new(credentials: Credentials) -> Self {
        R2Client {
            credentials,
            http: Client::builder().connect_timeout(CONNECT_TIMEOUT).build(),
            sleeper: Arc::new(std::thread::sleep),
            attempts: DEFAULT_ATTEMPTS,
            list_page: DEFAULT_LIST_PAGE,
            request_timeout: REQUEST_TIMEOUT,
        }
    }

    pub fn with_sleeper<F>(mut self, sleeper: F) -> Self
    where
        F: Fn(Duration) + Send + Sync + 'static,
    {
        self.sleeper = Arc::new(sleeper);
        self
    }

    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts.max(1);
        self
    }

    pub fn with_list_page(mut self, page: usize) -> Self {
        self.list_page = page.clamp(1, DEFAULT_LIST_PAGE);
        self
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    pub fn request(&self, request: RawRequest<'_>) -> Result<RawResponse, PublishError> {
        self.execute(&request)
    }

    pub fn get_object(&self, bucket: &str, key: &str) -> Result<Vec<u8>, PublishError> {
        let response = self.execute(&RawRequest {
            method: "GET",
            bucket,
            key,
            query: Vec::new(),
            body: &[],
            content_type: None,
            extra_headers: Vec::new(),
        })?;
        self.check_status(&response, &format!("GET {bucket}/{key}"))?;
        Ok(response.body)
    }

    pub fn download_object_to_file(
        &self,
        bucket: &str,
        key: &str,
        destination: &Path,
    ) -> Result<(String, u64), PublishError> {
        let attempts = self.attempts.max(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self.download_once(bucket, key, destination) {
                Ok(result) => return Ok(result),
                Err(error) => {
                    if attempt == attempts {
                        last_error = Some(error);
                        break;
                    }
                    (self.sleeper)(backoff(attempt));
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            PublishError::Transport(format!("GET {bucket}/{key}: exhausted {attempts} attempts"))
        }))
    }

    pub fn head_object(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<ObjectMeta, PublishError> {
        let response = self.execute(&RawRequest {
            method: "HEAD",
            bucket,
            key,
            query: Vec::new(),
            body: &[],
            content_type: None,
            extra_headers: Vec::new(),
        })?;
        self.check_status(&response, &format!("HEAD {bucket}/{key}"))?;
        let size = response
            .header("content-length")
            .ok_or_else(|| {
                PublishError::Integrity(format!(
                    "HEAD {bucket}/{key}: response has no Content-Length"
                ))
            })?
            .parse::<u64>()
            .map_err(|_| {
                PublishError::Integrity(format!(
                    "HEAD {bucket}/{key}: response has an invalid Content-Length"
                ))
            })?;
        let etag = response
            .header("etag")
            .ok_or_else(|| {
                PublishError::Integrity(format!("HEAD {bucket}/{key}: response has no ETag"))
            })?
            .trim_matches('"')
            .to_string();
        Ok(ObjectMeta {
            size,
            etag,
            content_type: response.header("content-type").map(str::to_string),
        })
    }

    pub fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: &[u8],
        content_type: &str,
        precondition: Precondition,
    ) -> Result<String, PublishError> {
        let extra_headers = match &precondition {
            Precondition::None => Vec::new(),
            Precondition::IfMatch(etag) => vec![("if-match", etag.as_str())],
            Precondition::IfNoneMatchAny => vec![("if-none-match", "*")],
        };
        let response = self.execute(&RawRequest {
            method: "PUT",
            bucket,
            key,
            query: Vec::new(),
            body,
            content_type: Some(content_type),
            extra_headers,
        })?;
        self.check_status(&response, &format!("PUT {bucket}/{key}"))?;
        let etag = response
            .header("etag")
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        let expected = md5_hex(body);
        if !etag.contains('-') && etag != expected {
            return Err(PublishError::Integrity(format!(
                "PUT {bucket}/{key}: expected ETag {expected}, got {etag:?}"
            )));
        }
        Ok(etag)
    }

    pub fn list_objects_v2(&self, bucket: &str) -> Result<Vec<ListedObject>, PublishError> {
        let mut objects = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query: Vec<(&str, String)> = vec![
                ("list-type", "2".to_string()),
                ("max-keys", self.list_page.to_string()),
            ];
            if let Some(current) = &token {
                query.push(("continuation-token", current.clone()));
            }
            let response = self.execute(&RawRequest {
                method: "GET",
                bucket,
                key: "",
                query,
                body: &[],
                content_type: None,
                extra_headers: Vec::new(),
            })?;
            self.check_status(&response, &format!("list {bucket}"))?;
            let page = xml::parse_list_objects_v2(&response.body)?;
            objects.extend(page.objects);
            if !page.is_truncated {
                return Ok(objects);
            }
            match page.next_continuation_token {
                Some(next) if Some(&next) != token.as_ref() => token = Some(next),
                Some(_) => {
                    return Err(PublishError::Integrity(format!(
                        "list {bucket} repeated a continuation token"
                    )));
                }
                None => {
                    return Err(PublishError::Integrity(format!(
                        "list {bucket} was truncated without a continuation token"
                    )));
                }
            }
        }
    }

    fn execute(&self, request: &RawRequest<'_>) -> Result<RawResponse, PublishError> {
        let attempts = self.attempts.max(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self.send_once(request) {
                Ok(response) if response.status == 429 || response.status >= 500 => {
                    if attempt == attempts {
                        return Err(PublishError::Transport(format!(
                            "{} {}/{}: HTTP {} after {attempts} attempts",
                            request.method, request.bucket, request.key, response.status
                        )));
                    }
                    let delay = retry_after(&response).unwrap_or_else(|| backoff(attempt));
                    (self.sleeper)(delay);
                }
                Ok(response) => return Ok(response),
                Err(error) => {
                    if attempt == attempts {
                        last_error = Some(error);
                        break;
                    }
                    (self.sleeper)(backoff(attempt));
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            PublishError::Transport(format!(
                "{} {}/{}: exhausted {attempts} attempts",
                request.method, request.bucket, request.key
            ))
        }))
    }

    fn build_http_request(
        &self,
        request: &RawRequest<'_>,
    ) -> Result<(String, http::Request<Vec<u8>>), PublishError> {
        let path = object_path(request.bucket, request.key);
        let query = encode_query(&request.query);
        let url = if query.is_empty() {
            format!("{}{path}", self.credentials.endpoint)
        } else {
            format!("{}{path}?{query}", self.credentials.endpoint)
        };
        let (datestamp, amz_date) = sigv4::format_amz_date(now_seconds());
        let payload_hash = sigv4::sha256_hex(request.body);
        let signed = sigv4::sign_request(
            SigningMaterial {
                access_key_id: self.credentials.access_key_id.as_str(),
                secret_access_key: self.credentials.secret_access_key.as_str(),
                region: self.credentials.region.as_str(),
                session_token: self.credentials.session_token.as_deref(),
            },
            &sigv4::SignRequest {
                method: request.method,
                host: authority(self.credentials.endpoint.as_str()),
                path: &path,
                query: &query,
                payload_hash: &payload_hash,
                content_type: request.content_type,
                extra_headers: &request.extra_headers,
                amz_date: &amz_date,
                datestamp: &datestamp,
            },
        );
        let mut builder = http::Request::builder().method(request.method).uri(&url);
        for (name, value) in &signed.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        builder = builder.header("authorization", signed.authorization.as_str());
        let http_request = builder.body(request.body.to_vec()).map_err(|error| {
            PublishError::Config(format!("could not build {} {url}: {error}", request.method))
        })?;
        Ok((url, http_request))
    }

    fn send_once(&self, request: &RawRequest<'_>) -> Result<RawResponse, PublishError> {
        let (url, http_request) = self.build_http_request(request)?;
        let mut response = self
            .http
            .request_with_timeout(http_request, Some(self.request_timeout))
            .map_err(|error| {
                PublishError::Transport(format!("{} {url}: {error}", request.method))
            })?;
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let mut body = Vec::new();
        response
            .body_mut()
            .set_read_timeout(Some(self.request_timeout))
            .map_err(|error| {
                PublishError::Transport(format!("{} {url}: {error}", request.method))
            })?;
        response.body_mut().read_to_end(&mut body).map_err(|error| {
            PublishError::Transport(format!("{} {url}: {error}", request.method))
        })?;
        Ok(RawResponse { status, headers, body })
    }

    fn download_once(
        &self,
        bucket: &str,
        key: &str,
        destination: &Path,
    ) -> Result<(String, u64), PublishError> {
        let (url, http_request) = self.build_http_request(&RawRequest {
            method: "GET",
            bucket,
            key,
            query: Vec::new(),
            body: &[],
            content_type: None,
            extra_headers: Vec::new(),
        })?;
        let mut response = self
            .http
            .request_with_timeout(http_request, Some(self.request_timeout))
            .map_err(|error| PublishError::Transport(format!("GET {url}: {error}")))?;
        let status = response.status().as_u16();
        if status == 429 || status >= 500 {
            return Err(PublishError::Transport(format!("GET {url}: HTTP {status}")));
        }
        if status != 200 {
            let mut err_body = Vec::new();
            let _ = response.body_mut().read_to_end(&mut err_body);
            let detail = error_detail(&err_body);
            return Err(PublishError::Integrity(format!(
                "GET {url}: unexpected HTTP {status}: {detail}"
            )));
        }
        response
            .body_mut()
            .set_read_timeout(Some(self.request_timeout))
            .map_err(|error| PublishError::Transport(format!("GET {url}: {error}")))?;

        let mut file = std::fs::File::create(destination).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", destination.display()))
        })?;
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        let mut buf = [0u8; 65536];
        let mut total = 0u64;
        let mut reader = response.into_body();
        loop {
            let n = reader.read(&mut buf).map_err(|error| {
                PublishError::Transport(format!("GET {url}: {error}"))
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|error| {
                PublishError::Config(format!("could not write {}: {error}", destination.display()))
            })?;
            total += n as u64;
        }
        file.flush().map_err(|error| {
            PublishError::Config(format!("could not flush {}: {error}", destination.display()))
        })?;
        let digest_hex = crate::publish::lower_hex(hasher.finalize());
        Ok((digest_hex, total))
    }

    fn check_status(
        &self,
        response: &RawResponse,
        context: &str,
    ) -> Result<(), PublishError> {
        match response.status {
            200..=299 => Ok(()),
            401 | 403 => Err(PublishError::Config(format!(
                "{context}: HTTP {}: {}",
                response.status,
                error_detail(&response.body)
            ))),
            404 => Err(PublishError::Integrity(format!(
                "{context}: HTTP 404: {}",
                error_detail(&response.body)
            ))),
            412 => Err(PublishError::Conflict(format!(
                "{context}: HTTP 412 precondition failed"
            ))),
            status if status >= 500 => Err(PublishError::Transport(format!(
                "{context}: HTTP {status}: {}",
                error_detail(&response.body)
            ))),
            status => Err(PublishError::Integrity(format!(
                "{context}: unexpected HTTP {status}: {}",
                error_detail(&response.body)
            ))),
        }
    }
}

fn object_path(bucket: &str, key: &str) -> String {
    let bucket = sigv4::quote_component(bucket);
    if key.is_empty() {
        format!("/{bucket}")
    } else {
        let key = key
            .split('/')
            .map(sigv4::quote_component)
            .collect::<Vec<_>>()
            .join("/");
        format!("/{bucket}/{key}")
    }
}

fn encode_query(query: &[(&str, String)]) -> String {
    query
        .iter()
        .map(|(name, value)| {
            let name = sigv4::quote_component(name);
            if value.is_empty() {
                name
            } else {
                format!("{name}={}", sigv4::quote_component(value))
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn authority(endpoint: &str) -> &str {
    let rest = endpoint
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    rest.split('/').next().unwrap_or(rest)
}

fn md5_hex(bytes: &[u8]) -> String {
    sigv4::hex(Md5::digest(bytes))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn backoff(attempt: u32) -> Duration {
    let seconds = 2_u64
        .checked_pow(attempt)
        .unwrap_or(u64::MAX)
        .min(MAX_BACKOFF_SECONDS);
    Duration::from_secs(seconds)
}

fn retry_after(response: &RawResponse) -> Option<Duration> {
    response
        .header("retry-after")?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn error_detail(body: &[u8]) -> String {
    match xml::parse_error(body) {
        Ok(error) => error.to_string(),
        Err(_) => {
            let text = String::from_utf8_lossy(body);
            let snippet: String = text.chars().take(200).collect();
            if snippet.is_empty() {
                "no error body".to_string()
            } else {
                snippet
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ExitCode;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(key, value)| ((*key).to_string(), (*value).to_string())).collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn credentials_read_the_r2_s3_variables_in_one_place() {
        let credentials = Credentials::from_env(env(&[
            ("R2_S3_ENDPOINT", "https://account.r2.cloudflarestorage.com/"),
            ("R2_S3_ACCESS_KEY_ID", "access"),
            ("R2_S3_SECRET_ACCESS_KEY", "secret"),
            ("R2_S3_SESSION_TOKEN", "token"),
        ]))
        .unwrap();
        assert_eq!(
            credentials,
            Credentials {
                endpoint: "https://account.r2.cloudflarestorage.com".to_string(),
                access_key_id: "access".to_string(),
                secret_access_key: "secret".to_string(),
                region: "auto".to_string(),
                session_token: Some("token".to_string()),
            }
        );
    }

    #[test]
    fn missing_credentials_are_config_errors() {
        for missing in [
            "R2_S3_ENDPOINT",
            "R2_S3_ACCESS_KEY_ID",
            "R2_S3_SECRET_ACCESS_KEY",
        ] {
            let mut pairs = vec![
                ("R2_S3_ENDPOINT", "http://127.0.0.1:9000"),
                ("R2_S3_ACCESS_KEY_ID", "access"),
                ("R2_S3_SECRET_ACCESS_KEY", "secret"),
            ];
            pairs.retain(|(name, _)| *name != missing);
            let error = Credentials::from_env(env(&pairs)).unwrap_err();
            assert_eq!(error.classification(), ExitCode::Usage, "{missing}");
            assert!(error.to_string().contains(missing), "{error}");
        }
    }

    #[test]
    fn object_paths_quote_each_segment_like_the_python_oracle() {
        assert_eq!(object_path("bucket", "a b/c$d"), "/bucket/a%20b/c%24d");
        assert_eq!(object_path("stuff-mycrates", ""), "/stuff-mycrates");
    }

    #[test]
    fn authority_is_the_endpoint_netloc() {
        assert_eq!(authority("http://127.0.0.1:9000"), "127.0.0.1:9000");
        assert_eq!(
            authority("https://examplebucket.s3.amazonaws.com"),
            "examplebucket.s3.amazonaws.com"
        );
    }

    #[test]
    fn backoff_is_bounded_like_the_python_oracle() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(4));
        assert_eq!(backoff(6), Duration::from_secs(60));
        assert_eq!(backoff(40), Duration::from_secs(60));
    }

    #[test]
    fn valueless_query_keys_match_the_python_request_target() {
        assert_eq!(
            encode_query(&[
                ("uploads", String::new()),
                ("prefix", "a b".to_string()),
            ]),
            "uploads&prefix=a%20b"
        );
    }
}
