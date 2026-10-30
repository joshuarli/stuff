use crate::config::LimitsConfig;
use crate::crate_file::{parse_packaged_manifest, scan_crate_staged, ScanLimits};
use crate::error::{Error, Result};
use crate::index::publish_to_index_with_manifest;
use crate::publish::Publisher;
use crate::store::{NativeRecord, Store};
use crate::wire::decode_publish_payload;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct ServeConfig {
    pub bind: String,
    pub token: Option<String>,
    pub auth_required: bool,
    pub state_dir: PathBuf,
    pub site_dir: PathBuf,
    pub public_base_url: String,
    pub limits: LimitsConfig,
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn verify_token(provided_header: Option<&str>, expected_token: &str) -> bool {
    let header = match provided_header {
        Some(h) => h.trim(),
        None => return false,
    };
    let provided_token = if let Some(stripped) = header.strip_prefix("Bearer ") {
        stripped.trim()
    } else {
        header
    };

    let mut h1 = Sha256::new();
    h1.update(provided_token.as_bytes());
    let d1 = h1.finalize();

    let mut h2 = Sha256::new();
    h2.update(expected_token.as_bytes());
    let d2 = h2.finalize();

    constant_time_eq(&d1, &d2)
}

fn send_json_response(mut stream: &TcpStream, status: u16, reason: &str, body_json: &str) {
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        body_json.len(),
        body_json
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn json_escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn send_error(stream: &TcpStream, status: u16, detail: &str) {
    let mut body = String::from("{\"errors\":[{\"detail\":\"");
    json_escape_into(&mut body, detail);
    body.push_str("\"}]}");
    let reason = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    send_json_response(stream, status, reason, &body);
}

pub fn handle_publish_request<R: Read>(
    stream: &TcpStream,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    reader: &mut R,
    cfg: &ServeConfig,
) -> Result<()> {
    if cfg.auth_required || cfg.token.is_some() {
        if let Some(ref exp_token) = cfg.token {
            let auth_hdr = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                .map(|(_, v)| v.as_str());
            if !verify_token(auth_hdr, exp_token) {
                send_error(stream, 401, "unauthorized");
                return Ok(());
            }
        } else if cfg.auth_required {
            send_error(stream, 401, "unauthorized");
            return Ok(());
        }
    }

    let store = Store::new(cfg.state_dir.clone());
    let publisher = Publisher::new(&store, cfg.site_dir.clone(), cfg.public_base_url.clone());

    if method == "PUT" && path == "/api/v1/crates/new" {
        if cfg.auth_required || cfg.token.is_some() {
            if let Some(ref exp_token) = cfg.token {
                let auth_hdr = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
                    .map(|(_, v)| v.as_str());
                if !verify_token(auth_hdr, exp_token) {
                    send_error(stream, 401, "unauthorized");
                    return Ok(());
                }
            } else {
                send_error(stream, 401, "unauthorized");
                return Ok(());
            }
        }
        let is_chunked = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
        });
        if is_chunked {
            send_error(stream, 411, "chunked transfer-encoding not supported");
            return Ok(());
        }

        let content_length = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<u64>().ok());

        let content_len = match content_length {
            Some(l) => l,
            None => {
                send_error(stream, 411, "content-length required");
                return Ok(());
            }
        };

        if content_len > cfg.limits.max_compressed + 10 * 1024 * 1024 {
            send_error(stream, 413, "payload exceeds size limit");
            return Ok(());
        }

        let staging_dir = store.blobs_dir();
        let decoded = match decode_publish_payload(
            reader,
            content_len,
            cfg.limits.max_compressed,
            &staging_dir,
        ) {
            Ok(d) => d,
            Err(Error::Limit(msg)) => {
                send_error(stream, 413, &msg);
                return Ok(());
            }
            Err(e) => {
                send_error(stream, 400, &e.to_string());
                return Ok(());
            }
        };

        let scan_limits = ScanLimits {
            max_compressed: cfg.limits.max_compressed,
            max_expanded: cfg.limits.max_expanded,
            max_entries: cfg.limits.max_entries,
            max_ratio: cfg.limits.max_ratio,
            max_manifest_bytes: 1024 * 1024,
        };

        let staged_file = match decoded.open() {
            Ok(f) => f,
            Err(e) => {
                send_error(stream, 400, &e.to_string());
                return Ok(());
            }
        };
        let scan_res = match scan_crate_staged(
            staged_file,
            decoded.crate_len,
            cfg.limits.max_compressed,
            Some(&decoded.crate_sha256),
            &decoded.body.name,
            &decoded.body.vers,
            &scan_limits,
        ) {
            Ok(s) => s,
            Err(Error::Limit(msg)) => {
                send_error(stream, 413, &msg);
                return Ok(());
            }
            Err(e) => {
                send_error(stream, 400, &e.to_string());
                return Ok(());
            }
        };

        let _guard = store.lock_exclusive()?;

        let blob_sha = store.ingest_staged_file(&decoded.crate_bytes_path, &decoded.crate_sha256)?;
        if blob_sha != scan_res.checksum {
            send_error(stream, 400, "checksum mismatch");
            return Ok(());
        }

        let mut body = decoded.body;
        body.checksum = blob_sha.clone();

        let packaged = parse_packaged_manifest(&scan_res.cargo_toml)?;
        let index_entry = match publish_to_index_with_manifest(&body, Some(&packaged)) {
            Ok(entry) => entry,
            Err(e) => {
                send_error(stream, 400, &e.to_string());
                return Ok(());
            }
        };

        let record = NativeRecord {
            name: body.name.clone(),
            vers: body.vers.clone(),
            checksum: blob_sha,
            yanked: body.yanked,
            entry: index_entry,
        };

        match store.put_native_record(&record) {
            Ok(_) => {}
            Err(Error::Conflict(msg)) => {
                send_error(stream, 409, &msg);
                return Ok(());
            }
            Err(e) => {
                send_error(stream, 400, &e.to_string());
                return Ok(());
            }
        }

        if let Err(e) = publisher.publish() {
            send_error(stream, 500, &e.to_string());
            return Ok(());
        }

        let resp_json = "{\"warnings\":{\"invalid_categories\":[],\"invalid_badges\":[],\"other\":[]}}";
        send_json_response(stream, 200, "OK", resp_json);
        return Ok(());
    }

    if (method == "DELETE" && path.ends_with("/yank"))
        || (method == "PUT" && path.ends_with("/unyank"))
    {
        let is_yank = method == "DELETE";
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        if parts.len() == 6 && parts[0] == "api" && parts[1] == "v1" && parts[2] == "crates" {
            let crate_name = parts[3];
            let crate_version = parts[4];

            let _guard = store.lock_exclusive()?;
            match store.set_native_yank(crate_name, crate_version, is_yank) {
                Ok(_) => {
                    if let Err(e) = publisher.publish() {
                        send_error(stream, 500, &e.to_string());
                        return Ok(());
                    }
                    send_json_response(stream, 200, "OK", "{\"ok\":true}");
                    return Ok(());
                }
                Err(Error::Input(msg)) => {
                    send_error(stream, 404, &msg);
                    return Ok(());
                }
                Err(e) => {
                    send_error(stream, 400, &e.to_string());
                    return Ok(());
                }
            }
        }
        send_error(stream, 404, "invalid route");
        return Ok(());
    }

    send_error(stream, 404, "not found");
    Ok(())
}

