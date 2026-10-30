#[path = "support/cargo.rs"]
mod cargo;
#[path = "support/fixture.rs"]
mod fixture;

use fixture::TestServer;
use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::error::Error;
use mycrates::lockfile::{classify, parse_lockfile, PkgKind};
use mycrates::pins::{write_pins, PinEntry, Pins};
use mycrates::publish::Publisher;
use mycrates::store::{Store, UpstreamRecord};
use mycrates::validate::{validate_archive, validate_project};
use sha2::{Digest, Sha256};
use std::fs;
use std::process::Command;
use tar::Builder;
use tempfile::tempdir;

fn make_crate_bytes(name: &str, ver: &str, cargo_extra: &str) -> (Vec<u8>, String) {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(&mut encoder);
    let cargo_toml = format!(
        "[package]\nname = \"{}\"\nversion = \"{}\"\nedition = \"2021\"\n{}\n",
        name, ver, cargo_extra
    );
    let mut header = tar::Header::new_gnu();
    let path = format!("{}-{}/Cargo.toml", name, ver);
    header.set_path(&path).unwrap();
    header.set_size(cargo_toml.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, cargo_toml.as_bytes()).unwrap();

    let code = "pub fn probe() {}\n";
    let code_path = format!("{}-{}/src/lib.rs", name, ver);
    let mut code_header = tar::Header::new_gnu();
    code_header.set_path(&code_path).unwrap();
    code_header.set_size(code.len() as u64);
    code_header.set_mode(0o644);
    code_header.set_cksum();
    tar.append(&code_header, code.as_bytes()).unwrap();

    let _ = tar.into_inner().unwrap();
    let data = encoder.finish().unwrap();
    let mut h = Sha256::new();
    h.update(&data);
    let sha = format!("{:x}", h.finalize_reset());
    (data, sha)
}

fn seed_upstream(
    store: &Store,
    name: &str,
    vers: &str,
    cargo_extra: &str,
    index_deps: deser_value::Value,
) -> String {
    let (bytes, sha) = make_crate_bytes(name, vers, cargo_extra);
    let ingested = store.ingest_blob(&bytes[..]).unwrap();
    assert_eq!(ingested, sha);
    let line = deser_json::to_string(&deser_value::value!({
        "name": name,
        "vers": vers,
        "deps": index_deps,
        "cksum": sha.clone(),
        "features": {},
        "yanked": false
    }))
    .unwrap();
    let rec = UpstreamRecord {
        name: name.to_string(),
        vers: vers.to_string(),
        checksum: sha.clone(),
        raw_index_line: line,
        acquired_at: 1000,
    };
    store.put_upstream_record(&rec).unwrap();
    sha
}

fn cargo_home_with_mirror(home: &std::path::Path, mirror_url: &str) {
    fs::create_dir_all(home).unwrap();
    let cfg = format!(
        "[source.crates-io]\nreplace-with = \"mycrates-mirror\"\n\n[source.mycrates-mirror]\nregistry = \"sparse+{}\"\n",
        mirror_url
    );
    fs::write(home.join("config.toml"), cfg).unwrap();
}

