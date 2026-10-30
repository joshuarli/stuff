use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::acquire::{fetch, plan, verify};
use mycrates::config::{Config, UpstreamConfig};
use mycrates::error::Error;
use mycrates::pins::{write_pins, PinEntry, Pins};
use mycrates::store::Store;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use tar::Builder;
use tempfile::tempdir;

fn make_test_crate(name: &str, ver: &str) -> (Vec<u8>, String) {
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

    let _ = tar.into_inner().unwrap();
    let data = encoder.finish().unwrap();

    let mut h = Sha256::new();
    h.update(&data);
    let sha = format!("{:x}", h.finalize_reset());
    (data, sha)
}

struct MockUpstream {
    pub port: u16,
    pub stop: Arc<AtomicBool>,
}

impl MockUpstream {
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&str, &str, &str) -> (u16, Vec<u8>, &'static str) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let handler = Arc::new(handler);

        thread::spawn(move || {
            let _ = listener.set_nonblocking(true);
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let handler = handler.clone();
                        thread::spawn(move || {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                            let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
                            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                            let mut header_bytes = Vec::new();
                            loop {
                                let mut line = Vec::new();
                                if let Ok(n) = std::io::BufRead::read_until(&mut reader, b'\n', &mut line) {
                                    if n == 0 {
                                        break;
                                    }
                                    header_bytes.extend_from_slice(&line);
                                    if header_bytes.ends_with(b"\r\n\r\n") || header_bytes.ends_with(b"\n\n") {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            let req = String::from_utf8_lossy(&header_bytes);
                            let host = req
                                .lines()
                                .find(|l| l.to_lowercase().starts_with("host:"))
                                .map(|l| l[5..].trim())
                                .unwrap_or("127.0.0.1");
                            if let Some(req_line) = req.lines().next() {
                                let parts: Vec<&str> = req_line.split_whitespace().collect();
                                if parts.len() >= 2 {
                                    let method = parts[0];
                                    let path = parts[1];
                                    let (status, body, mime) = handler(method, path, host);
                                    let reason = if status == 200 { "OK" } else { "Error" };
                                    let resp = format!(
                                        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                        status, reason, mime, body.len()
                                    );
                                    let mut s = stream;
                                    let _ = s.write_all(resp.as_bytes());
                                    let _ = s.write_all(&body);
                                    let _ = s.flush();
                                }
                            }
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { port, stop }
    }
}

impl Drop for MockUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(format!("127.0.0.1:{}", self.port));
    }
}

#[test]
fn fetch_trust_chain_and_idempotence() {
    let (crate_bytes, real_sha) = make_test_crate("mock-pkg", "1.0.0");
    let crate_bytes_clone = crate_bytes.clone();
    let real_sha_clone = real_sha.clone();

    let upstream = MockUpstream::start(move |_method, path, host| {
        if path == "/config.json" {
            (
                200,
                format!("{{\"dl\":\"http://{}/{{crate}}/{{crate}}-{{version}}.crate\"}}", host).into_bytes(),
                "application/json",
            )
        } else if path == "/8/mo/mock-pkg" || path.ends_with("/mock-pkg") {
            let line = deser_json::to_string(&deser_value::value!({
                "name": "mock-pkg",
                "vers": "1.0.0",
                "deps": [],
                "cksum": real_sha_clone.clone(),
                "features": {},
                "yanked": false
            }))
            .unwrap();
            (200, format!("{line}\n").into_bytes(), "text/plain")
        } else if path.ends_with(".crate") {
            (200, crate_bytes_clone.clone(), "application/x-tar")
        } else {
            (404, b"not found".to_vec(), "text/plain")
        }
    });

    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let pins = Pins {
        registry: vec![PinEntry {
            name: "mock-pkg".to_string(),
            version: "1.0.0".to_string(),
            checksum: real_sha.clone(),
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
        ..Default::default()
    };
    write_pins(&store.pins_dir().join("default.toml"), &pins).unwrap();

    let plan_items = plan(&state_dir).unwrap();
    assert_eq!(plan_items.len(), 1);
    assert_eq!(plan_items[0].name, "mock-pkg");

    let cfg = Config {
        upstream: UpstreamConfig {
            index_url: format!("http://127.0.0.1:{}", upstream.port),
        },
        ..Default::default()
    };

    let summary = fetch(&state_dir, &cfg).unwrap();
    assert_eq!(summary.fetched_count, 1);
    assert_eq!(summary.total_bytes, crate_bytes.len() as u64);

    verify(&state_dir).unwrap();
    assert!(store.blobs_dir().join(format!("{}.crate", real_sha)).exists());
    assert!(store.upstream_dir().join("mock-pkg/1.0.0.json").exists());

    let plan_after = plan(&state_dir).unwrap();
    assert!(plan_after.is_empty());
    let summary2 = fetch(&state_dir, &cfg).unwrap();
    assert_eq!(summary2.fetched_count, 0);
}

#[test]
fn lock_index_checksum_mismatch() {
    let (_, real_sha) = make_test_crate("mismatch-pkg", "1.0.0");

    let upstream = MockUpstream::start(move |_method, path, host| {
        if path == "/config.json" {
            (
                200,
                format!("{{\"dl\":\"http://{}/{{crate}}/{{crate}}-{{version}}.crate\"}}", host).into_bytes(),
                "application/json",
            )
        } else if path.ends_with("/mismatch-pkg") {
            let line = deser_json::to_string(&deser_value::value!({
                "name": "mismatch-pkg",
                "vers": "1.0.0",
                "deps": [],
                "cksum": "different_index_checksum_value",
                "features": {},
                "yanked": false
            }))
            .unwrap();
            (200, format!("{line}\n").into_bytes(), "text/plain")
        } else {
            (404, b"not found".to_vec(), "text/plain")
        }
    });

    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let pins = Pins {
        registry: vec![PinEntry {
            name: "mismatch-pkg".to_string(),
            version: "1.0.0".to_string(),
            checksum: real_sha,
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
        ..Default::default()
    };
    write_pins(&store.pins_dir().join("default.toml"), &pins).unwrap();

    let mut cfg = Config::default();
    cfg.upstream.index_url = format!("http://127.0.0.1:{}", upstream.port);

    let err = fetch(&state_dir, &cfg).unwrap_err();
    match err {
        Error::Integrity(msg) => {
            assert!(msg.contains("trust chain violation"));
        }
        other => panic!("expected Integrity error, got {other:?}"),
    }

    assert_eq!(fs::read_dir(store.blobs_dir()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(store.upstream_dir()).unwrap().count(), 0);
}

#[test]
fn download_bytes_checksum_mismatch() {
    let (_, real_sha) = make_test_crate("bad-bytes", "1.0.0");
    let real_sha_clone = real_sha.clone();

    let upstream = MockUpstream::start(move |_method, path, host| {
        if path == "/config.json" {
            (
                200,
                format!("{{\"dl\":\"http://{}/{{crate}}/{{crate}}-{{version}}.crate\"}}", host).into_bytes(),
                "application/json",
            )
        } else if path.ends_with("/bad-bytes") {
            let line = deser_json::to_string(&deser_value::value!({
                "name": "bad-bytes",
                "vers": "1.0.0",
                "deps": [],
                "cksum": real_sha_clone.clone(),
                "features": {},
                "yanked": false
            }))
            .unwrap();
            (200, format!("{line}\n").into_bytes(), "text/plain")
        } else if path.ends_with(".crate") {
            (200, b"corrupted-tarball-bytes".to_vec(), "application/x-tar")
        } else {
            (404, b"not found".to_vec(), "text/plain")
        }
    });

    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let pins = Pins {
        registry: vec![PinEntry {
            name: "bad-bytes".to_string(),
            version: "1.0.0".to_string(),
            checksum: real_sha,
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
        ..Default::default()
    };
    write_pins(&store.pins_dir().join("default.toml"), &pins).unwrap();

    let mut cfg = Config::default();
    cfg.upstream.index_url = format!("http://127.0.0.1:{}", upstream.port);

    let err = fetch(&state_dir, &cfg).unwrap_err();
    match err {
        Error::Integrity(msg) => {
            assert!(msg.contains("checksum mismatch"));
        }
        other => panic!("expected Integrity error, got {other:?}"),
    }

    assert_eq!(fs::read_dir(store.blobs_dir()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(store.upstream_dir()).unwrap().count(), 0);
}

#[test]
fn missing_version_fails_closed() {
    let upstream = MockUpstream::start(|_method, path, host| {
        if path == "/config.json" {
            (
                200,
                format!("{{\"dl\":\"http://{}/{{crate}}/{{crate}}-{{version}}.crate\"}}", host).into_bytes(),
                "application/json",
            )
        } else if path.ends_with("/missing-ver") {
            let line = deser_json::to_string(&deser_value::value!({
                "name": "missing-ver",
                "vers": "1.0.1",
                "deps": [],
                "cksum": "some_cksum",
                "features": {},
                "yanked": false
            }))
            .unwrap();
            (200, format!("{line}\n").into_bytes(), "text/plain")
        } else {
            (404, b"not found".to_vec(), "text/plain")
        }
    });

    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let pins = Pins {
        registry: vec![PinEntry {
            name: "missing-ver".to_string(),
            version: "1.0.0".to_string(),
            checksum: "expected_sha_100".to_string(),
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
        ..Default::default()
    };
    write_pins(&store.pins_dir().join("default.toml"), &pins).unwrap();

    let mut cfg = Config::default();
    cfg.upstream.index_url = format!("http://127.0.0.1:{}", upstream.port);

    let err = fetch(&state_dir, &cfg).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("not found in upstream index"));

    assert_eq!(fs::read_dir(store.upstream_dir()).unwrap().count(), 0);
}

#[test]
fn fetch_budget_limit() {
    let (crate_bytes, real_sha) = make_test_crate("budget-pkg", "1.0.0");
    let crate_bytes_clone = crate_bytes.clone();
    let real_sha_clone = real_sha.clone();

    let upstream = MockUpstream::start(move |_method, path, host| {
        if path == "/config.json" {
            (
                200,
                format!("{{\"dl\":\"http://{}/{{crate}}/{{crate}}-{{version}}.crate\"}}", host).into_bytes(),
                "application/json",
            )
        } else if path.ends_with("/budget-pkg") {
            let line = deser_json::to_string(&deser_value::value!({
                "name": "budget-pkg",
                "vers": "1.0.0",
                "deps": [],
                "cksum": real_sha_clone.clone(),
                "features": {},
                "yanked": false
            }))
            .unwrap();
            (200, format!("{line}\n").into_bytes(), "text/plain")
        } else if path.ends_with(".crate") {
            (200, crate_bytes_clone.clone(), "application/x-tar")
        } else {
            (404, b"not found".to_vec(), "text/plain")
        }
    });

    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let pins = Pins {
        registry: vec![PinEntry {
            name: "budget-pkg".to_string(),
            version: "1.0.0".to_string(),
            checksum: real_sha,
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
        ..Default::default()
    };
    write_pins(&store.pins_dir().join("default.toml"), &pins).unwrap();

    let mut cfg = Config::default();
    cfg.upstream.index_url = format!("http://127.0.0.1:{}", upstream.port);
    cfg.limits.fetch_bytes = 10;

    let err = fetch(&state_dir, &cfg).unwrap_err();
    match err {
        Error::Limit(msg) => {
            assert!(msg.contains("budget"));
        }
        other => panic!("expected Limit error, got {other:?}"),
    }
}
