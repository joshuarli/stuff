#[path = "support/fixture.rs"]
mod fixture;

use fixture::TestServer;
use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::config::LimitsConfig;
use mycrates::error::Error;
use mycrates::publish::Publisher;
use mycrates::serve::{handle_connection, try_admit, ServeConfig};
use mycrates::store::Store;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use tar::Builder;
use tempfile::tempdir;

fn make_test_crate(name: &str, ver: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = format!(
        "[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n",
        name, ver
    );
    let mut header = tar::Header::new_gnu();
    let path = format!("{}-{}/Cargo.toml", name, ver);
    header.set_path(&path).unwrap();
    header.set_size(cargo_toml.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, cargo_toml.as_bytes()).unwrap();

    let code_path = format!("{}-{}/src/lib.rs", name, ver);
    let mut code_header = tar::Header::new_gnu();
    code_header.set_path(&code_path).unwrap();
    code_header.set_size(0);
    code_header.set_mode(0o644);
    code_header.set_cksum();
    tar.append(&code_header, &[][..]).unwrap();

    let _ = tar.into_inner().unwrap();
    encoder.finish().unwrap()
}

fn build_wire_payload(body_json: &str, crate_bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    let json_bytes = body_json.as_bytes();
    let json_len = json_bytes.len() as u32;
    payload.extend_from_slice(&json_len.to_le_bytes());
    payload.extend_from_slice(json_bytes);

    let crate_len = crate_bytes.len() as u32;
    payload.extend_from_slice(&crate_len.to_le_bytes());
    payload.extend_from_slice(crate_bytes);
    payload
}

struct RawServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn start_raw_server(
    state_dir: PathBuf,
    site_dir: PathBuf,
    token: Option<String>,
    auth_required: bool,
) -> RawServer {
    start_raw_server_with_timeouts(
        state_dir,
        site_dir,
        token,
        auth_required,
        Duration::from_secs(30),
        Duration::from_secs(3600),
    )
}

fn start_raw_server_with_timeouts(
    state_dir: PathBuf,
    site_dir: PathBuf,
    token: Option<String>,
    auth_required: bool,
    header_timeout: Duration,
    body_timeout: Duration,
) -> RawServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);
    let cfg = ServeConfig {
        bind: format!("127.0.0.1:{port}"),
        token,
        auth_required,
        state_dir,
        site_dir,
        public_base_url: format!("http://127.0.0.1:{port}"),
        limits: LimitsConfig::default(),
        max_connections: 64,
        max_writes: 4,
        header_timeout,
        body_timeout,
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
    RawServer {
        port,
        stop,
        handle: Some(handle),
    }
}

#[test]
fn overlong_header_line_without_newline_is_rejected_early() {
    let dir = tempdir().unwrap();
    let server = start_raw_server(
        dir.path().join("state"),
        dir.path().join("site"),
        None,
        false,
    );
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut raw = b"GET / HTTP/1.1\r\nHost: x\r\nX-Probe: ".to_vec();
    raw.extend(std::iter::repeat_n(b'a', 8195 - b"X-Probe: ".len()));
    stream.write_all(&raw).unwrap();

    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(
        resp.starts_with("HTTP/1.1 400"),
        "expected an early 400, got {resp:?}"
    );
}

#[test]
fn header_trickle_exceeds_the_header_deadline() {
    let dir = tempdir().unwrap();
    let server = start_raw_server_with_timeouts(
        dir.path().join("state"),
        dir.path().join("site"),
        None,
        false,
        Duration::from_secs(1),
        Duration::from_secs(5),
    );
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nX-Trickle: ")
        .unwrap();
    for _ in 0..3 {
        thread::sleep(Duration::from_millis(100));
        stream.write_all(b"a").unwrap();
    }
    thread::sleep(Duration::from_millis(1100));

    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(
        resp.starts_with("HTTP/1.1 408"),
        "expected a 408 after the header deadline, got {resp:?}"
    );
}