pub fn handle_connection(stream: TcpStream, cfg: &ServeConfig) -> Result<()> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut header_bytes = Vec::new();

    loop {
        let mut line = Vec::new();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            return Ok(());
        }
        if line.len() > 8192 + 2 {
            send_error(&stream, 400, "header line too long");
            return Ok(());
        }
        header_bytes.extend_from_slice(&line);
        if header_bytes.len() > 65536 + 4096 {
            send_error(&stream, 400, "headers too large");
            return Ok(());
        }
        if header_bytes.ends_with(b"\r\n\r\n") || header_bytes.ends_with(b"\n\n") {
            break;
        }
        if header_bytes.len() > 65536 {
            send_error(&stream, 400, "headers too large");
            return Ok(());
        }
    }

    let mut headers_buf = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers_buf);
    match req.parse(&header_bytes) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => {
            send_error(&stream, 400, "malformed request headers");
            return Ok(());
        }
    }

    let method = req.method.unwrap_or_default().to_string();
    let path = req.path.unwrap_or_default().to_string();
    let mut headers = Vec::new();
    for h in req.headers {
        if !h.name.is_empty() {
            let val = String::from_utf8_lossy(h.value).to_string();
            headers.push((h.name.to_string(), val));
        }
    }

    handle_publish_request(&stream, &method, &path, &headers, &mut reader, cfg)
}

struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn run_server(cfg: ServeConfig, stop: Arc<AtomicBool>) -> Result<()> {
    let listener = TcpListener::bind(&cfg.bind)?;
    listener.set_nonblocking(true)?;
    let active = Arc::new(AtomicUsize::new(0));

    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if active.load(Ordering::SeqCst) >= cfg.limits.concurrent_requests {
                    send_error(&stream, 503, "too many concurrent requests");
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let guard = ActiveGuard(active.clone());
                let cfg_clone = cfg.clone();
                std::thread::spawn(move || {
                    let _guard = guard;
                    let _ = handle_connection(stream, &cfg_clone);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(Error::Operation(e.to_string())),
        }
    }

    Ok(())
}
