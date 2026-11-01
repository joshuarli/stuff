#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use stuff_oci::{
    plan_blob, plan_error, plan_manifest, plan_method_not_allowed, plan_ping, plan_referrers,
    plan_tags, Body, Descriptor, Digest, Error as OciError, Method, Parsed, Reference,
    RepositoryName, Request, RequestHeaders, ResponsePlan,
};
use stuff_store::Site;
use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_MAX_CONNECTIONS: usize = 64;
type HeaderList = Vec<(String, String)>;
const MAX_HEADER_BYTES: usize = 65536;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub site_dir: PathBuf,
    pub bind: String,
    pub max_connections: usize,
    pub push: Option<crate::push::PushOptions>,
}

pub fn run(options: ServeOptions) -> Result<()> {
    let site = Site::new(options.site_dir.clone());
    site.check_no_symlinks_in_path()?;
    let listener = TcpListener::bind(&options.bind)
        .map_err(|e| Error::Operation(format!("could not bind {}: {e}", options.bind)))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::Operation(format!("could not configure listener: {e}")))?;
    let address = listener
        .local_addr()
        .map_err(|e| Error::Operation(format!("could not read listener address: {e}")))?;
    println!("listening on {address}");
    let _ = std::io::stdout().flush();
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let Some(guard) = try_admit(&active, options.max_connections) else {
                    refuse(stream);
                    continue;
                };
                let site_dir = options.site_dir.clone();
                let push = options.push.clone();
                std::thread::spawn(move || {
                    let _guard = guard;
                    handle_connection(stream, &site_dir, push.as_ref());
                });
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                eprintln!("myoci: accept failed: {error}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn try_admit(active: &Arc<AtomicUsize>, limit: usize) -> Option<ActiveGuard> {
    loop {
        let current = active.load(Ordering::SeqCst);
        if current >= limit {
            return None;
        }
        if active
            .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Some(ActiveGuard(active.clone()));
        }
    }
}

