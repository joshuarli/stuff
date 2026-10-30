#[path = "support/fixture.rs"]
mod fixture;

use fixture::TestServer;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
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

    let code_path = format!("{}-{}/src/lib.rs", name, ver);
    let mut code_header = tar::Header::new_gnu();
    code_header.set_path(&code_path).unwrap();
    code_header.set_size(0);
    code_header.set_mode(0o644);
    code_header.set_cksum();
    tar.append(&code_header, &[][..]).unwrap();

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

fn extract_sh_verified_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut current = Vec::new();

    for line in markdown.lines() {
        if line.trim() == "```sh verified" {
            in_block = true;
            current.clear();
            continue;
        }
        if in_block {
            if line.trim() == "```" {
                in_block = false;
                blocks.push(current.join("\n"));
                current.clear();
            } else {
                current.push(line);
            }
        }
    }

    blocks
}

#[test]
fn execute_readme_verified_blocks() {
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let readme_path = root_dir.join("README.md");
    let readme_text = fs::read_to_string(&readme_path).expect("read README.md");
    let blocks = extract_sh_verified_blocks(&readme_text);
    assert!(!blocks.is_empty(), "found no `sh verified` blocks in README.md");

    // Build binary first
    let build_out = Command::new("cargo")
        .args(["build", "--bin", "mycrates"])
        .current_dir(&root_dir)
        .output()
        .expect("build mycrates binary");
    assert!(build_out.status.success(), "cargo build mycrates failed");

    let target_dir = std::env::var("CARGO_TARGET_DIR").map(PathBuf::from);
    let bin_path = match target_dir {
        Ok(dir) => dir.join("debug/mycrates"),
        Err(_) => root_dir
            .ancestors()
            .map(|a| a.join("target/debug/mycrates"))
            .find(|p| p.is_file())
            .expect("built mycrates binary"),
    };

    // Setup fixture environment
    let work_dir = tempdir().unwrap();
    let (crate_bytes, cksum) = make_test_crate("sample-pkg", "0.1.0");
    let crate_bytes_clone = crate_bytes.clone();
    let cksum_clone = cksum.clone();

    let upstream = MockUpstream::start(move |_method, path, host| {
        if path == "/config.json" {
            (
                200,
                format!("{{\"dl\":\"http://{}/{{crate}}/{{crate}}-{{version}}.crate\"}}", host).into_bytes(),
                "application/json",
            )
        } else if path.ends_with("/sample-pkg") {
            let line = deser_json::to_string(&deser_value::value!({
                "name": "sample-pkg",
                "vers": "0.1.0",
                "deps": [],
                "cksum": cksum_clone.clone(),
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

    // Write a sample Cargo.lock in work_dir
    let lock_content = format!(
        r#"version = 3

[[package]]
name = "sample-pkg"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{}"

[[package]]
name = "validation-probe"
version = "0.1.0"
dependencies = [
 "sample-pkg",
]
"#,
        cksum
    );
    fs::write(work_dir.path().join("Cargo.lock"), lock_content).unwrap();
    fs::write(
        work_dir.path().join("Cargo.toml"),
        "[package]\nname = \"validation-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nsample-pkg = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(work_dir.path().join("src")).unwrap();
    fs::write(work_dir.path().join("src/lib.rs"), "").unwrap();

    let bin_dir = work_dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::copy(&bin_path, bin_dir.join("mycrates")).unwrap();

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut _site_server: Option<TestServer> = None;

    // Execute each verified block
    for block in blocks {
        for raw_line in block.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(line);
            cmd.current_dir(work_dir.path());
            cmd.env("PATH", &path_env);

            let out = cmd.output().unwrap_or_else(|e| panic!("failed command `{line}`: {e}"));
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "README command failed: `{line}`\nstdout: {stdout}\nstderr: {stderr}"
            );

            // If init was run, configure upstream to loopback and start site server
            if line == "mycrates init" {
                let state_dir = work_dir.path().join(".work/state");
                let site_dir = work_dir.path().join(".work/site");
                let s_server = TestServer::start(state_dir.clone(), site_dir.clone(), "", None, false);
                let site_url = s_server.base_url();
                _site_server = Some(s_server);

                let cfg_file = state_dir.join("config.json");
                let mut cfg: deser_value::Value =
                    deser_json::from_str(&fs::read_to_string(&cfg_file).unwrap()).unwrap();
                cfg["upstream"]["index_url"] =
                    deser_value::value!((format!("http://127.0.0.1:{}", upstream.port)));
                cfg["public_base_url"] = deser_value::value!(site_url);
                let pretty = deser_json::SerializerConfig::builder()
                    .pretty(deser_json::Indent::Spaces(2))
                    .build();
                fs::write(&cfg_file, pretty.to_string(&cfg).unwrap()).unwrap();
            }
        }
    }
}
