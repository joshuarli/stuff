use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::publish::Publisher;
use super::render::{self, CargoUrls};
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
    pub published_state_dir: PathBuf,
    pub site_dir: PathBuf,
    pub urls: CargoUrls,
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
            published_state_dir: config.published_state_dir,
            site_dir: config.site_dir,
            urls: config.urls,
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
    published_state_dir: PathBuf,
    site_dir: PathBuf,
    urls: CargoUrls,
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
        let body = render::native_config_json(&CargoUrls::on_one_host(context.base_url.as_str()), context.auth_required);
        return if head {
            write_head(stream, 200, "application/json", body.len() as u64)?;
            stream.flush()
        } else {
            respond(stream, 200, "application/json", body.as_bytes())
        };
    }
    if let Some(rest) = path.strip_prefix("/blobs/")
        && let Some(checksum) = blob_checksum(rest)
        && !context.site_dir.join("blobs").join(rest).is_file()
    {
        return serve_published_blob(stream, context, checksum, head);
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

fn blob_checksum(segment: &str) -> Option<&str> {
    let checksum = segment.strip_suffix(".crate")?;
    let valid = checksum.len() == 64
        && checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    valid.then_some(checksum)
}

fn serve_published_blob(
    stream: &mut TcpStream,
    context: &Context,
    checksum: &str,
    head: bool,
) -> io::Result<()> {
    let local = Store::new(&context.published_state_dir).blob_path(checksum);
    if let Ok(file) = fs::File::open(&local) {
        write_head(stream, 200, "application/x-tar", file.metadata()?.len())?;
        if !head {
            let mut file = file;
            io::copy(&mut file, stream)?;
        }
        return stream.flush();
    }
    let location = context.urls.blob_url(checksum);
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(response.as_bytes())?;
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
            context.urls.clone(),
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
    use std::io::Read;

    fn get(server: &PublishServer, target: &str) -> String {
        let mut stream = TcpStream::connect(server.address).unwrap();
        write!(stream, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn a_blob_missing_from_the_site_is_served_locally_or_redirected_to_the_public_registry() {
        let root = tempfile::tempdir().unwrap();
        let published = root.path().join("published");
        let store = Store::new(&published);
        store.ensure_dirs().unwrap();
        let local = "a".repeat(64);
        fs::write(store.blob_path(&local), b"local bytes").unwrap();
        let server = PublishServer::start(ServerConfig {
            state_dir: root.path().join("work"),
            published_state_dir: published,
            site_dir: root.path().join("site"),
            urls: CargoUrls::on_one_host("https://crates.example.test/"),
            auth_required: false,
            token: "token".to_string(),
            limits: Limits::default(),
        })
        .unwrap();

        let served = get(&server, &format!("/blobs/{local}.crate"));
        assert!(served.starts_with("HTTP/1.1 200 "), "{served}");
        assert!(served.ends_with("local bytes"), "{served}");

        let remote = "b".repeat(64);
        let redirected = get(&server, &format!("/blobs/{remote}.crate"));
        assert!(redirected.starts_with("HTTP/1.1 307 "), "{redirected}");
        assert!(
            redirected.contains(&format!(
                "Location: https://crates.example.test/blobs/{remote}.crate\r\n"
            )),
            "{redirected}"
        );

        for bad in ["..%2f..", "ABCDEF", &format!("{}.crate", "A".repeat(64)), "short.crate"] {
            let response = get(&server, &format!("/blobs/{bad}"));
            assert!(response.starts_with("HTTP/1.1 404 "), "{bad}: {response}");
        }
    }

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
