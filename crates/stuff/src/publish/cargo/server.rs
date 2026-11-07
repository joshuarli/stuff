use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::publish::Publisher;
use super::render;
use super::store::Store;
use super::wire::{self, FRAMING_SLACK};
use super::{Limits, NativeRecord, PUBLISH_RESPONSE, Rejection, accept, integrity};
use crate::publish::PublishError;

const MAX_HEADER_BYTES: usize = 65536;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub state_dir: PathBuf,
    pub site_dir: PathBuf,
    pub public_base_url: String,
    pub auth_required: bool,
    pub token: String,
    pub limits: Limits,
}

pub struct PublishServer {
    address: SocketAddr,
    base_url: String,
    token: String,
    shutdown: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PublishServer {
    pub fn start(config: ServerConfig) -> Result<Self, PublishError> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| {
            PublishError::Config(format!("could not bind the loopback publish server: {error}"))
        })?;
        let address = listener.local_addr().map_err(|error| {
            PublishError::Config(format!("could not read the loopback publish address: {error}"))
        })?;
        let base_url = format!("http://{address}");
        let token = config.token.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&shutdown);
        let context = Arc::new(Context {
            state_dir: config.state_dir,
            site_dir: config.site_dir,
            public_base_url: config.public_base_url,
            auth_required: config.auth_required,
            token: config.token,
            base_url: base_url.clone(),
            limits: config.limits,
        });
        let worker = std::thread::Builder::new()
            .name("stuff-cargo-publish".to_string())
            .spawn(move || accept_loop(listener, context, flag))
            .map_err(|error| {
                PublishError::Config(format!(
                    "could not start the loopback publish server: {error}"
                ))
            })?;
        Ok(Self {
            address,
            base_url,
            token,
            shutdown,
            worker: Some(worker),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for PublishServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(500));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Context {
    state_dir: PathBuf,
    site_dir: PathBuf,
    public_base_url: String,
    auth_required: bool,
    token: String,
    base_url: String,
    limits: Limits,
}

fn accept_loop(listener: TcpListener, context: Arc<Context>, shutdown: Arc<AtomicBool>) {
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let _ = handle_connection(stream, &context);
    }
}

