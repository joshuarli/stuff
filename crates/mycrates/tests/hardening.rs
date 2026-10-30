#[path = "support/fixture.rs"]
mod fixture;

use fixture::TestServer;
use flate2::write::GzEncoder;
use flate2::Compression;
use mycrates::error::Error;
use mycrates::publish::Publisher;
use mycrates::store::Store;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
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

#[test]
fn corrupted_stored_blob_after_publish() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");

    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", None, false);
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
        "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

    let server = TestServer::start(state_dir.clone(), site_dir.clone(), "", None, false);

    // 1. JSON length 0 -> 400
    {
        let mut wire = Vec::new();
        wire.extend_from_slice(&0u32.to_le_bytes());
        wire.extend_from_slice(&10u32.to_le_bytes());
        wire.extend_from_slice(&[0u8; 10]);

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 400"));
        assert!(resp.contains("empty json"));
    }

    // 2. Corrupt gzip payload in crate tarball -> 400
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
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 400"));
    }

    // 3. Name mismatch between JSON metadata and Cargo.toml -> 400
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
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            wire.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&wire).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 400"));
        assert!(resp.contains("wrong prefix") || resp.contains("name/version mismatch"));
    }
}