#[test]
fn archive_via_real_project_unchanged_lock() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", None, false);

    let store = Store::new(state_dir.clone());
    let cksum = seed_upstream(&store, "archived-dep", "0.1.0", "", deser_value::value!([]));
    Publisher::new(&store, site_dir.clone(), server.base_url())
        .publish()
        .unwrap();

    let proj_dir = dir.path().join("validation-probe");
    fs::create_dir_all(proj_dir.join("src")).unwrap();
    fs::write(
        proj_dir.join("Cargo.toml"),
        "[package]\nname = \"validation-probe\"\nversion = \"1.2.3\"\nedition = \"2021\"\n\n[dependencies]\narchived-dep = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(proj_dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();

    let cargo_home = dir.path().join("cargo_home");
    cargo_home_with_mirror(&cargo_home, &server.mirror_index_url());
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&proj_dir);
    cmd.env("CARGO_HOME", &cargo_home);
    cmd.env("CARGO_TERM_COLOR", "never");
    cmd.env("CARGO_HTTP_PROXY", "http://127.0.0.1:1");
    cmd.env("HTTPS_PROXY", "http://127.0.0.1:1");
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.args(["generate-lockfile"]);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "generate-lockfile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lock_content = fs::read_to_string(proj_dir.join("Cargo.lock")).unwrap();
    assert!(lock_content.contains("archived-dep"));
    assert!(lock_content.contains(&cksum));
    assert!(lock_content.contains("1.2.3"));

    let pins = Pins {
        project: Some("sample-proj".to_string()),
        lock_path: Some(proj_dir.join("Cargo.lock").display().to_string()),
        lock_content: Some(lock_content.clone()),
        registry: vec![PinEntry {
            name: "archived-dep".to_string(),
            version: "0.1.0".to_string(),
            checksum: cksum,
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
    };
    let pins_path = store.pins_dir().join("sample-proj.toml");
    write_pins(&pins_path, &pins).unwrap();

    let lock_before = fs::read_to_string(proj_dir.join("Cargo.lock")).unwrap();
    let res = validate_project(&state_dir, &pins_path, &server.mirror_index_url(), Some(&proj_dir)).unwrap();
    assert_eq!(res.project, "sample-proj");
    assert_eq!(res.registry_count, 1);
    assert!(!res.is_partial);
    let lock_after = fs::read_to_string(proj_dir.join("Cargo.lock")).unwrap();
    assert_eq!(lock_before, lock_after);
    assert_eq!(lock_after, lock_content);
}

#[test]
fn missing_project_dir_fails_clearly() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let (bytes, sha) = make_crate_bytes("some-dep", "1.0.0", "");
    let ingested = store.ingest_blob(&bytes[..]).unwrap();
    assert_eq!(ingested, sha);
    let rec = UpstreamRecord {
        name: "some-dep".to_string(),
        vers: "1.0.0".to_string(),
        checksum: sha.clone(),
        raw_index_line: deser_json::to_string(&deser_value::value!({
            "name": "some-dep",
            "vers": "1.0.0",
            "deps": [],
            "cksum": sha,
            "features": {},
            "yanked": false
        }))
        .unwrap(),
        acquired_at: 1000,
    };
    store.put_upstream_record(&rec).unwrap();
    let lock_content = format!(
        "version = 3\n\n[[package]]\nname = \"some-dep\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{}\"\n\n[[package]]\nname = \"root-pkg\"\nversion = \"1.2.3\"\ndependencies = [\n \"some-dep\",\n]\n",
        rec.checksum
    );
    let pins = Pins {
        project: Some("needs-dir".to_string()),
        lock_path: Some("Cargo.lock".to_string()),
        lock_content: Some(lock_content),
        registry: vec![PinEntry {
            name: "some-dep".to_string(),
            version: "1.0.0".to_string(),
            checksum: rec.checksum.clone(),
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
    };
    let pins_path = store.pins_dir().join("needs-dir.toml");
    write_pins(&pins_path, &pins).unwrap();

    let err = validate_project(&state_dir, &pins_path, "http://127.0.0.1:9999/mirror/", None).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("--project-dir"), "unexpected error: {msg}");
    let arch = validate_archive(&state_dir, &pins_path).unwrap();
    assert_eq!(arch.registry_count, 1);
}

#[test]
fn real_workspace_with_transitive_dupes_and_targets() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");
    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", None, false);
    let store = Store::new(state_dir.clone());

    seed_upstream(&store, "dup-dep", "1.0.0", "", deser_value::value!([]));
    seed_upstream(&store, "dup-dep", "2.0.0", "", deser_value::value!([]));
    seed_upstream(
        &store,
        "mid-a",
        "1.0.0",
        "[dependencies]\ndup-dep = \"=1.0.0\"\n",
        deser_value::value!([{"name": "dup-dep", "req": "=1.0.0", "features": [], "optional": false, "default_features": true}]),
    );
    seed_upstream(
        &store,
        "mid-b",
        "1.0.0",
        "[dependencies]\ndup-dep = \"=2.0.0\"\n",
        deser_value::value!([{"name": "dup-dep", "req": "=2.0.0", "features": [], "optional": false, "default_features": true}]),
    );
    seed_upstream(&store, "win-dep", "1.0.0", "", deser_value::value!([]));
    seed_upstream(&store, "trans-leaf", "0.5.0", "", deser_value::value!([]));
    Publisher::new(&store, site_dir.clone(), server.base_url())
        .publish()
        .unwrap();

    let proj_dir = dir.path().join("workspace-root");
    fs::create_dir_all(proj_dir.join("src")).unwrap();
    fs::write(
        proj_dir.join("Cargo.toml"),
        "[package]\nname = \"workspace-root\"\nversion = \"1.2.3\"\nedition = \"2021\"\n\n[dependencies]\nmid-a = \"1.0\"\nmid-b = \"1.0\"\ntrans-leaf = \"0.5\"\n\n[target.'cfg(windows)'.dependencies]\nwin-dep = \"1.0\"\n",
    )
    .unwrap();
    fs::write(proj_dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();

    let cargo_home = dir.path().join("cargo_home");
    cargo_home_with_mirror(&cargo_home, &server.mirror_index_url());
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&proj_dir);
    cmd.env("CARGO_HOME", &cargo_home);
    cmd.env("CARGO_TERM_COLOR", "never");
    cmd.env("CARGO_HTTP_PROXY", "http://127.0.0.1:1");
    cmd.env("HTTPS_PROXY", "http://127.0.0.1:1");
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    cmd.args(["generate-lockfile"]);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "generate-lockfile failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lock_content = fs::read_to_string(proj_dir.join("Cargo.lock")).unwrap();
    assert!(lock_content.contains("1.2.3"), "root version must be preserved");
    assert!(lock_content.contains("mid-a"));
    assert!(lock_content.contains("mid-b"));
    assert!(lock_content.contains("win-dep"));
    assert!(lock_content.contains("dup-dep"), "transitive dup-dep must be in lock");
    let dup_count = lock_content.matches("name = \"dup-dep\"").count();
    assert_eq!(dup_count, 2, "lock must contain two versions of dup-dep, got:\n{lock_content}");

    let parsed = parse_lockfile(&lock_content).unwrap();
    let mut registry_pins = Vec::new();
    for pkg in &parsed.package {
        if let Some(src) = &pkg.source
            && src.starts_with("registry+")
        {
            registry_pins.push(PinEntry {
                name: pkg.name.clone(),
                version: pkg.version.clone(),
                checksum: pkg.checksum.clone().unwrap(),
                source: src.clone(),
            });
        }
    }
    assert!(registry_pins.len() >= 5, "expected at least 5 registry packages, got {}", registry_pins.len());
    let pins = Pins {
        project: Some("real-workspace".to_string()),
        lock_path: Some(proj_dir.join("Cargo.lock").display().to_string()),
        lock_content: Some(lock_content.clone()),
        registry: registry_pins,
        unsupported: vec![],
    };
    let pins_path = store.pins_dir().join("real-workspace.toml");
    write_pins(&pins_path, &pins).unwrap();

    let lock_before = fs::read_to_string(proj_dir.join("Cargo.lock")).unwrap();
    let res = validate_project(&state_dir, &pins_path, &server.mirror_index_url(), Some(&proj_dir)).unwrap();
    assert_eq!(res.registry_count, pins.registry.len());
    assert!(!res.is_partial);
    let lock_after = fs::read_to_string(proj_dir.join("Cargo.lock")).unwrap();
    assert_eq!(lock_before, lock_after);
    assert_eq!(lock_after, lock_content);
}

