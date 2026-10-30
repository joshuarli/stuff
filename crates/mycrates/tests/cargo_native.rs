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
use std::io::{Read, Write};
use std::net::TcpStream;
use tar::Builder;
use tempfile::tempdir;

fn make_test_crate(name: &str, ver: &str, code: &str) -> Vec<u8> {
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
    code_header.set_size(code.len() as u64);
    code_header.set_mode(0o644);
    code_header.set_cksum();
    tar.append(&code_header, code.as_bytes()).unwrap();

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

#[test]
fn publish_and_consume() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "test-secret-token";

    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some(token.to_string()),
        true,
    );

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(
        cargo_home,
        server.registry_index_url(),
        Some(token.to_string()),
    );

    // 1. Author and publish `alpha` 0.1.0
    let alpha_dir = dir.path().join("alpha");
    make_crate_project(
        &alpha_dir,
        "alpha",
        "0.1.0",
        "",
        "pub fn hello() -> &'static str { \"hello from alpha\" }",
    );

    let out = c_env.run(&alpha_dir, &["publish", "--registry", "mycrates"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "cargo publish failed: stdout={stdout}\nstderr={stderr}"
    );

    // 2. Author consumer project
    let consumer_dir = dir.path().join("consumer");
    fs::create_dir_all(consumer_dir.join("src")).unwrap();
    let consumer_toml =
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nalpha = { version = \"0.1.0\", registry = \"mycrates\" }\n";
    fs::write(consumer_dir.join("Cargo.toml"), consumer_toml).unwrap();
    fs::write(
        consumer_dir.join("src/main.rs"),
        "fn main() { println!(\"{}\", alpha::hello()); }",
    )
    .unwrap();

    let out2 = c_env.run(&consumer_dir, &["run"]);
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        out2.status.success(),
        "cargo run failed: stdout={stdout2}\nstderr={stderr2}"
    );
    assert!(stdout2.contains("hello from alpha"));
}

