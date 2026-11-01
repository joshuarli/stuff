use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::config::LimitsConfig;
use mycrates::serve::{handle_connection, run_server, ServeConfig};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

fn make_test_crate(name: &str, ver: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = format!("[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n", name, ver);
    let mut header = tar::Header::new_gnu();
    let path = format!("{}-{}/Cargo.toml", name, ver);
    header.set_path(&path).unwrap();
    header.set_size(cargo_toml.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, cargo_toml.as_bytes()).unwrap();
    let _ = tar.into_inner().unwrap();
    encoder.finish().unwrap()
}

use tar::Builder;

fn build_wire_payload(body_json: &str, crate_bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(body_json.len() as u32).to_le_bytes());
    payload.extend_from_slice(body_json.as_bytes());
    payload.extend_from_slice(&(crate_bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(crate_bytes);
    payload
}

fn body_json(name: &str, vers: &str) -> String {
    deser_json::to_string(&deser_value::value!({
        "name": name,
        "vers": vers,
        "deps": [],
        "features": {},
        "authors": [],
        "description": "test",
        "documentation": null,
        "homepage": null,
        "readme": null,
        "keywords": [],
        "categories": [],
        "license": "MIT",
        "license_file": null,
        "repository": null,
        "badges": {},
        "links": null
    }))
    .unwrap()
}

struct ProdServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for ProdServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn start_production_server(state_dir: PathBuf, site_dir: PathBuf, token: &str, auth_required: bool) -> ProdServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let base_url = format!("http://127.0.0.1:{}", port);
    let cfg = ServeConfig {
        bind: format!("127.0.0.1:{}", port),
        token: Some(token.to_string()),
        auth_required,
        state_dir,
        site_dir,
        public_base_url: base_url,
        limits: LimitsConfig::default(),
        max_connections: 64,
        max_writes: 4,
        header_timeout: Duration::from_secs(30),
        body_timeout: Duration::from_secs(3600),
    };
    let writes = Arc::new(AtomicUsize::new(0));
    let handle = thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        while !stop_clone.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let cfg = cfg.clone();
                    let writes = Arc::clone(&writes);
                    thread::spawn(move || {
                        let _ = handle_connection(stream, &cfg, &writes);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    ProdServer { port, stop, handle: Some(handle) }
}

fn start_run_server(
    state_dir: PathBuf,
    site_dir: PathBuf,
    token: &str,
    max_connections: usize,
    max_writes: usize,
) -> ProdServer {
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);
    let cfg = ServeConfig {
        bind: format!("127.0.0.1:{port}"),
        token: Some(token.to_string()),
        auth_required: false,
        state_dir,
        site_dir,
        public_base_url: format!("http://127.0.0.1:{port}"),
        limits: LimitsConfig::default(),
        max_connections,
        max_writes,
        header_timeout: Duration::from_secs(30),
        body_timeout: Duration::from_secs(3600),
    };
    let handle = thread::spawn(move || {
        let _ = run_server(cfg, stop_clone);
    });
    for _ in 0..100 {
        if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    ProdServer { port, stop, handle: Some(handle) }
}

fn send_request(port: u16, token: Option<&str>, payload: &[u8], split_at: Option<usize>) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let full = [req.as_bytes(), payload].concat();
    match split_at {
        None => {
            stream.write_all(&full).unwrap();
        }
        Some(at) => {
            let at = at.min(full.len());
            stream.write_all(&full[..at]).unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(20));
            stream.write_all(&full[at..]).unwrap();
        }
    }
    stream.flush().unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    resp
}

fn send_get(port: u16, token: Option<&str>, path: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Connection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).unwrap();
    let split = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(resp.len());
    let head = String::from_utf8_lossy(&resp[..split]);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, resp[split..].to_vec())
}