#[test]
fn incomplete_archive_fails_naming_missing_package() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let pins = Pins {
        project: Some("incomplete-proj".to_string()),
        lock_path: None,
        lock_content: None,
        registry: vec![PinEntry {
            name: "missing-crate-xyz".to_string(),
            version: "2.3.4".to_string(),
            checksum: "a".repeat(64),
            source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
        }],
        unsupported: vec![],
    };

    let pins_path = store.pins_dir().join("incomplete-proj.toml");
    write_pins(&pins_path, &pins).unwrap();

    let err = validate_project(&state_dir, &pins_path, "http://127.0.0.1:9999/mirror/", None).unwrap_err();
    match err {
        Error::Integrity(msg) => {
            assert!(msg.contains("incomplete archive"));
            assert!(msg.contains("missing-crate-xyz"));
            assert!(msg.contains("2.3.4"));
        }
        other => panic!("expected Integrity error, got {other:?}"),
    }
}

#[test]
fn unsupported_sources_reported_as_partial() {
    let lock_text = r#"version = 3

[[package]]
name = "crates-io-pkg"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "1111111111111111111111111111111111111111111111111111111111111111"

[[package]]
name = "git-pkg"
version = "0.2.0"
source = "git+https://github.com/example/repo#12345678"

[[package]]
name = "alt-reg-pkg"
version = "0.3.0"
source = "registry+https://other-registry.example.org/index"
checksum = "2222222222222222222222222222222222222222222222222222222222222222"

[[package]]
name = "local-pkg"
version = "0.1.0"
source = "path+file:///some/local/path"
"#;

    let parsed = parse_lockfile(lock_text).unwrap();
    let mut reg_pins = Vec::new();
    let mut unsupported = Vec::new();

    for pkg in &parsed.package {
        let classified = classify(pkg);
        match classified.kind {
            PkgKind::CratesIo => {
                reg_pins.push(PinEntry {
                    name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    checksum: pkg.checksum.clone().unwrap(),
                    source: pkg.source.clone().unwrap(),
                });
            }
            PkgKind::Git => {
                unsupported.push(format!("git: {} {}", pkg.name, pkg.version));
            }
            PkgKind::AlternateRegistry { ref url } => {
                unsupported.push(format!("alternate-registry ({}): {} {}", url, pkg.name, pkg.version));
            }
            PkgKind::Path => {
                unsupported.push(format!("path: {} {}", pkg.name, pkg.version));
            }
            _ => {}
        }
    }

    assert_eq!(reg_pins.len(), 1);
    assert_eq!(unsupported.len(), 3);
    assert!(unsupported.iter().any(|u| u.contains("git: git-pkg")));
    assert!(unsupported.iter().any(|u| u.contains("alternate-registry")));
    assert!(unsupported.iter().any(|u| u.contains("path: local-pkg")));

    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let store = Store::new(state_dir.clone());
    store.ensure_dirs().unwrap();

    let pins = Pins {
        project: Some("partial-proj".to_string()),
        lock_path: None,
        lock_content: None,
        registry: vec![],
        unsupported,
    };

    let pins_path = store.pins_dir().join("partial-proj.toml");
    write_pins(&pins_path, &pins).unwrap();

    let res = validate_project(&state_dir, &pins_path, "http://127.0.0.1:9999/mirror/", None).unwrap();
    assert!(res.is_partial);
    assert_eq!(res.unsupported_count, 3);
}