#[test]
fn cross_registry() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "token-cross";

    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some(token.to_string()),
        true,
    );

    // Setup an upstream crate in the mirror view: `mock-upstream` 0.1.0
    let store = Store::new(state_dir.clone());
    let upstream_bytes = make_test_crate(
        "mock-upstream",
        "0.1.0",
        "pub fn upstream_fn() -> &'static str { \"upstream works\" }",
    );
    let up_sha = store.ingest_blob(&upstream_bytes[..]).unwrap();
    let up_index_line = deser_json::to_string(&deser_value::value!({
        "name": "mock-upstream",
        "vers": "0.1.0",
        "deps": [],
        "cksum": up_sha.clone(),
        "features": {},
        "yanked": false
    }))
    .unwrap();
    let up_record = UpstreamRecord {
        name: "mock-upstream".to_string(),
        vers: "0.1.0".to_string(),
        checksum: up_sha,
        raw_index_line: up_index_line,
        acquired_at: 1000,
    };
    store.put_upstream_record(&up_record).unwrap();
    Publisher::new(&store, site_dir.clone(), server.base_url())
        .publish()
        .unwrap();

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(
        cargo_home.clone(),
        server.registry_index_url(),
        Some(token.to_string()),
    );

    // Configure crates-io source replacement in CARGO_HOME/config.toml
    let cargo_config = format!(
        "[source.crates-io]\nreplace-with = \"mycrates-mirror\"\n\n[source.mycrates-mirror]\nregistry = \"sparse+{}\"\n",
        server.mirror_index_url()
    );
    fs::write(cargo_home.join("config.toml"), cargo_config).unwrap();

    // 1. Publish native crate `lib-a`
    let lib_a_dir = dir.path().join("lib_a");
    make_crate_project(
        &lib_a_dir,
        "lib-a",
        "0.1.0",
        "",
        "pub fn a_val() -> &'static str { \"lib-a works\" }",
    );
    let out = c_env.run(&lib_a_dir, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success(), "publish lib-a failed: {}", String::from_utf8_lossy(&out.stderr));

    // 2. Publish native crate `lib-b` depending on `lib-a` and renamed `mock-upstream`
    let lib_b_dir = dir.path().join("lib_b");
    let lib_b_extra = "[dependencies]\nlib-a = { version = \"0.1.0\", registry = \"mycrates\" }\nrenamed-upstream = { version = \"0.1.0\", package = \"mock-upstream\" }\n";
    make_crate_project(
        &lib_b_dir,
        "lib-b",
        "0.1.0",
        lib_b_extra,
        "pub fn b_val() -> String { format!(\"{} + {}\", lib_a::a_val(), renamed_upstream::upstream_fn()) }",
    );
    let out = c_env.run(&lib_b_dir, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success(), "publish lib-b failed: {}", String::from_utf8_lossy(&out.stderr));

    // Verify index line for lib-b
    let index_file = site_dir.join("current/registry/lib-b");
    let index_path = if index_file.exists() {
        index_file
    } else {
        site_dir.join("current/registry/5/li/lib-b")
    };
    // Check shard format: "li/b-/lib-b"
    let index_shard = site_dir.join("current/registry/li/b-/lib-b");
    let actual_index_file = if index_path.exists() {
        index_path
    } else {
        index_shard
    };
    let index_content = fs::read_to_string(&actual_index_file).unwrap();
    let index_json: deser_value::Value = deser_json::from_str(index_content.trim()).unwrap();
    let deps = index_json["deps"].as_seq().unwrap();

    // Verify: native dependency has `"registry": null`
    let native_dep = deps.iter().find(|d| d["name"].as_str() == Some("lib-a")).unwrap();
    assert!(native_dep["registry"].is_null());

    // Verify: crates.io dependency carries crates.io URL
    let cratesio_dep = deps.iter().find(|d| d["name"].as_str() == Some("renamed-upstream")).unwrap();
    assert_eq!(
        cratesio_dep["registry"].as_str(),
        Some("https://github.com/rust-lang/crates.io-index")
    );
    assert_eq!(cratesio_dep["package"].as_str(), Some("mock-upstream"));

    // 3. Consumer project compiles and runs
    let consumer_dir = dir.path().join("cross_consumer");
    fs::create_dir_all(consumer_dir.join("src")).unwrap();
    fs::write(
        consumer_dir.join("Cargo.toml"),
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nlib-b = { version = \"0.1.0\", registry = \"mycrates\" }\n",
    )
    .unwrap();
    fs::write(
        consumer_dir.join("src/main.rs"),
        "fn main() { println!(\"{}\", lib_b::b_val()); }",
    )
    .unwrap();

    let out = c_env.run(&consumer_dir, &["run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "consumer run failed: stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("lib-a works + upstream works"));
}

#[test]
fn idempotent_and_conflict() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "token-idem";

    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some(token.to_string()),
        true,
    );

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(
        cargo_home,
        server.registry_index_url(),
        Some(token.to_string()),
    );

    let pkg_dir = dir.path().join("pkg");
    make_crate_project(
        &pkg_dir,
        "idem-pkg",
        "0.1.0",
        "",
        "pub fn v() -> u32 { 1 }",
    );
    let out = c_env.run(&pkg_dir, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success(), "publish 0.1.0 failed");

    // Grab the exact wire payload that was stored
    let store = Store::new(state_dir.clone());
    let recs = store.load_native_records().unwrap();
    assert_eq!(recs.len(), 1);
    let orig_checksum = recs[0].checksum.clone();
    let orig_blob = fs::read(store.blobs_dir().join(format!("{}.crate", orig_checksum))).unwrap();

    let json = deser_json::to_string(&deser_value::value!({
        "name": "idem-pkg",
        "vers": "0.1.0",
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

    // 1. Same bytes sent directly to publish API: 200 OK (idempotent no-op)
    {
        let wire = build_wire_payload(&json, &orig_blob);
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            token,
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "expected 200 OK for idempotent ingest, got: {resp}");
    }

    // 2. Different bytes sent directly to publish API: 409 Conflict
    {
        let different_blob = make_test_crate("idem-pkg", "0.1.0", "pub fn v() -> u32 { 9999 }");
        let wire = build_wire_payload(&json, &different_blob);
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            token,
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 409 Conflict"), "expected 409 Conflict, got: {resp}");
        assert!(resp.contains("already exists with different checksum"));

        // Verify original record and blob are preserved intact
        let recs_after = store.load_native_records().unwrap();
        assert_eq!(recs_after.len(), 1);
        assert_eq!(recs_after[0].checksum, orig_checksum);
    }
}

#[test]
fn auth_failure_handling() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");

    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some("correct-token".to_string()),
        true,
    );

    let pkg_dir = dir.path().join("pkg");
    make_crate_project(&pkg_dir, "auth-pkg", "0.1.0", "", "pub fn f() {}");

    // 1. Wrong token
    let cargo_home1 = dir.path().join("home1");
    let c_env1 = CargoEnv::new(
        cargo_home1,
        server.registry_index_url(),
        Some("bad-token".to_string()),
    );
    let out1 = c_env1.run(&pkg_dir, &["publish", "--registry", "mycrates"]);
    assert!(!out1.status.success());
    let err1 = String::from_utf8_lossy(&out1.stderr);
    assert!(err1.contains("unauthorized"));

    // 2. Missing token
    let cargo_home2 = dir.path().join("home2");
    let c_env2 = CargoEnv::new(cargo_home2, server.registry_index_url(), None);
    let out2 = c_env2.run(&pkg_dir, &["publish", "--registry", "mycrates"]);
    assert!(!out2.status.success());
}

