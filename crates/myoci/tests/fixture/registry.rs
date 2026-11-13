#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobKind {
    Layer,
    Config,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub manifest_get: u64,
    pub blob_get: u64,
    pub blob_head: u64,
    pub config_get: u64,
    pub referrers_get: u64,
    pub body_bytes: u64,
}

pub struct Fixture {
    pub address: String,
    state: Arc<Mutex<State>>,
    counts: Arc<Mutex<Counts>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

struct State {
    repo: String,
    blobs: HashMap<String, (Vec<u8>, BlobKind)>,
    manifests: HashMap<String, (Vec<u8>, String)>,
    tags: HashMap<String, String>,
    referrers: HashMap<String, Vec<String>>,
    referrers_status: u16,
}

impl Fixture {
    pub fn new(repo: &str) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let state = Arc::new(Mutex::new(State {
            repo: repo.to_string(),
            blobs: HashMap::new(),
            manifests: HashMap::new(),
            tags: HashMap::new(),
            referrers: HashMap::new(),
            referrers_status: 200,
        }));
        let counts = Arc::new(Mutex::new(Counts::default()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_counts = Arc::clone(&counts);
        let thread_shutdown = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || loop {
            if thread_shutdown.load(Ordering::Relaxed) {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                    let state = Arc::clone(&thread_state);
                    let counts = Arc::clone(&thread_counts);
                    std::thread::spawn(move || {
                        let _ = handle_connection(stream, state, counts);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => break,
            }
        });
        Fixture {
            address,
            state,
            counts,
            shutdown,
            handle: Some(handle),
        }
    }

    pub fn add_blob(&self, bytes: &[u8], _media_type: &str) -> String {
        let digest = digest_hex(bytes);
        self.state
            .lock()
            .unwrap()
            .blobs
            .insert(digest.clone(), (bytes.to_vec(), BlobKind::Layer));
        format!("sha256:{digest}")
    }

    pub fn add_config(&self, bytes: &[u8]) -> String {
        let digest = digest_hex(bytes);
        self.state
            .lock()
            .unwrap()
            .blobs
            .insert(digest.clone(), (bytes.to_vec(), BlobKind::Config));
        format!("sha256:{digest}")
    }

    pub fn add_manifest(&self, bytes: Vec<u8>, media_type: &str) -> String {
        let digest = digest_hex(&bytes);
        self.state
            .lock()
            .unwrap()
            .manifests
            .insert(digest.clone(), (bytes, media_type.to_string()));
        format!("sha256:{digest}")
    }

    pub fn add_tag(&self, tag: &str, digest: &str) {
        self.state.lock().unwrap().tags.insert(tag.to_string(), digest.to_string());
    }

    pub fn add_referrer(&self, subject: &str, descriptor_json: String) {
        self.state
            .lock()
            .unwrap()
            .referrers
            .entry(subject.trim_start_matches("sha256:").to_string())
            .or_default()
            .push(descriptor_json);
    }

    pub fn tamper_manifest(&self, digest: &str) {
        let hex = digest.trim_start_matches("sha256:").to_string();
        let mut state = self.state.lock().unwrap();
        if let Some((bytes, media_type)) = state.manifests.get_mut(&hex) {
            *bytes = b"tampered manifest bytes".to_vec();
            let _ = media_type;
        }
    }

    pub fn clear_referrers(&self, subject: &str) {
        let hex = subject.trim_start_matches("sha256:").to_string();
        self.state.lock().unwrap().referrers.remove(&hex);
    }

    pub fn set_referrers_status(&self, status: u16) {
        self.state.lock().unwrap().referrers_status = status;
    }

    pub fn counts(&self) -> Counts {
        *self.counts.lock().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn digest_hex(bytes: &[u8]) -> String {
    stuff_oci::Digest::from_bytes(bytes).hex().to_string()
}

pub fn config_json(os: &str, architecture: &str, variant: Option<&str>) -> Vec<u8> {
    let variant = variant
        .map(|variant| format!(r#","variant":"{variant}""#))
        .unwrap_or_default();
    format!(r#"{{"architecture":"{architecture}","created":"2026-01-01T00:00:00Z","os":"{os}"{variant},"rootfs":{{"type":"layers","diff_ids":["sha256:0000000000000000000000000000000000000000000000000000000000000001"]}}}}"#)
        .into_bytes()
}

pub fn manifest_json(
    config_digest: &str,
    config_size: u64,
    config_media_type: &str,
    layers: &[(String, u64, String)],
    subject: Option<(&str, u64, &str)>,
    artifact_type: Option<&str>,
) -> Vec<u8> {
    let layers_json: Vec<String> = layers
        .iter()
        .map(|(digest, size, media_type)| {
            format!(r#"{{"mediaType":"{media_type}","digest":"{digest}","size":{size}}}"#)
        })
        .collect();
    let subject_json = subject
        .map(|(digest, size, media_type)| {
            format!(r#","subject":{{"mediaType":"{media_type}","digest":"{digest}","size":{size}}}"#)
        })
        .unwrap_or_default();
    let artifact_json = artifact_type
        .map(|artifact_type| format!(r#","artifactType":"{artifact_type}""#))
        .unwrap_or_default();
    format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{config_media_type}","digest":"{config_digest}","size":{config_size}}},"layers":[{}]{subject_json}{artifact_json}}}"#,
        layers_json.join(",")
    )
    .into_bytes()
}

type ChildEntry<'a> = (&'a str, u64, &'a str, Option<(&'a str, &'a str, Option<&'a str>)>);

pub fn index_json(
    children: &[ChildEntry<'_>],
    subject: Option<(&str, u64, &str)>,
    artifact_type: Option<&str>,
) -> Vec<u8> {
    let children_json: Vec<String> = children
        .iter()
        .map(|(digest, size, media_type, platform)| {
            let platform_json = platform
                .map(|(os, architecture, variant)| {
                    let variant = variant
                        .map(|variant| format!(r#","variant":"{variant}""#))
                        .unwrap_or_default();
                    format!(r#","platform":{{"architecture":"{architecture}","os":"{os}"{variant}}}"#)
                })
                .unwrap_or_default();
            format!(r#"{{"mediaType":"{media_type}","digest":"{digest}","size":{size}{platform_json}}}"#)
        })
        .collect();
    let subject_json = subject
        .map(|(digest, size, media_type)| {
            format!(r#","subject":{{"mediaType":"{media_type}","digest":"{digest}","size":{size}}}"#)
        })
        .unwrap_or_default();
    let artifact_json = artifact_type
        .map(|artifact_type| format!(r#","artifactType":"{artifact_type}""#))
        .unwrap_or_default();
    format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{}]{subject_json}{artifact_json}}}"#,
        children_json.join(",")
    )
    .into_bytes()
}

pub fn referrer_descriptor(media_type: &str, digest: &str, size: u64, artifact_type: Option<&str>) -> String {
    let artifact = artifact_type
        .map(|artifact_type| format!(r#","artifactType":"{artifact_type}""#))
        .unwrap_or_default();
    format!(r#"{{"mediaType":"{media_type}","digest":"{digest}","size":{size}{artifact}}}"#)
}

fn handle_connection(
    stream: TcpStream,
    state: Arc<Mutex<State>>,
    counts: Arc<Mutex<Counts>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if method.is_empty() || target.is_empty() {
        return Ok(());
    }
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let trimmed = header.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
    }
    let state = Arc::clone(&state);
    let counts = Arc::clone(&counts);
    let mut counts_guard = counts.lock().unwrap();
    let response = route(
        &method,
        &target,
        &mut counts_guard,
        &state.lock().unwrap(),
    );
    drop(counts_guard);
    drop(state);
    let mut stream = stream;
    stream.write_all(&response)?;
    stream.flush()?;
    Ok(())
}

fn route(
    method: &str,
    target: &str,
    counts: &mut Counts,
    state: &State,
) -> Vec<u8> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let prefix = format!("/v2/{}/", state.repo);
    let Some(rest) = path.strip_prefix(&prefix) else {
        return error_response(404, "NAME_UNKNOWN", "repository not found");
    };
    if let Some(reference) = rest.strip_prefix("manifests/") {
        counts.manifest_get += 1;
        return manifest_response(method, reference, state, counts);
    }
    if let Some(digest) = rest.strip_prefix("blobs/") {
        return blob_response(method, digest, state, counts, query);
    }
    if let Some(subject) = rest.strip_prefix("referrers/") {
        counts.referrers_get += 1;
        return referrers_response(subject, state, query);
    }
    if rest == "tags/list" {
        return tags_response(state);
    }
    error_response(404, "NAME_UNKNOWN", "unsupported path")
}

fn manifest_response(method: &str, reference: &str, state: &State, counts: &mut Counts) -> Vec<u8> {
    let digest = if reference.starts_with("sha256:") {
        reference.to_string()
    } else {
        match state.tags.get(reference) {
            Some(digest) => digest.clone(),
            None => return error_response(404, "MANIFEST_UNKNOWN", "tag not found"),
        }
    };
    let hex = digest.trim_start_matches("sha256:").to_string();
    let Some((bytes, media_type)) = state.manifests.get(&hex) else {
        return error_response(404, "MANIFEST_UNKNOWN", "manifest not found");
    };
    counts.body_bytes += bytes.len() as u64;
    if method == "HEAD" {
        return response(
            200,
            &[
                ("content-type", media_type),
                ("docker-content-digest", &digest),
                ("content-length", &bytes.len().to_string()),
            ],
            &[],
        );
    }
    response(
        200,
        &[
            ("content-type", media_type),
            ("docker-content-digest", &digest),
            ("content-length", &bytes.len().to_string()),
        ],
        bytes,
    )
}

fn blob_response(
    method: &str,
    digest: &str,
    state: &State,
    counts: &mut Counts,
    _query: &str,
) -> Vec<u8> {
    let hex = digest.trim_start_matches("sha256:");
    let Some((bytes, kind)) = state.blobs.get(hex) else {
        return error_response(404, "BLOB_UNKNOWN", "blob not found");
    };
    match kind {
        BlobKind::Config => counts.config_get += 1,
        BlobKind::Layer => counts.blob_get += 1,
    }
    counts.body_bytes += bytes.len() as u64;
    if method == "HEAD" {
        counts.blob_head += 1;
        return response(
            200,
            &[
                ("content-type", "application/octet-stream"),
                ("docker-content-digest", digest),
                ("content-length", &bytes.len().to_string()),
            ],
            &[],
        );
    }
    response(
        200,
        &[
            ("content-type", "application/octet-stream"),
            ("docker-content-digest", digest),
            ("content-length", &bytes.len().to_string()),
        ],
        bytes,
    )
}

fn referrers_response(subject: &str, state: &State, query: &str) -> Vec<u8> {
    if state.referrers_status != 200 {
        return error_response(state.referrers_status, "UNSUPPORTED", "referrers API unavailable");
    }
    let hex = subject.trim_start_matches("sha256:").to_string();
    let mut descriptors = state.referrers.get(&hex).cloned().unwrap_or_default();
    if let Some(artifact) = query_param(query, "artifactType") {
        descriptors.retain(|descriptor| descriptor.contains(&format!(r#""artifactType":"{artifact}""#)));
    }
    let body = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{}]}}"#,
        descriptors.join(",")
    );
    response(
        200,
        &[
            ("content-type", "application/vnd.oci.image.index.v1+json"),
            ("content-length", &body.len().to_string()),
        ],
        body.as_bytes(),
    )
}

fn tags_response(state: &State) -> Vec<u8> {
    let mut tags: Vec<&String> = state.tags.keys().collect();
    tags.sort();
    let body = format!(
        r#"{{"name":"{}","tags":[{}]}}"#,
        state.repo,
        tags.iter().map(|tag| format!(r#""{tag}""#)).collect::<Vec<_>>().join(",")
    );
    response(
        200,
        &[
            ("content-type", "application/json"),
            ("content-length", &body.len().to_string()),
        ],
        body.as_bytes(),
    )
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then_some(value)
    })
}

fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let mut out = format!("HTTP/1.1 {status} {reason}\r\nconnection: close\r\n");
    for (name, value) in headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn error_response(status: u16, code: &str, message: &str) -> Vec<u8> {
    let body = format!(r#"{{"errors":[{{"code":"{code}","message":"{message}"}}]}}"#);
    response(
        status,
        &[
            ("content-type", "application/json"),
            ("content-length", &body.len().to_string()),
        ],
        body.as_bytes(),
    )
}
