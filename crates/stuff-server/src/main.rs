#![forbid(unsafe_code)]

use std::io::{Read, Seek, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use stuff_server::{respond, Config, ResponseBody, Route};

const DEFAULT_MAX_CONNECTIONS: usize = 64;

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

fn send_file(stream: &mut TcpStream, path: &std::path::Path, offset: u64, len: u64) {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 9\r\nContent-Type: text/plain\r\n\r\nnot found");
            return;
        }
    };
    if offset != 0 && file.seek(std::io::SeekFrom::Start(offset)).is_err() {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 9\r\nContent-Type: text/plain\r\n\r\nnot found");
        return;
    }
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
            send_response(&mut stream, &stuff_server::FileResponse {
                status: 400,
                reason: "Bad Request",
                headers: vec![("content-length".to_string(), "19".to_string())],
                body: ResponseBody::Empty,
            }, Some(b"header line too long"));
            return;
        }
        header_bytes.extend_from_slice(&line);
        if header_bytes.len() > 65536 + 4096 || header_bytes.len() > 65536 {
            send_response(&mut stream, &stuff_server::FileResponse {
                status: 400,
                reason: "Bad Request",
                headers: vec![("content-length".to_string(), "17".to_string())],
                body: ResponseBody::Empty,
            }, Some(b"headers too large"));
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
        let r = stuff_server::FileResponse {
            status: 404,
            reason: "Not Found",
            headers: vec![("content-length".to_string(), "9".to_string())],
            body: ResponseBody::Empty,
        };
        send_response(&mut stream, &r, Some(b"not found"));
        return;
    };
    let resp = respond(route, &rest, method, &headers);
    match &resp.body {
        ResponseBody::Empty => send_response(&mut stream, &resp, None),
        ResponseBody::Inline(bytes) => send_response(&mut stream, &resp, Some(bytes)),
        ResponseBody::File { path, offset, len } => {
            send_response(&mut stream, &resp, None);
            if method == "GET" {
                send_file(&mut stream, path, *offset, *len);
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
                    let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 26\r\nContent-Type: text/plain\r\n\r\ntoo many concurrent requests");
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
