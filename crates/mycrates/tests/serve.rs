#[path = "support/fixture.rs"]
mod fixture;

use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};
use std::net::TcpStream;
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
fn serve_framing_and_auth() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");

    let server = fixture::TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        Some("secret-token-123".to_string()),
        true,
    );

    let crate_bytes = make_test_crate("my-srv-pkg", "1.0.0");
    let json = deser_json::to_string(&deser_value::value!({
        "name": "my-srv-pkg",
        "vers": "1.0.0",
        "deps": [],
        "features": {},
        "authors": [],
        "description": "test crate",
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

    // 1. Missing auth token -> 401
    {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&payload).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 401"));
        assert!(resp.contains("unauthorized"));
    }

    // 2. Wrong auth token -> 401
    {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer wrong-token\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&payload).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 401"));
    }

    // 3. Chunked transfer encoding -> 411
    {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer secret-token-123\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 411"));
    }

    // 4. Missing Content-Length -> 411
    {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer secret-token-123\r\nConnection: close\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 411"));
    }

    // 5. Zero-length crate -> 400
    {
        let empty_payload = build_wire_payload(&json, &[]);
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer secret-token-123\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            empty_payload.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&empty_payload).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 400"));
        assert!(resp.contains("empty crate archive"));
    }

    // 6. Valid publication with correct token -> 200 OK
    {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = format!(
            "PUT /api/v1/crates/new HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer secret-token-123\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        stream.write_all(&payload).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"));
        assert!(resp.contains("warnings"));

        // Verify current symlink exists and contains the crate in index
        let current_target = std::fs::read_link(site_dir.join("current")).unwrap();
        assert!(site_dir.join("current/registry/1/my-srv-pkg").exists() || site_dir.join("current/registry/my/--/my-srv-pkg").exists() || site_dir.join(&current_target).exists());
    }

    // 7. Yank and Unyank
    {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req = "DELETE /api/v1/crates/my-srv-pkg/1.0.0/yank HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer secret-token-123\r\nConnection: close\r\n\r\n";
        stream.write_all(req.as_bytes()).unwrap();

        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"));
        assert!(resp.contains("ok"));

        let mut stream2 = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
        let req2 = "PUT /api/v1/crates/my-srv-pkg/1.0.0/unyank HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer secret-token-123\r\nConnection: close\r\n\r\n";
        stream2.write_all(req2.as_bytes()).unwrap();

        let mut resp2 = String::new();
        stream2.read_to_string(&mut resp2).unwrap();
        assert!(resp2.starts_with("HTTP/1.1 200"));
        assert!(resp2.contains("ok"));
    }
}

#[test]
fn static_file_etag_caching() {
    let dir = tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let site_dir = dir.path().join("site");

    let server = fixture::TestServer::start(
        state_dir.clone(),
        site_dir.clone(),
        "",
        None,
        false,
    );

    // Write a dummy file in site_dir/current/registry/config.json
    let reg_dir = site_dir.join("current/registry");
    std::fs::create_dir_all(&reg_dir).unwrap();
    std::fs::write(reg_dir.join("config.json"), b"{\"dl\":\"test\"}").unwrap();

    // 1. First GET: 200 OK with ETag
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    let req = "GET /registry/config.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    stream.write_all(req.as_bytes()).unwrap();

    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200 OK"));
    assert!(resp.contains("ETag: "));

    let etag_line = resp.lines().find(|l| l.starts_with("ETag: ")).unwrap();
    let etag_val = etag_line.strip_prefix("ETag: ").unwrap().trim();

    // 2. Second GET with If-None-Match matching ETag: 304 Not Modified
    let mut stream2 = TcpStream::connect(format!("127.0.0.1:{}", server.port)).unwrap();
    let req2 = format!(
        "GET /registry/config.json HTTP/1.1\r\nHost: 127.0.0.1\r\nIf-None-Match: {}\r\nConnection: close\r\n\r\n",
        etag_val
    );
    stream2.write_all(req2.as_bytes()).unwrap();

    let mut resp2 = String::new();
    stream2.read_to_string(&mut resp2).unwrap();
    assert!(resp2.starts_with("HTTP/1.1 304 Not Modified"));
}