fn refuse(mut stream: TcpStream) {
    let body = b"too many concurrent requests";
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 503 Service Unavailable\r\nconnection: close\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(body);
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

fn handle_connection(
    mut stream: TcpStream,
    site_dir: &Path,
    push: Option<&crate::push::PushOptions>,
) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let Some(head) = read_request(&mut stream) else {
        send_text(&mut stream, 400, "Bad Request", b"malformed request");
        return;
    };
    let RequestHead { method, target, headers, body } = head;
    if let Some(push) = push
        && !matches!(Method::parse(&method), Method::Get | Method::Head)
    {
        crate::push::handle_front(&mut stream, &method, &target, &headers, &body, push);
        return;
    }
    let (plan, path) = plan_response(site_dir, &method, &target, &headers);
    send_plan(&mut stream, &plan, path.as_deref());
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

pub(crate) struct RequestHead {
    pub method: String,
    pub target: String,
    pub headers: HeaderList,
    pub body: Vec<u8>,
}

pub(crate) fn read_request(stream: &mut TcpStream) -> Option<RequestHead> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HEADER_BYTES {
            return None;
        }
        if bytes.windows(4).any(|window| window == b"\r\n\r\n")
            || bytes.windows(2).any(|window| window == b"\n\n")
        {
            break;
        }
    }
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
        .or_else(|| bytes.windows(2).position(|window| window == b"\n\n").map(|position| position + 2))?;
    let body = bytes[header_end..].to_vec();
    let mut header_buf = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut header_buf);
    if !matches!(request.parse(&bytes[..header_end]), Ok(httparse::Status::Complete(_))) {
        return None;
    }
    let method = request.method?.to_string();
    let target = request.path?.to_string();
    let mut headers = Vec::new();
    for header in request.headers.iter() {
        if !header.name.is_empty() {
            headers.push((
                header.name.to_ascii_lowercase(),
                String::from_utf8_lossy(header.value).to_string(),
            ));
        }
    }
    Some(RequestHead { method, target, headers, body })
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn plan_response(
    site_dir: &Path,
    method_text: &str,
    target: &str,
    headers: &[(String, String)],
) -> (ResponsePlan, Option<PathBuf>) {
    let method = Method::parse(method_text);
    match stuff_oci::parse_request(method, target) {
        Parsed::Request(Request::Ping) => (plan_ping(), None),
        Parsed::Request(Request::Manifest { repository, reference, .. }) => {
            let view = match load_view(site_dir, &repository) {
                Ok(Some(view)) => view,
                Ok(None) => {
                    return (
                        plan_error(&OciError::name_unknown(repository.as_str())),
                        None,
                    );
                }
                Err(_) => return (internal_error(), None),
            };
            let descriptor = match &reference {
                Reference::Tag(tag) => view.tags.get(tag).cloned(),
                Reference::Digest(digest) => view.manifests.get(digest).cloned(),
            };
            let Some(descriptor) = descriptor else {
                return (plan_error(&OciError::manifest_unknown(reference.as_str())), None);
            };
            let path = site_dir
                .join("oci/manifests/sha256")
                .join(descriptor.digest.hex());
            let Ok(metadata) = std::fs::metadata(&path) else {
                return (plan_error(&OciError::manifest_unknown(reference.as_str())), None);
            };
            let plan = plan_manifest(
                method,
                &reference,
                &descriptor.media_type,
                &descriptor.digest,
                metadata.len(),
            );
            (plan, Some(path))
        }
        Parsed::Request(Request::Blob { repository, digest, .. }) => {
            let view = match load_view(site_dir, &repository) {
                Ok(Some(view)) => view,
                Ok(None) => {
                    return (
                        plan_error(&OciError::name_unknown(repository.as_str())),
                        None,
                    );
                }
                Err(_) => return (internal_error(), None),
            };
            if view.is_manifest(&digest) {
                return (plan_error(&OciError::blob_unknown(digest.as_str())), None);
            }
            let path = site_dir.join("blobs/sha256").join(digest.hex());
            let Ok(metadata) = std::fs::metadata(&path) else {
                return (plan_error(&OciError::blob_unknown(digest.as_str())), None);
            };
            let range = header(headers, "range").map(str::to_string);
            let plan = plan_blob(method, &digest, metadata.len(), &RequestHeaders { range });
            (plan, Some(path))
        }
        Parsed::Request(Request::Tags { repository, pagination }) => {
            let view = match load_view(site_dir, &repository) {
                Ok(Some(view)) => view,
                Ok(None) => {
                    return (
                        plan_error(&OciError::name_unknown(repository.as_str())),
                        None,
                    );
                }
                Err(_) => return (internal_error(), None),
            };
            let tags: Vec<stuff_oci::Tag> = view.tags.keys().cloned().collect();
            (plan_tags(method, &repository, &tags, &pagination), None)
        }
        Parsed::Request(Request::Referrers { repository, subject, artifact_type, pagination }) => {
            let view = match load_view(site_dir, &repository) {
                Ok(Some(view)) => view,
                Ok(None) => {
                    return (
                        plan_error(&OciError::name_unknown(repository.as_str())),
                        None,
                    );
                }
                Err(_) => return (internal_error(), None),
            };
            let _ = view;
            let path = repository_dir(site_dir, &repository)
                .join("referrers/sha256")
                .join(format!("{}.json", subject.hex()));
            let descriptors = match std::fs::read_to_string(&path) {
                Ok(text) => match parse_referrer_descriptors(&text) {
                    Ok(descriptors) => descriptors,
                    Err(_) => return (internal_error(), None),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(_) => return (internal_error(), None),
            };
            (
                plan_referrers(
                    method,
                    &repository,
                    &subject,
                    &descriptors,
                    artifact_type.as_deref(),
                    &pagination,
                ),
                None,
            )
        }
        Parsed::MethodNotAllowed => (plan_method_not_allowed(), None),
        Parsed::NotFound => (plan_error(&OciError::name_unknown("unknown path")), None),
        Parsed::Invalid(error) => (plan_error(&error), None),
    }
}

fn load_view(site_dir: &Path, repository: &RepositoryName) -> Result<Option<stuff_oci::RepositoryView>> {
    let site = Site::new(site_dir.to_path_buf());
    let Some(generation) = site.current_generation()? else {
        return Ok(None);
    };
    let path = site
        .generations_dir()
        .join(generation)
        .join(repository_dir_relative(repository))
        .join("repository.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => stuff_oci::parse_repository_json(repository, &text)
            .map(Some)
            .map_err(|error| Error::Integrity(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Operation(format!("could not read {}: {error}", path.display()))),
    }
}

fn repository_dir(site_dir: &Path, repository: &RepositoryName) -> PathBuf {
    let site = Site::new(site_dir.to_path_buf());
    match site.current_generation() {
        Ok(Some(generation)) => site
            .generations_dir()
            .join(generation)
            .join(repository_dir_relative(repository)),
        _ => site_dir.to_path_buf(),
    }
}

fn repository_dir_relative(repository: &RepositoryName) -> PathBuf {
    let mut path = PathBuf::from("repositories");
    for component in repository.as_str().split('/') {
        path.push(component);
    }
    path
}

fn parse_referrer_descriptors(text: &str) -> std::result::Result<Vec<Descriptor>, stuff_oci::Error> {
    let value: deser_value::Value = deser_json::from_str(text)
        .map_err(|error| stuff_oci::Error::manifest_invalid(format!("referrers: {error}")))?;
    let map = value
        .as_map()
        .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrers must be an object"))?;
    let manifests = map
        .get("manifests")
        .and_then(|value| value.as_seq())
        .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrers.manifests must be an array"))?;
    let mut descriptors = Vec::with_capacity(manifests.len());
    for item in manifests.iter() {
        let descriptor = item
            .as_map()
            .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrer must be an object"))?;
        let media_type = descriptor
            .get("mediaType")
            .and_then(|value| value.as_str())
            .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrer.mediaType must be a string"))?;
        let digest = descriptor
            .get("digest")
            .and_then(|value| value.as_str())
            .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrer.digest must be a string"))?;
        let size = descriptor
            .get("size")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrer.size must be an integer"))?;
        let artifact_type = descriptor
            .get("artifactType")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let mut annotations = BTreeMap::new();
        if let Some(values) = descriptor.get("annotations").and_then(|value| value.as_map()) {
            for (key, value) in values.iter() {
                let key = key
                    .as_str()
                    .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrer annotation key"))?;
                let value = value
                    .as_str()
                    .ok_or_else(|| stuff_oci::Error::manifest_invalid("referrer annotation value"))?;
                annotations.insert(key.to_string(), value.to_string());
            }
        }
        descriptors.push(Descriptor {
            media_type: media_type.to_string(),
            digest: Digest::parse(digest)?,
            size,
            artifact_type,
            annotations,
            platform: None,
            urls: Vec::new(),
        });
    }
    Ok(descriptors)
}

fn internal_error() -> ResponsePlan {
    let body = b"{\"errors\":[{\"code\":\"UNKNOWN\",\"message\":\"internal error\"}]}".to_vec();
    ResponsePlan {
        status: 500,
        headers: vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("content-length".to_string(), body.len().to_string()),
            ("cache-control".to_string(), "no-store".to_string()),
        ],
        body: Body::Inline(body),
    }
}

fn send_text(stream: &mut TcpStream, status: u16, reason: &str, body: &[u8]) {
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\nconnection: close\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(body);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

pub(crate) fn send_plan(stream: &mut TcpStream, plan: &ResponsePlan, path: Option<&Path>) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nconnection: close\r\n",
        plan.status,
        reason(plan.status)
    );
    for (name, value) in &plan.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    match &plan.body {
        Body::Empty => {}
        Body::Inline(bytes) => {
            let _ = stream.write_all(bytes);
        }
        Body::Opaque { length } => {
            if let Some(path) = path {
                stream_file(stream, path, 0, *length);
            }
        }
        Body::Range { offset, length } => {
            if let Some(path) = path {
                stream_file(stream, path, *offset, *length);
            }
        }
    }
}

fn stream_file(stream: &mut TcpStream, path: &Path, offset: u64, length: u64) {
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    if offset != 0 && file.seek(std::io::SeekFrom::Start(offset)).is_err() {
        return;
    }
    let mut remaining = length;
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = match file.read(&mut buffer[..want]) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if stream.write_all(&buffer[..read]).is_err() {
            break;
        }
        remaining -= read as u64;
    }
}
