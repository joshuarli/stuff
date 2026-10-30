use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::config::LimitsConfig;
use mycrates::serve::{handle_connection, ServeConfig};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

fn start_production_server(state_dir: PathBuf, site_dir: PathBuf, token: &str) -> ProdServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let base_url = format!("http://127.0.0.1:{}", port);
    let cfg = ServeConfig {
        bind: format!("127.0.0.1:{}", port),
        token: Some(token.to_string()),
        auth_required: true,
        state_dir,
        site_dir,
        public_base_url: base_url,
        limits: LimitsConfig::default(),
    };
    let handle = thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        while !stop_clone.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let cfg = cfg.clone();
                    thread::spawn(move || {
                        let _ = handle_connection(stream, &cfg);
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

fn send_request(port: u16, token: &str, payload: &[u8], split_at: Option<usize>) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).unwrap();
    let req = format!(
        "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        token,
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

#[test]
fn production_reader_single_write() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "prod-single-token";
    let server = start_production_server(state_dir, site_dir, token);

    let crate_bytes = make_test_crate("prod-single", "1.0.0");
    let payload = build_wire_payload(&body_json("prod-single", "1.0.0"), &crate_bytes);
    let resp = send_request(server.port, token, &payload, None);
    assert!(resp.starts_with("HTTP/1.1 200"), "single-write publish failed: {resp}");
}

#[test]
fn production_reader_split_boundaries() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "prod-split-token";
    let server = start_production_server(state_dir.clone(), site_dir.clone(), token);

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
        let resp = send_request(server.port, token, &payload, Some(at));
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
    let resp = send_request(port, token, &payload, None);
    assert!(resp.starts_with("HTTP/1.1 200"), "binary publish failed: {resp}");
    let store = mycrates::store::Store::new(state_dir);
    let recs = store.load_native_records().unwrap();
    assert!(recs.iter().any(|r| r.name == "prod-bin-pkg"));
    let _ = child.kill();
    let _ = child.wait();
}

use std::fs;