#[test]
fn prefix_path_hosting() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "pref-token";

    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "/custom/prefix",
        Some(token.to_string()),
        true,
    );

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(
        cargo_home,
        server.registry_index_url(),
        Some(token.to_string()),
    );

    let pkg_dir = dir.path().join("pref_pkg");
    make_crate_project(
        &pkg_dir,
        "pref-pkg",
        "1.0.0",
        "",
        "pub fn val() -> &'static str { \"prefix ok\" }",
    );

    let out = c_env.run(&pkg_dir, &["publish", "--registry", "mycrates"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "publish with prefix failed: {stderr}");

    let consumer_dir = dir.path().join("consumer");
    fs::create_dir_all(consumer_dir.join("src")).unwrap();
    fs::write(
        consumer_dir.join("Cargo.toml"),
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\npref-pkg = { version = \"1.0.0\", registry = \"mycrates\" }\n",
    )
    .unwrap();
    fs::write(
        consumer_dir.join("src/main.rs"),
        "fn main() { println!(\"{}\", pref_pkg::val()); }",
    )
    .unwrap();

    let out2 = c_env.run(&consumer_dir, &["run"]);
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(out2.status.success());
    assert!(stdout2.contains("prefix ok"));
}

#[test]
fn yank_and_unyank_resolution() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let token = "yank-token";

    let server = TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some(token.to_string()),
        true,
    );

    let cargo_home = dir.path().join("cargo_home");
    let c_env = CargoEnv::new(
        cargo_home,
        server.registry_index_url(),
        Some(token.to_string()),
    );

    let pkg_dir = dir.path().join("yank_pkg");
    make_crate_project(&pkg_dir, "yank-pkg", "1.0.0", "", "pub fn num() -> u32 { 100 }");
    let out = c_env.run(&pkg_dir, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success());

    make_crate_project(&pkg_dir, "yank-pkg", "1.0.1", "", "pub fn num() -> u32 { 101 }");
    let out = c_env.run(&pkg_dir, &["publish", "--registry", "mycrates"]);
    assert!(out.status.success());

    // Yank 1.0.1
    let out = c_env.run(&pkg_dir, &["yank", "--registry", "mycrates", "--version", "1.0.1", "yank-pkg"]);
    assert!(out.status.success(), "cargo yank failed: {}", String::from_utf8_lossy(&out.stderr));

    // Fresh consumer resolving "1.0" must pick unyanked 1.0.0
    let consumer1 = dir.path().join("consumer1");
    fs::create_dir_all(consumer1.join("src")).unwrap();
    fs::write(
        consumer1.join("Cargo.toml"),
        "[package]\nname = \"c1\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nyank-pkg = { version = \"1.0\", registry = \"mycrates\" }\n",
    )
    .unwrap();
    fs::write(consumer1.join("src/main.rs"), "fn main() { assert_eq!(yank_pkg::num(), 100); }").unwrap();

    let out = c_env.run(&consumer1, &["run"]);
    assert!(out.status.success(), "fresh resolve should skip yanked version: {}", String::from_utf8_lossy(&out.stderr));

    // Unyank 1.0.1
    let out = c_env.run(&pkg_dir, &["yank", "--undo", "--registry", "mycrates", "--version", "1.0.1", "yank-pkg"]);
    assert!(out.status.success(), "cargo unyank failed: {}", String::from_utf8_lossy(&out.stderr));

    // Fresh consumer now picks 1.0.1
    let consumer2 = dir.path().join("consumer2");
    fs::create_dir_all(consumer2.join("src")).unwrap();
    fs::write(
        consumer2.join("Cargo.toml"),
        "[package]\nname = \"c2\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nyank-pkg = { version = \"1.0\", registry = \"mycrates\" }\n",
    )
    .unwrap();
    fs::write(consumer2.join("src/main.rs"), "fn main() { assert_eq!(yank_pkg::num(), 101); }").unwrap();

    let out = c_env.run(&consumer2, &["run"]);
    assert!(out.status.success(), "fresh resolve after unyank should pick 1.0.1: {}", String::from_utf8_lossy(&out.stderr));
}