fn handle_connection(stream: TcpStream, context: &Context) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let Some(request) = Request::read(&mut reader)? else {
        return Ok(());
    };
    let path = request.target.split('?').next().unwrap_or("").to_string();
    match request.method.as_str() {
        "GET" | "HEAD" => {
            let head = request.method == "HEAD";
            serve_read(&mut writer, context, &path, head)
        }
        "PUT" if path == "/api/v1/crates/new" => {
            serve_publish(&mut writer, &mut reader, context, &request)
        }
        _ => respond(&mut writer, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn read(reader: &mut BufReader<TcpStream>) -> io::Result<Option<Self>> {
        let mut total = 0;
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line)?;
            if read == 0 {
                return Ok(None);
            }
            total += read;
            if total > MAX_HEADER_BYTES {
                return Ok(None);
            }
            if line != "\r\n" && line != "\n" {
                break;
            }
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or_default().to_string();
        if method.is_empty() || target.is_empty() {
            return Ok(None);
        }
        let mut headers = Vec::new();
        loop {
            let mut header = String::new();
            let read = reader.read_line(&mut header)?;
            if read == 0 {
                break;
            }
            total += read;
            if total > MAX_HEADER_BYTES {
                return Ok(None);
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
            let header = header.trim_end_matches(['\r', '\n']);
            if let Some((name, value)) = header.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
        Ok(Some(Request {
            method,
            target,
            headers,
        }))
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn serve_read(stream: &mut TcpStream, context: &Context, path: &str, head: bool) -> io::Result<()> {
    if path == "/index/config.json" {
        let body = render::native_config_json(&context.base_url, context.auth_required);
        return if head {
            write_head(stream, 200, "application/json", body.len() as u64)?;
            stream.flush()
        } else {
            respond(stream, 200, "application/json", body.as_bytes())
        };
    }
    let (root, rest) = if let Some(rest) = path.strip_prefix("/index/") {
        (context.site_dir.join("current").join("registry"), rest)
    } else if let Some(rest) = path.strip_prefix("/blobs/") {
        (context.site_dir.join("blobs"), rest)
    } else if let Some(rest) = path.strip_prefix("/current/") {
        (context.site_dir.join("current"), rest)
    } else if let Some(rest) = path.strip_prefix("/generations/") {
        (context.site_dir.join("generations"), rest)
    } else {
        return respond(stream, 404, "text/plain; charset=utf-8", b"not found");
    };
    let Some(relative) = safe_relative(rest) else {
        return respond(stream, 404, "text/plain; charset=utf-8", b"not found");
    };
    let target = root.join(relative);
    let file = match fs::File::open(&target) {
        Ok(file) => file,
        Err(_) => return respond(stream, 404, "text/plain; charset=utf-8", b"not found"),
    };
    let size = file.metadata()?.len();
    let kind = content_type_for(&target);
    write_head(stream, 200, kind, size)?;
    if !head {
        let mut file = file;
        io::copy(&mut file, stream)?;
    }
    stream.flush()
}

fn serve_publish(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    context: &Context,
    request: &Request,
) -> io::Result<()> {
    let provided = request.header("authorization").unwrap_or_default();
    let provided = provided.strip_prefix("Bearer ").unwrap_or(provided).trim();
    if provided != context.token {
        return respond(stream, 401, "application/json", b"{\"errors\":[{\"detail\":\"unauthorized\"}]}");
    }
    if request
        .header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return respond(stream, 411, "application/json", b"{\"errors\":[{\"detail\":\"chunked transfer-encoding not supported\"}]}");
    }
    let Some(length) = request
        .header("content-length")
        .and_then(|value| value.trim().parse::<u64>().ok())
    else {
        return respond(stream, 411, "application/json", b"{\"errors\":[{\"detail\":\"content-length required\"}]}");
    };
    if length > context.limits.max_compressed.saturating_add(FRAMING_SLACK) {
        return respond(stream, 413, "application/json", b"{\"errors\":[{\"detail\":\"payload exceeds size limit\"}]}");
    }
    if request
        .header("expect")
        .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
    {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        stream.flush()?;
    }
    let uploads = context.state_dir.join("uploads");
    let staged = match wire::decode_publish_payload(
        reader,
        length,
        context.limits.max_compressed,
        &uploads,
    ) {
        Ok(staged) => staged,
        Err(rejection) => return respond_rejection(stream, &rejection),
    };
    let outcome = publish_upload(context, &staged);
    staged.discard();
    match outcome {
        Ok(()) => respond(stream, 200, "application/json", PUBLISH_RESPONSE.as_bytes()),
        Err(rejection) => respond_rejection(stream, &rejection),
    }
}

fn publish_upload(context: &Context, staged: &wire::StagedUpload) -> Result<(), Rejection> {
    let store = Store::new(&context.state_dir);
    store.ensure_dirs()?;
    let archive = fs::read(&staged.path).map_err(|error| {
        Rejection::new(
            super::RejectionKind::Operation,
            format!("could not read the staged upload: {error}"),
        )
    })?;
    let accepted = accept(&staged.body, &archive)?;
    store.with_lock(|| {
        if accepted.cksum != staged.crate_sha256 {
            return Err(integrity("checksum mismatch"));
        }
        let file = fs::File::open(&staged.path).map_err(|error| {
            Rejection::new(
                super::RejectionKind::Operation,
                format!("could not read the staged upload: {error}"),
            )
        })?;
        if store.ingest_blob(file)? != staged.crate_sha256 {
            return Err(integrity("checksum mismatch"));
        }
        store.put_native_record(&NativeRecord {
            entry: accepted.entry,
        })?;
        Publisher::new(
            &store,
            context.site_dir.clone(),
            context.public_base_url.clone(),
            context.auth_required,
        )
        .publish()?;
        Ok(())
    })
}

fn respond_rejection(stream: &mut TcpStream, rejection: &Rejection) -> io::Result<()> {
    respond(
        stream,
        rejection.status(),
        "application/json",
        rejection.body().as_bytes(),
    )
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write_head(stream, status, content_type, body.len() as u64)?;
    stream.write_all(body)?;
    stream.flush()
}

fn write_head(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    length: u64,
) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n",
        reason(status)
    );
    stream.write_all(head.as_bytes())
}

fn reason(status: u16) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => "application/json",
        Some("html") => "text/html; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("crate") => "application/x-tar",
        _ => "application/octet-stream",
    }
}

fn safe_relative(rest: &str) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for segment in rest.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.starts_with('.')
            || segment.contains('\\')
            || segment.contains('\0')
        {
            return None;
        }
        relative.push(segment);
    }
    Some(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_relative_paths_are_served() {
        assert_eq!(
            safe_relative("3/a/abcd"),
            Some(PathBuf::from("3/a/abcd"))
        );
        for bad in ["", "..", "a/../b", "a/./b", "a//b", ".hidden", "a/.hidden"] {
            assert!(safe_relative(bad).is_none(), "{bad}");
        }
    }
}