#[test]
fn idle_connections_do_not_block_authenticated_writes() {
    let dir = tempdir().unwrap();
    let server = start_run_server(
        dir.path().join("state"),
        dir.path().join("site"),
        "idle-token",
        8,
        1,
    );
    let idle: Vec<TcpStream> = (0..4)
        .map(|_| TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap())
        .collect();

    let payload = build_wire_payload(
        &body_json("idle-pkg", "1.0.0"),
        &make_test_crate("idle-pkg", "1.0.0"),
    );
    let resp = send_request(server.port, Some("idle-token"), &payload, None);
    assert!(resp.starts_with("HTTP/1.1 200"), "publish was blocked: {resp:?}");
    drop(idle);
}

#[test]
fn unauthenticated_requests_do_not_take_write_slots() {
    let dir = tempdir().unwrap();
    let server = start_run_server(
        dir.path().join("state"),
        dir.path().join("site"),
        "slot-token",
        8,
        1,
    );

    let mut slow = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    let slow_headers = "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer slot-token\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n";
    slow.write_all(slow_headers.as_bytes()).unwrap();
    slow.write_all(&1000u32.to_le_bytes()).unwrap();
    slow.write_all(&[b' '; 10]).unwrap();
    slow.flush().unwrap();
    thread::sleep(Duration::from_millis(200));

    let payload = build_wire_payload(
        &body_json("slot-pkg", "1.0.0"),
        &make_test_crate("slot-pkg", "1.0.0"),
    );
    let unauthorized = send_request(server.port, None, &payload, None);
    assert!(
        unauthorized.starts_with("HTTP/1.1 401"),
        "unauthorized write consumed a slot: {unauthorized:?}"
    );
    let blocked = send_request(server.port, Some("slot-token"), &payload, None);
    assert!(
        blocked.starts_with("HTTP/1.1 503"),
        "the slow write did not hold the only slot: {blocked:?}"
    );
    drop(slow);
}

#[test]
fn production_reader_single_write() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "prod-single-token";
    let server = start_production_server(state_dir, site_dir, token, true);

    let crate_bytes = make_test_crate("prod-single", "1.0.0");
    let payload = build_wire_payload(&body_json("prod-single", "1.0.0"), &crate_bytes);
    let resp = send_request(server.port, Some(token), &payload, None);
    assert!(resp.starts_with("HTTP/1.1 200"), "single-write publish failed: {resp}");
}

#[test]
fn production_reader_split_boundaries() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "prod-split-token";
    let server = start_production_server(state_dir.clone(), site_dir.clone(), token, true);

    for i in 0..4 {
        let name = format!("prod-split-{}", i);
        let crate_bytes = make_test_crate(&name, "1.0.0");
        let payload = build_wire_payload(&body_json(&name, "1.0.0"), &crate_bytes);
        let req_len = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            token,
            payload.len()
        )
        .len();
        let splits = [req_len - 2, req_len, req_len + 1, req_len + 4, req_len + 10];
        let at = splits[i % splits.len()];
        let resp = send_request(server.port, Some(token), &payload, Some(at));
        assert!(
            resp.starts_with("HTTP/1.1 200"),
            "split publish {name} at {at} failed: {resp}"
        );
    }
    let store = mycrates::store::Store::new(state_dir);
    let recs = store.load_native_records().unwrap();
    assert_eq!(recs.len(), 4);
}

