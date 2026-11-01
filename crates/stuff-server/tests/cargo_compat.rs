use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::model::IndexEntry;
use mycrates::publish::Publisher;
use mycrates::store::{NativeRecord, Store};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use tar::Builder;
use tempfile::TempDir;

fn make_test_crate(name: &str, ver: &str, code: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = format!(
        "[package]\nname = \"{name}\"\nversion = \"{ver}\"\nedition = \"2021\"\n"
    );
    let mut header = tar::Header::new_gnu();
    let path = format!("{name}-{ver}/Cargo.toml");
    header.set_path(&path).unwrap();
    header.set_size(cargo_toml.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, cargo_toml.as_bytes()).unwrap();
    let code_path = format!("{name}-{ver}/src/lib.rs");
    let mut code_header = tar::Header::new_gnu();
    code_header.set_path(&code_path).unwrap();
    code_header.set_size(code.len() as u64);
    code_header.set_mode(0o644);
    code_header.set_cksum();
    tar.append(&code_header, code.as_bytes()).unwrap();
    let _ = tar.into_inner().unwrap();
    encoder.finish().unwrap()
}

struct TestServer {
    child: Child,
    base_url: String,
}

impl TestServer {
    fn boot(site_dir: &std::path::Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_stuff-server");
        let mut child = Command::new(bin)
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--route")
            .arg(format!("/={}", site_dir.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn stuff-server");
        let stdout = child.stdout.take().expect("server stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("server banner");
        let addr = line
            .trim()
            .strip_prefix("listening on ")
            .expect("banner format")
            .to_string();
        drop(reader);
        Self {
            child,
            base_url: format!("http://{addr}"),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn cargo_installs_from_stuff_server() {
    let dir = TempDir::new().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    fs::create_dir_all(&site_dir).unwrap();

    let server = TestServer::boot(&site_dir);

    let store = Store::new(state_dir.clone());
    let crate_bytes = make_test_crate("alpha", "0.1.0", "pub fn hello() -> &'static str { \"hello from alpha\" }");
    let sha = store.ingest_blob(&crate_bytes[..]).unwrap();
    let rec = NativeRecord {
        entry: IndexEntry {
            name: "alpha".to_string(),
            vers: "0.1.0".to_string(),
            deps: vec![],
            cksum: sha,
            features: BTreeMap::new(),
            yanked: false,
            links: None,
            rust_version: None,
            v: Some(1),
            extra: BTreeMap::new(),
        },
    };
    store.put_native_record(&rec).unwrap();
    Publisher::new(&store, site_dir.clone(), server.base_url.clone())
        .publish()
        .unwrap();

    let cargo_home = dir.path().join("cargo_home");
    fs::create_dir_all(&cargo_home).unwrap();
    let consumer_dir = dir.path().join("consumer");
    fs::create_dir_all(consumer_dir.join("src")).unwrap();
    fs::write(
        consumer_dir.join("Cargo.toml"),
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nalpha = { version = \"0.1.0\", registry = \"mycrates\" }\n",
    )
    .unwrap();
    fs::write(
        consumer_dir.join("src/main.rs"),
        "fn main() { println!(\"{}\", alpha::hello()); }",
    )
    .unwrap();

    let out = Command::new("cargo")
        .current_dir(&consumer_dir)
        .arg("run")
        .env("CARGO_HOME", &cargo_home)
        .env(
            "CARGO_REGISTRIES_MYCRATES_INDEX",
            format!("sparse+{}/current/registry/", server.base_url),
        )
        .env_remove("CARGO_REGISTRIES_MYCRATES_TOKEN")
        .env("CARGO_TERM_COLOR", "never")
        .env("CARGO_HTTP_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .output()
        .expect("run cargo");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "cargo run failed: stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("hello from alpha"));
}
