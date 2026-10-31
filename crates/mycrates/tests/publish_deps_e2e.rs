#[path = "support/cargo.rs"]
mod cargo;
#[path = "support/fixture.rs"]
mod fixture;

use cargo::{make_crate_project, CargoEnv};
use fixture::TestServer;
use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::config::LimitsConfig;
use mycrates::publish::Publisher;
use mycrates::serve::{run_server, ServeConfig};
use mycrates::store::{Store, UpstreamRecord};
use std::fs;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tar::Builder;
use tempfile::tempdir;

fn make_upstream_bytes(name: &str, ver: &str) -> (Vec<u8>, String) {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = format!("[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n", name, ver);
    let mut header = tar::Header::new_gnu();
    header.set_path(format!("{}-{}/Cargo.toml", name, ver)).unwrap();
    header.set_size(cargo_toml.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, cargo_toml.as_bytes()).unwrap();
    let code = "pub fn f() {}\n";
    let mut ch = tar::Header::new_gnu();
    ch.set_path(format!("{}-{}/src/lib.rs", name, ver)).unwrap();
    ch.set_size(code.len() as u64);
    ch.set_mode(0o644);
    ch.set_cksum();
    tar.append(&ch, code.as_bytes()).unwrap();
    let _ = tar.into_inner().unwrap();
    let data = encoder.finish().unwrap();
    let sha = format!("{:x}", {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&data);
        h.finalize()
    });
    (data, sha)
}

fn seed(store: &Store, name: &str, vers: &str) {
    let (bytes, sha) = make_upstream_bytes(name, vers);
    store.ingest_blob(&bytes[..]).unwrap();
    let line = deser_json::to_string(&deser_value::value!({
        "name": name, "vers": vers, "deps": [], "cksum": sha.clone(), "features": {}, "yanked": false
    }))
    .unwrap();
    store
        .put_upstream_record(&UpstreamRecord {
            name: name.to_string(),
            vers: vers.to_string(),
            checksum: sha,
            raw_index_line: line,
            acquired_at: 1000,
        })
        .unwrap();
}

struct PublishServer {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for PublishServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl PublishServer {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

fn start_publish_server(state_dir: std::path::PathBuf, site_dir: std::path::PathBuf, token: &str) -> PublishServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let cfg = ServeConfig {
        bind: format!("127.0.0.1:{port}"),
        token: Some(token.to_string()),
        auth_required: false,
        state_dir,
        site_dir,
        public_base_url: format!("http://127.0.0.1:{port}"),
        limits: LimitsConfig::default(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = thread::spawn(move || {
        let _ = run_server(cfg, stop_clone);
    });
    for _ in 0..100 {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    PublishServer { port, stop, handle: Some(handle) }
}

#[test]
fn publish_unrenamed_public_dependency() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "tok-unrenamed";
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), false);
    let store = Store::new(state_dir.clone());
    seed(&store, "public-dep", "1.0.0");
    Publisher::new(&store, site_dir.clone(), server.base_url()).publish().unwrap();

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(cargo_home.clone(), server.registry_index_url(), Some(token.to_string()));
    fs::write(
        cargo_home.join("config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"mycrates-mirror\"\n\n[source.mycrates-mirror]\nregistry = \"sparse+{}\"\n",
            server.mirror_index_url()
        ),
    )
    .unwrap();

    let pkg = dir.path().join("pkg");
    make_crate_project(
        &pkg,
        "uses-public",
        "0.1.0",
        "[dependencies]\npublic-dep = \"1.0\"\n",
        "pub fn f() { public_dep::f(); }",
    );
    let out = c_env.run(&pkg, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success(), "unrenamed publish failed: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn publish_versioned_dev_dependency() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "tok-dev";
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), false);
    let store = Store::new(state_dir.clone());
    seed(&store, "dev-dep", "1.0.0");
    Publisher::new(&store, site_dir.clone(), server.base_url()).publish().unwrap();

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(cargo_home.clone(), server.registry_index_url(), Some(token.to_string()));
    fs::write(
        cargo_home.join("config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"mycrates-mirror\"\n\n[source.mycrates-mirror]\nregistry = \"sparse+{}\"\n",
            server.mirror_index_url()
        ),
    )
    .unwrap();