#[test]
fn body_delivered_after_the_header_deadline_is_accepted() {
    let dir = tempdir().unwrap();
    let token = "slow-body-token";
    let server = start_raw_server_with_timeouts(
        dir.path().join("state"),
        dir.path().join("site"),
        Some(token.to_string()),
        true,
        Duration::from_millis(200),
        Duration::from_secs(2),
    );
    let crate_bytes = make_test_crate("slow-body", "1.0.0");
    let json = deser_json::to_string(&deser_value::value!({
        "name": "slow-body",
        "vers": "1.0.0",
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
    .unwrap();
    let payload = build_wire_payload(&json, &crate_bytes);
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = format!(
        "PUT /api/v1/crates/new HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(500));
    stream.write_all(&payload).unwrap();

    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(
        resp.starts_with("HTTP/1.1 200"),
        "delayed body should use the body deadline, got {resp:?}"
    );
}

#[test]
fn body_trickle_exceeds_the_body_deadline() {
    let dir = tempdir().unwrap();
    let token = "slow-body-timeout-token";
    let server = start_raw_server_with_timeouts(
        dir.path().join("state"),
        dir.path().join("site"),
        Some(token.to_string()),
        true,
        Duration::from_secs(1),
        Duration::from_millis(300),
    );
    let crate_bytes = make_test_crate("slow-body-timeout", "1.0.0");
    let json = deser_json::to_string(&deser_value::value!({
        "name": "slow-body-timeout",
        "vers": "1.0.0",
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
    .unwrap();
    let payload = build_wire_payload(&json, &crate_bytes);
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = format!(
        "PUT /api/v1/crates/new HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(&payload[..8]).unwrap();

    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(
        !resp.starts_with("HTTP/1.1 200"),
        "trickled body should hit the body deadline, got {resp:?}"
    );
}

#[test]
fn blob_get_streams_exact_bytes() {
    let dir = tempdir().unwrap();
    let site_dir = dir.path().join("site");
    let checksum = "a".repeat(64);
    let blob: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    fs::create_dir_all(site_dir.join("blobs")).unwrap();
    fs::write(
        site_dir.join("blobs").join(format!("{checksum}.crate")),
        &blob,
    )
    .unwrap();
    let server = start_raw_server(dir.path().join("state"), site_dir, None, false);

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let request = format!("GET /blobs/{checksum}.crate HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response headers")
        + 4;
    let head = String::from_utf8_lossy(&raw[..split]);
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
    assert!(head.contains(&format!("Content-Length: {}", blob.len())), "{head}");
    assert_eq!(&raw[split..], blob.as_slice());
}

#[test]
fn concurrent_admission_never_exceeds_cap() {
    const LIMIT: usize = 4;
    const WORKERS: usize = 16;
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut handles = Vec::new();
    for _ in 0..WORKERS {
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for _ in 0..2000 {
                if let Some(guard) = try_admit(&active, LIMIT) {
                    peak.fetch_max(active.load(Ordering::SeqCst), Ordering::SeqCst);
                    drop(guard);
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert!(peak.load(Ordering::SeqCst) <= LIMIT);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[test]
fn corrupted_stored_blob_after_publish() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");

    let token = "hardening-token";
    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some(token.to_string()),
        false,
    );
    let crate_bytes = make_test_crate("victim-pkg", "1.0.0");
    let json = deser_json::to_string(&deser_value::value!({
        "name": "victim-pkg",
        "vers": "1.0.0",
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
    .unwrap();

    let payload = build_wire_payload(&json, &crate_bytes);
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    let req = format!(
        "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.write_all(&payload).unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200 OK"));

    let store = Store::new(state_dir.clone());
    let recs = store.load_native_records().unwrap();
    assert_eq!(recs.len(), 1);
    let blob_path = store.blobs_dir().join(format!("{}.crate", recs[0].checksum));

    // Corrupt the blob in state store
    fs::write(&blob_path, b"corrupted-blob-bytes").unwrap();

    // Store verification fails
    let err = store.verify().unwrap_err();
    match err {
        Error::Integrity(msg) => {
            assert!(msg.contains("corrupted blob"));
        }
        other => panic!("expected Integrity, got {other:?}"),
    }

    // Subsequent publish attempt fails before exposing
    let publ = Publisher::new(&store, site_dir.clone(), server.base_url());
    let pub_err = publ.publish().unwrap_err();
    assert!(format!("{pub_err}").contains("tampered blob"));
}

#[test]
fn stale_or_broken_generation_under_current() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");

    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();
    fs::create_dir_all(&site_dir).unwrap();

    // Point `current` to a broken/dangling target
    let current_link = site_dir.join("current");
    std::os::unix::fs::symlink("generations/nonexistent_gen_12345", &current_link).unwrap();
    assert!(current_link.is_symlink());

    // Publisher runs and recovers `current` to point to a valid generation
    let publ = Publisher::new(&store, site_dir.clone(), "http://localhost:8080".to_string());
    let res = publ.publish().unwrap();
    assert!(!res.is_noop);

    let target = fs::read_link(&current_link).unwrap();
    assert_eq!(target.to_string_lossy(), format!("generations/{}", res.generation_id));
    assert!(site_dir.join(&target).exists());
}

#[test]
fn concurrent_writers_over_http() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "multi-writer-token";

    let server = Arc::new(TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some(token.to_string()),
        true,
    ));

    let mut handles = Vec::new();
    let num_writers = 6;

    for i in 0..num_writers {
        let srv = server.clone();
        let tok = token.to_string();
        let handle = thread::spawn(move || {
            let name = format!("pkg-conc-{}", i);
            let crate_bytes = make_test_crate(&name, "1.0.0");
            let json = deser_json::to_string(&deser_value::value!({
                "name": name,
                "vers": "1.0.0",
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
            .unwrap();

            let payload = build_wire_payload(&json, &crate_bytes);
            let mut stream = TcpStream::connect(format!("127.0.0.1:{}", srv.port)).unwrap();
            let req = format!(
                "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                tok,
                payload.len()
            );
            stream.write_all(req.as_bytes()).unwrap();
            stream.write_all(&payload).unwrap();

            let mut resp = String::new();
            stream.read_to_string(&mut resp).unwrap();
            assert!(resp.starts_with("HTTP/1.1 200 OK"), "upload failed: {resp}");
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }

    let store = Store::new(state_dir);
    let recs = store.load_native_records().unwrap();
    assert_eq!(recs.len(), num_writers);

    // Current generation must contain all published crates
    for i in 0..num_writers {
        let name = format!("pkg-conc-{}", i);
        let rec = recs.iter().find(|r| r.name == name);
        assert!(rec.is_some(), "missing record for {name}");
    }
}

#[test]
fn malformed_upload_corpus() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");

    let token = "malformed-token";
    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some(token.to_string()),
        false,
    );
    // 1. JSON length 0 -> 400
    {
        let mut wire = Vec::new();
        wire.extend_from_slice(&0u32.to_le_bytes());
        wire.extend_from_slice(&10u32.to_le_bytes());
        wire.extend_from_slice(&[0u8; 10]);

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 400"), "expected 400, got {resp:?}");
        assert!(resp.contains("empty json"));
    }

    // 2. Corrupt gzip payload in crate tarball -> 422
    {
        let json = deser_json::to_string(&deser_value::value!({
            "name": "corrupt-gz",
            "vers": "1.0.0",
            "deps": [],
            "features": {},
            "authors": []
        }))
        .unwrap();
        let bad_crate = b"not-a-valid-gzip-stream-at-all";
        let wire = build_wire_payload(&json, bad_crate);

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 422"), "expected 422, got {resp:?}");
    }

    // 3. Name mismatch between JSON metadata and Cargo.toml -> 422
    {
        let crate_bytes = make_test_crate("real-name", "1.0.0");
        let json = deser_json::to_string(&deser_value::value!({
            "name": "different-name",
            "vers": "1.0.0",
            "deps": [],
            "features": {},
            "authors": []
        }))
        .unwrap();
        let wire = build_wire_payload(&json, &crate_bytes);

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 422"), "expected 422, got {resp:?}");
        assert!(resp.contains("wrong prefix") || resp.contains("name/version mismatch"));
    }
}
