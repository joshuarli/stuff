#[path = "support/cargo.rs"]
mod cargo;
#[path = "support/fixture.rs"]
mod fixture;

use cargo::{make_crate_project, CargoEnv};
use fixture::TestServer;
use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::publish::Publisher;
use mycrates::store::{Store, UpstreamRecord};
use std::fs;
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

#[test]
fn publish_unrenamed_public_dependency() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "tok-unrenamed";
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), true);
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
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), true);
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
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), true);
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
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", Some(token.to_string()), true);
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
