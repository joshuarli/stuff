use mycrates::config::LimitsConfig;
use mycrates::serve::{handle_publish_request, ServeConfig};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[allow(dead_code)]
pub struct TestServer {
    pub port: u16,
    pub prefix: String,
    pub token: Option<String>,
    pub stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[allow(dead_code)]
impl TestServer {
    pub fn start(
        state_dir: PathBuf,
        site_dir: PathBuf,
        prefix: &str,
        token: Option<String>,
        auth_required: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let prefix_clean = prefix.trim_matches('/').to_string();
        let base_url = if prefix_clean.is_empty() {
            format!("http://127.0.0.1:{}", port)
        } else {
            format!("http://127.0.0.1:{}/{}", port, prefix_clean)
        };

        let cfg = ServeConfig {
            bind: format!("127.0.0.1:{}", port),
            token: token.clone(),
            auth_required,
            state_dir: state_dir.clone(),
            site_dir: site_dir.clone(),
            public_base_url: base_url.clone(),
            limits: LimitsConfig::default(),
        };

        // Render initial generation so config.json is available
        let store = mycrates::store::Store::new(state_dir);
        let publ = mycrates::publish::Publisher::new(&store, site_dir, base_url);
        let _ = publ.publish();

        let prefix_copy = prefix_clean.clone();
        let handle = thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let cfg = cfg.clone();
                        let pfx = prefix_copy.clone();
                        thread::spawn(move || {
                            let _ = handle_test_connection(stream, &cfg, &pfx);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            prefix: prefix_clean,
            token,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        if self.prefix.is_empty() {
            format!("http://127.0.0.1:{}", self.port)
        } else {
            format!("http://127.0.0.1:{}/{}", self.port, self.prefix)
        }
    }

    pub fn registry_index_url(&self) -> String {
        format!("{}/current/registry/", self.base_url())
    }

    pub fn mirror_index_url(&self) -> String {
        format!("{}/current/crates-io/", self.base_url())
    }
}

fn handle_test_connection(stream: TcpStream, cfg: &ServeConfig, prefix: &str) -> std::io::Result<()> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut header_bytes = Vec::new();

    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        header_bytes.extend_from_slice(&line);
        if header_bytes.ends_with(b"\r\n\r\n") || header_bytes.ends_with(b"\n\n") {
            break;
        }
    }

    let mut headers_buf = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers_buf);
    if req.parse(&header_bytes).is_err() {
        return Ok(());
    }

    let method = req.method.unwrap_or_default().to_string();
    let raw_path = req.path.unwrap_or_default().to_string();
    let mut headers = Vec::new();
    for h in req.headers {
        if !h.name.is_empty() {
            let val = String::from_utf8_lossy(h.value).to_string();
            headers.push((h.name.to_string(), val));
        }
    }

    // Strip prefix if applicable
    let path = if !prefix.is_empty() {
        let pfx = format!("/{}", prefix);
        if raw_path.starts_with(&pfx) {
            raw_path[pfx.len()..].to_string()
        } else {
            raw_path.clone()
        }
    } else {
        raw_path.clone()
    };

    // Route /api/v1/crates/* to publish service
    if path.starts_with("/api/v1/crates") {
        let _ = handle_publish_request(&stream, &method, &path, &headers, &mut reader, cfg);
        return Ok(());
    }

    // Static site serving: blobs and index
    if method == "GET" || method == "HEAD" {
        serve_static_file(&stream, &method, &path, &headers, &cfg.site_dir);
        return Ok(());
    }

    send_response(&stream, 405, "Method Not Allowed", "text/plain", b"method not allowed");
    Ok(())
}

fn serve_static_file(
    mut stream: &TcpStream,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    site_dir: &std::path::Path,
) {
    let clean_path = path.trim_start_matches('/');

    let target_file = if clean_path.starts_with("blobs/") || clean_path.starts_with("current/") {
        site_dir.join(clean_path)
    } else {
        site_dir.join("current").join(clean_path)
    };

    if !target_file.exists() || target_file.is_dir() {
        send_response(stream, 404, "Not Found", "text/plain", b"not found");
        return;
    }

    let file_bytes = match fs::read(&target_file) {
        Ok(b) => b,
        Err(_) => {
            send_response(stream, 500, "Internal Server Error", "text/plain", b"error reading file");
            return;
        }
    };

    let mut h = Sha256::new();
    h.update(&file_bytes);
    let etag = format!("\"{:x}\"", h.finalize());

    if let Some((_, client_etag)) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("if-none-match"))
        && client_etag.trim() == etag
    {
        let resp = format!(
            "HTTP/1.1 304 Not Modified\r\nETag: {}\r\nConnection: close\r\n\r\n",
            etag
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let mime = if target_file.extension().and_then(|s| s.to_str()) == Some("crate") {
        "application/x-tar"
    } else if target_file.extension().and_then(|s| s.to_str()) == Some("json") {
        "application/json"
    } else {
        "text/plain"
    };

    let resp_hdr = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nETag: {}\r\nConnection: close\r\n\r\n",
        mime,
        file_bytes.len(),
        etag
    );
    let _ = stream.write_all(resp_hdr.as_bytes());
    if method == "GET" {
        let _ = stream.write_all(&file_bytes);
    }
}

fn send_response(mut stream: &TcpStream, status: u16, reason: &str, content_type: &str, body: &[u8]) {
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.write_all(body);
}
