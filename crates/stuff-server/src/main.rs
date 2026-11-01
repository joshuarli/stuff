#![forbid(unsafe_code)]

use std::io::{Read, Seek, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use stuff_server::admission::try_admit;
use stuff_server::{etag_for, respond, Config, ResponseBody, Route};

const DEFAULT_MAX_CONNECTIONS: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_TRANSFER_BYTES_PER_SEC: u64 = 64 * 1024;
const MAX_HEADER_LINE: usize = 8192;
const MAX_HEADER_BYTES: usize = 65536;

const NOT_FOUND: &[u8] = b"not found";
const HEADER_LINE_TOO_LONG: &[u8] = b"header line too long";
const HEADERS_TOO_LARGE: &[u8] = b"headers too large";
const REQUEST_TIMED_OUT: &[u8] = b"request timed out";
const MALFORMED_REQUEST: &[u8] = b"malformed request";
const TOO_MANY_REQUESTS: &[u8] = b"too many concurrent requests";

struct DeadlineReader {
    stream: TcpStream,
    deadline: Instant,
}

impl DeadlineReader {
    fn new(stream: TcpStream, deadline: Instant) -> Self {
        Self { stream, deadline }
    }
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request deadline exceeded",
            ));
        }
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buf)
    }
}

fn timed_out(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn send_text(stream: &mut TcpStream, status: u16, reason: &str, body: &'static [u8]) {
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status} {reason}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(body);
}

fn refuse(mut stream: TcpStream, status: u16, reason: &str, body: &'static [u8]) {
    send_text(&mut stream, status, reason, body);
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_nonblocking(true);
    let mut sink = [0u8; 8192];
    for _ in 0..64 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

fn usage() -> ! {
    eprintln!("usage: stuff-server --bind ADDR --route PREFIX=DIR [--route PREFIX=DIR ...]");
    std::process::exit(2);
}

fn send_response(stream: &mut TcpStream, resp: &stuff_server::FileResponse, inline: Option<&[u8]>) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\n",
        resp.status, resp.reason
    );
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    if let Some(bytes) = inline {
        let _ = stream.write_all(bytes);
    }
}

fn open_verified(path: &std::path::Path) -> Option<std::fs::File> {
    use std::os::unix::fs::MetadataExt;
    let pre = std::fs::symlink_metadata(path).ok()?;
    if pre.file_type().is_symlink() || !pre.file_type().is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let post = file.metadata().ok()?;
    if !post.is_file() || post.dev() != pre.dev() || post.ino() != pre.ino() {
        return None;
    }
    Some(file)
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    if let Some(slot) = headers.iter_mut().find(|(k, _)| k == name) {
        slot.1 = value;
    } else {
        headers.push((name.to_string(), value));
    }
}

fn response_deadline(len: u64) -> Duration {
    REQUEST_TIMEOUT.max(Duration::from_secs(len.div_ceil(MIN_TRANSFER_BYTES_PER_SEC)))
}

fn send_opened(stream: &mut TcpStream, mut file: std::fs::File, len: u64) {
    let deadline = Instant::now() + response_deadline(len);
    let mut remaining = len;
    let mut buf = [0u8; 65536];
    while remaining > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_write_timeout(Some(left)).is_err() {
            break;
        }
        let want = remaining.min(buf.len() as u64) as usize;
        let n = match file.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if stream.write_all(&buf[..n]).is_err() {
            break;
        }
        remaining -= n as u64;
    }
}