#[test]
fn production_binary_end_to_end_publish() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    fs::create_dir_all(&state_dir).unwrap();
    let token = "prod-bin-token";
    let token_file = dir.path().join("token");
    fs::write(&token_file, token).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config_path = state_dir.join("config.json");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let public_base = format!("http://127.0.0.1:{}", port);
    let limits_val: deser_value::Value =
        deser_value::to_value(&mycrates::config::LimitsConfig::default()).unwrap();
    let cfg_val = deser_value::value!({
        "version": 1,
        "public_base_url": public_base,
        "upstream": {"index_url": "https://index.crates.io/"},
        "limits": limits_val,
        "auth_required": true
    });
    fs::create_dir_all(&state_dir).unwrap();
    let pretty = deser_json::SerializerConfig::builder()
        .pretty(deser_json::Indent::Spaces(2))
        .build();
    fs::write(&config_path, pretty.to_string(&cfg_val).unwrap()).unwrap();

    let bin = env!("CARGO_BIN_EXE_mycrates");
    let mut child = std::process::Command::new(bin)
        .args([
            "serve-publish",
            "--bind",
            &format!("127.0.0.1:{}", port),
            "--token-file",
            token_file.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--site-dir",
            site_dir.to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
        ])
        .spawn()
        .expect("spawn serve-publish");
    let mut ready = false;
    for _ in 0..100 {
        thread::sleep(Duration::from_millis(50));
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            ready = true;
            break;
        }
        if child.try_wait().unwrap().is_some() {
            break;
        }
    }
    assert!(ready, "serve-publish binary did not listen");
    // actual publish through production HTTP reader in one write
    let crate_bytes = make_test_crate("prod-bin-pkg", "0.2.0");
    let payload = build_wire_payload(&body_json("prod-bin-pkg", "0.2.0"), &crate_bytes);
    let resp = send_request(port, Some(token), &payload, None);
    assert!(resp.starts_with("HTTP/1.1 200"), "binary publish failed: {resp}");
    let store = mycrates::store::Store::new(state_dir);
    let recs = store.load_native_records().unwrap();
    assert!(recs.iter().any(|r| r.name == "prod-bin-pkg"));
    let _ = child.kill();
    let _ = child.wait();
}

use std::fs;

fn publish_test_crate(server: &ProdServer, token: &str, name: &str, version: &str) -> Vec<u8> {
    let crate_bytes = make_test_crate(name, version);
    let payload = build_wire_payload(&body_json(name, version), &crate_bytes);
    let resp = send_request(server.port, Some(token), &payload, None);
    assert!(resp.starts_with("HTTP/1.1 200"), "{name} publish failed: {resp}");
    crate_bytes
}

fn published_checksum(state_dir: &Path, name: &str) -> String {
    mycrates::store::Store::new(state_dir.to_path_buf())
        .load_native_records()
        .unwrap()
        .into_iter()
        .find(|r| r.name == name)
        .unwrap()
        .checksum
}

#[test]
fn production_reads_follow_auth_required_false() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "prod-open-token";
    let server = start_production_server(state_dir.clone(), site_dir, token, false);

    let crate_bytes = publish_test_crate(&server, token, "prod-open", "1.0.0");
    let path = format!("/blobs/{}.crate", published_checksum(&state_dir, "prod-open"));

    let (status, body) = send_get(server.port, None, &path);
    assert_eq!(status, 200, "unauthenticated blob GET should be open");
    assert_eq!(body, crate_bytes);

    let (status, body) = send_get(server.port, Some(token), &path);
    assert_eq!(status, 200);
    assert_eq!(body, crate_bytes);

    let payload = build_wire_payload(&body_json("prod-open-write", "1.0.0"), &make_test_crate("prod-open-write", "1.0.0"));
    let resp = send_request(server.port, None, &payload, None);
    assert!(resp.starts_with("HTTP/1.1 401"), "unauthenticated publish should fail: {resp}");
}

#[test]
fn production_reads_follow_auth_required_true() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "prod-private-token";
    let server = start_production_server(state_dir.clone(), site_dir, token, true);

    let crate_bytes = publish_test_crate(&server, token, "prod-private", "1.0.0");
    let path = format!("/blobs/{}.crate", published_checksum(&state_dir, "prod-private"));

    let (status, _) = send_get(server.port, None, &path);
    assert_eq!(status, 401, "unauthenticated blob GET should require the token");

    let (status, body) = send_get(server.port, Some(token), &path);
    assert_eq!(status, 200);
    assert_eq!(body, crate_bytes);
}