    let pkg = dir.path().join("pkg");
    make_crate_project(
        &pkg,
        "uses-dev",
        "0.1.0",
        "[dev-dependencies]\ndev-dep = \"1.0\"\n",
        "#[cfg(test)] mod t { #[test] fn it() { dev_dep::f(); } } pub fn f() {}",
    );
    let out = c_env.run(&pkg, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success(), "dev-dep publish failed: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn publish_same_crate_runtime_and_build() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "tok-rb";
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), false);
    let store = Store::new(state_dir.clone());
    seed(&store, "shared-dep", "1.0.0");
    Publisher::new(&store, site_dir.clone(), server.base_url()).publish().unwrap();

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(cargo_home.clone(), server.registry_index_url(), Some(token.to_string()));
    fs::write(
        cargo_home.join("config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"mycrates-mirror\"\n\n[source.mycrates-mirror]\nregistry = \"sparse+{}\"\n",
            server.mirror_index_url()
        ),
    )
    .unwrap();

    let pkg = dir.path().join("pkg");
    make_crate_project(
        &pkg,
        "uses-rb",
        "0.1.0",
        "[dependencies]\nshared-dep = \"1.0\"\n\n[build-dependencies]\nshared-dep = \"1.0\"\n",
        "pub fn f() { shared_dep::f(); }",
    );
    fs::write(pkg.join("build.rs"), "fn main() {}").unwrap();
    let out = c_env.run(&pkg, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success(), "runtime+build publish failed: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn publish_same_alias_two_targets() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "tok-targ";
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), false);
    let store = Store::new(state_dir.clone());
    seed(&store, "targ-dep", "1.0.0");
    Publisher::new(&store, site_dir.clone(), server.base_url()).publish().unwrap();

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(cargo_home.clone(), server.registry_index_url(), Some(token.to_string()));
    fs::write(
        cargo_home.join("config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"mycrates-mirror\"\n\n[source.mycrates-mirror]\nregistry = \"sparse+{}\"\n",
            server.mirror_index_url()
        ),
    )
    .unwrap();

    let pkg = dir.path().join("pkg");
    make_crate_project(
        &pkg,
        "uses-targ",
        "0.1.0",
        "[target.'cfg(unix)'.dependencies]\ntarg-dep = \"1.0\"\n\n[target.'cfg(windows)'.dependencies]\ntarg-dep = \"1.0\"\n",
        "pub fn f() {}",
    );
    let out = c_env.run(&pkg, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success(), "two-target publish failed: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn publish_native_dependency_without_no_verify() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "tok-native-dep";
    let index = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), false);
    let publish = start_publish_server(state_dir.clone(), site_dir.clone(), token);

    let store = Store::new(state_dir.clone());
    Publisher::new(&store, site_dir.clone(), publish.base_url())
        .with_auth_required(false)
        .publish()
        .unwrap();

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(cargo_home, index.registry_index_url(), Some(token.to_string()));

    let child = dir.path().join("native-child");
    make_crate_project(&child, "native-child", "0.1.0", "", "pub fn f() {}");
    let out = c_env.run(&child, &["publish", "--registry", "mycrates"]);
    assert!(
        out.status.success(),
        "native dependency publish failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let parent = dir.path().join("native-parent");
    make_crate_project(
        &parent,
        "native-parent",
        "0.1.0",
        "[dependencies]\nnative-child = { version = \"0.1.0\", registry = \"mycrates\" }\n",
        "pub fn f() { native_child::f(); }",
    );
    let out = c_env.run(&parent, &["publish", "--registry", "mycrates"]);
    assert!(
        out.status.success(),
        "dependent native publish failed without --no-verify: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let consumer = dir.path().join("native-consumer");
    make_crate_project(
        &consumer,
        "native-consumer",
        "0.1.0",
        "[dependencies]\nnative-parent = { version = \"0.1.0\", registry = \"mycrates\" }\n",
        "pub fn f() { native_parent::f(); }",
    );
    let out = c_env.run(&consumer, &["check"]);
    assert!(
        out.status.success(),
        "consumer could not build native dependencies: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
