#![forbid(unsafe_code)]

use std::io::{Read, Seek, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use stuff_server::{etag_for, respond, Config, ResponseBody, Route};

const DEFAULT_MAX_CONNECTIONS: usize = 64;

const NOT_FOUND: &[u8] = b"not found";
const HEADER_LINE_TOO_LONG: &[u8] = b"header line too long";
const HEADERS_TOO_LARGE: &[u8] = b"headers too large";
const TOO_MANY_REQUESTS: &[u8] = b"too many concurrent requests";

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

fn send_opened(stream: &mut TcpStream, mut file: std::fs::File, len: u64) {
    let mut remaining = len;
    let mut buf = [0u8; 65536];
    while remaining > 0 {
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
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut reader = match stream.try_clone() {
        Ok(s) => std::io::BufReader::new(s),
        Err(_) => return,
    };
    let mut header_bytes = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = match std::io::BufRead::read_until(&mut reader, b'\n', &mut line) {
            Ok(n) => n,
            Err(_) => return,
        };
        if n == 0 {
            return;
        }
        if line.len() > 8192 + 2 {
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
        header_bytes.extend_from_slice(&line);
        if header_bytes.len() > 65536 + 4096 || header_bytes.len() > 65536 {
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
        if header_bytes.ends_with(b"\r\n\r\n") || header_bytes.ends_with(b"\n\n") {
            break;
        }
    }

    let mut headers_buf = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers_buf);
    if !matches!(req.parse(&header_bytes), Ok(httparse::Status::Complete(_))) {
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

struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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

    static STOP: AtomicBool = AtomicBool::new(false);
    let active = Arc::new(AtomicUsize::new(0));
    while !STOP.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                if active.load(Ordering::SeqCst) >= max_connections {
                    send_text(&mut stream, 503, "Service Unavailable", TOO_MANY_REQUESTS);
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let guard = ActiveGuard(active.clone());
                let config = config.clone();
                std::thread::spawn(move || {
                    let _guard = guard;
                    handle_connection(stream, &config);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(Box::new(e)),
        }
    }
    Ok(())
}