fn handle_connection(mut stream: TcpStream, config: &Config) {
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
    let deadline = Instant::now() + REQUEST_TIMEOUT;

    let mut reader = match stream.try_clone() {
        Ok(s) => std::io::BufReader::new(DeadlineReader::new(s, deadline)),
        Err(_) => return,
    };
    let mut header_bytes = Vec::new();
    loop {
        let mut line = Vec::new();
        let limit = (MAX_HEADER_LINE + 3) as u64;
        let n = match std::io::BufRead::read_until(
            &mut reader.by_ref().take(limit),
            b'\n',
            &mut line,
        ) {
            Ok(n) => n,
            Err(ref e) if timed_out(e) => {
                send_text(&mut stream, 408, "Request Timeout", REQUEST_TIMED_OUT);
                return;
            }
            Err(_) => return,
        };
        if n == 0 {
            return;
        }
        if line.len() > MAX_HEADER_LINE + 2 {
            let mut resp = stuff_server::FileResponse {
                status: 400,
                reason: "Bad Request",
                headers: Vec::new(),
                body: ResponseBody::Empty,
            };
            set_header(&mut resp.headers, "content-length", HEADER_LINE_TOO_LONG.len().to_string());
            send_response(&mut stream, &resp, Some(HEADER_LINE_TOO_LONG));
            return;
        }
        if header_bytes.len() + line.len() > MAX_HEADER_BYTES {
            let mut resp = stuff_server::FileResponse {
                status: 400,
                reason: "Bad Request",
                headers: Vec::new(),
                body: ResponseBody::Empty,
            };
            set_header(&mut resp.headers, "content-length", HEADERS_TOO_LARGE.len().to_string());
            send_response(&mut stream, &resp, Some(HEADERS_TOO_LARGE));
            return;
        }
        header_bytes.extend_from_slice(&line);
        if header_bytes.ends_with(b"\r\n\r\n") || header_bytes.ends_with(b"\n\n") {
            break;
        }
    }

    let mut headers_buf = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers_buf);
    if !matches!(req.parse(&header_bytes), Ok(httparse::Status::Complete(_))) {
        send_text(&mut stream, 400, "Bad Request", MALFORMED_REQUEST);
        return;
    }
    let method = req.method.unwrap_or_default();
    let path = req.path.unwrap_or_default();
    let mut headers = Vec::new();
    for h in req.headers.iter() {
        if !h.name.is_empty() {
            headers.push((h.name.to_string(), String::from_utf8_lossy(h.value).to_string()));
        }
    }

    let Some((route, rest)) = config.match_route(path) else {
        send_text(&mut stream, 404, "Not Found", NOT_FOUND);
        return;
    };
    let mut resp = respond(route, &rest, method, &headers);
    match resp.body.clone() {
        ResponseBody::Empty => send_response(&mut stream, &resp, None),
        ResponseBody::Inline(bytes) => send_response(&mut stream, &resp, Some(&bytes)),
        ResponseBody::File { path, offset, len } => {
            let (mut file, fd_size, fd_mtime) = match open_verified(&path) {
                Some(opened) => {
                    let meta = opened.metadata().ok();
                    (
                        opened,
                        meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        meta.and_then(|m| m.modified().ok()),
                    )
                }
                None => {
                    send_text(&mut stream, 404, "Not Found", NOT_FOUND);
                    return;
                }
            };
            let (status, reason, send_len, content_range) = if resp.status == 206 {
                if offset >= fd_size || len == 0 {
                    let mut r = stuff_server::FileResponse {
                        status: 416,
                        reason: "Range Not Satisfiable",
                        headers: vec![(
                            "content-range".to_string(),
                            format!("bytes */{fd_size}"),
                        )],
                        body: ResponseBody::Empty,
                    };
                    if let Some(e) = etag_for(fd_size, fd_mtime) {
                        r.headers.push(("etag".to_string(), e));
                    }
                    send_response(&mut stream, &r, None);
                    return;
                }
                let clamped_end = (offset + len - 1).min(fd_size - 1);
                (
                    206u16,
                    "Partial Content",
                    clamped_end - offset + 1,
                    Some(format!("bytes {offset}-{clamped_end}/{fd_size}")),
                )
            } else {
                (200u16, "OK", fd_size, None)
            };
            if offset != 0 && file.seek(std::io::SeekFrom::Start(offset)).is_err() {
                send_text(&mut stream, 404, "Not Found", NOT_FOUND);
                return;
            }
            resp.status = status;
            resp.reason = reason;
            set_header(&mut resp.headers, "content-length", send_len.to_string());
            if let Some(e) = etag_for(fd_size, fd_mtime) {
                set_header(&mut resp.headers, "etag", e);
            }
            if let Some(cr) = content_range {
                set_header(&mut resp.headers, "content-range", cr);
            } else {
                resp.headers.retain(|(k, _)| k != "content-range");
            }
            send_response(&mut stream, &resp, None);
            if method == "GET" {
                send_opened(&mut stream, file, send_len);
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut bind: Option<std::net::SocketAddr> = None;
    let mut routes: Vec<(String, PathBuf)> = Vec::new();
    let mut max_connections = DEFAULT_MAX_CONNECTIONS;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => {
                let addr = args.next().unwrap_or_else(|| usage());
                bind = Some(addr.parse().unwrap_or_else(|_| usage()));
            }
            "--route" => {
                let spec = args.next().unwrap_or_else(|| usage());
                let (prefix, dir) = spec.split_once('=').unwrap_or_else(|| usage());
                routes.push((prefix.trim_matches('/').to_string(), PathBuf::from(dir)));
            }
            "--max-connections" => {
                let n = args.next().unwrap_or_else(|| usage());
                max_connections = n.parse().unwrap_or_else(|_| usage());
            }
            _ => usage(),
        }
    }
    if routes.is_empty() {
        usage();
    }
    let bind = bind.unwrap_or_else(|| "127.0.0.1:8000".parse().unwrap());

    let mut config_routes = Vec::new();
    for (prefix, site_dir) in routes {
        let canonical_site_dir = std::fs::canonicalize(&site_dir)?;
        config_routes.push(Route {
            prefix,
            site_dir,
            canonical_site_dir,
        });
    }
    let config = Arc::new(Config {
        routes: config_routes,
    });

    let listener = TcpListener::bind(bind)?;
    listener.set_nonblocking(true)?;
    println!("listening on {}", listener.local_addr()?);

    let active = Arc::new(AtomicUsize::new(0));
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let Some(guard) = try_admit(&active, max_connections) else {
                    refuse(stream, 503, "Service Unavailable", TOO_MANY_REQUESTS);
                    continue;
                };
                let config = config.clone();
                std::thread::spawn(move || {
                    let _guard = guard;
                    handle_connection(stream, &config);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("stuff-server: accept failed: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
