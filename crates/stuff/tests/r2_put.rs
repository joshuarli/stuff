#![forbid(unsafe_code)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use stuff::publish::r2::manifest;
use stuff::publish::r2::put::{self, PutAction, UploadOptions};
use stuff::publish::r2::{Credentials, R2Client, RawRequest};
use support::{FakeS3, ACCESS_KEY, SECRET_KEY};

fn credentials(fake: &FakeS3) -> Credentials {
    Credentials {
        endpoint: fake.base_url().to_string(),
        access_key_id: ACCESS_KEY.to_string(),
        secret_access_key: SECRET_KEY.to_string(),
        region: "auto".to_string(),
        session_token: None,
    }
}

fn client(fake: &FakeS3) -> R2Client {
    R2Client::new(credentials(fake))
}

fn recorder() -> (Arc<Mutex<Vec<Duration>>>, impl Fn(Duration) + 'static) {
    let sleeps = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&sleeps);
    (sleeps, move |duration| {
        recorded.lock().expect("sleeper lock").push(duration);
    })
}

fn write_site(files: &[(&str, &[u8])]) -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::tempdir().expect("site dir");
    let mut keys = Vec::new();
    for (key, body) in files {
        let path = dir.path().join(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("site parent");
        }
        std::fs::write(&path, body).expect("site file");
        keys.push((*key).to_string());
    }
    (dir, keys)
}

fn state_body(fake: &FakeS3, key: &str) -> Vec<u8> {
    fake.state()
        .into_iter()
        .find(|entry| entry.key == key)
        .unwrap_or_else(|| panic!("missing object {key}"))
        .body
}

fn state_content_type(fake: &FakeS3, key: &str) -> String {
    fake.state()
        .into_iter()
        .find(|entry| entry.key == key)
        .unwrap_or_else(|| panic!("missing object {key}"))
        .content_type
}

#[test]
fn content_types_match_the_site_layout() {
    for (key, kind) in [
        ("catalog.json", "application/json"),
        ("index.html", "text/html; charset=utf-8"),
        ("llms.txt", "text/markdown; charset=utf-8"),
        ("pyproject.toml", "text/plain; charset=utf-8"),
        ("blobs/a.crate", "application/x-tar"),
        ("tarballs/a.tgz", "application/octet-stream"),
        ("wheels/a.whl", "application/octet-stream"),
        ("meta.metadata", "text/plain; charset=utf-8"),
        ("other.bin", "application/octet-stream"),
    ] {
        assert_eq!(put::content_type(key), kind, "{key}");
    }
}

#[test]
fn uploads_through_s3_and_retries_rate_limits() {
    let fake = FakeS3::start_verified();
    let (sleeps, sleeper) = recorder();
    let client = client(&fake).with_attempts(2).with_sleeper(sleeper);
    let files: [(&str, &[u8]); 4] = [
        ("catalog.json", b"{\"generation\":\"g\"}"),
        ("index.html", b"<html></html>"),
        ("blobs/a.crate", b"crate bytes"),
        ("empty.txt", b""),
    ];
    let (site, keys) = write_site(&files);
    fake.inject_failures(1, 429, "0");

    let report = put::put_objects(&client, site.path(), "bucket", &keys, &UploadOptions::default())
        .expect("signed PUTs");

    assert_eq!(report.uploaded, keys);
    for (key, body) in files {
        assert_eq!(state_body(&fake, key), body, "{key}");
    }
    assert_eq!(
        state_content_type(&fake, "catalog.json"),
        "application/json"
    );
    assert_eq!(
        state_content_type(&fake, "index.html"),
        "text/html; charset=utf-8"
    );
    assert_eq!(
        state_content_type(&fake, "empty.txt"),
        "text/markdown; charset=utf-8"
    );
    assert_eq!(*sleeps.lock().unwrap(), vec![Duration::from_secs(0)]);
    let puts: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|request| request.method == "PUT")
        .map(|request| request.path.trim_start_matches("/bucket/").to_string())
        .collect();
    assert_eq!(puts.iter().filter(|key| *key == "catalog.json").count(), 2);
    for key in &keys[1..] {
        assert_eq!(puts.iter().filter(|put| *put == key).count(), 1, "{key}");
    }
}

#[test]
fn sync_promotes_through_s3() {
    let fake = FakeS3::start_verified();
    let client = client(&fake);
    let site = tempfile::tempdir().expect("site dir");
    let generation = site.path().join("generations").join("g1");
    std::fs::create_dir_all(&generation).expect("generation dir");
    std::fs::write(generation.join("catalog.json"), "{}\n").expect("catalog");
    std::fs::create_dir_all(site.path().join("manifests")).expect("manifests dir");

    let options = UploadOptions::default();
    let manifest_keys = vec!["generations/g1/catalog.json".to_string()];
    let document = manifest::build(
        site.path(),
        &manifest_keys,
        &options,
        &std::collections::BTreeMap::new(),
    )
    .expect("manifest");
    manifest::write(&site.path().join("manifests/g1.json"), &document).expect("write manifest");

    let keys = vec![
        "generations/g1/catalog.json".to_string(),
        "manifests/g1.json".to_string(),
    ];
    let report = put::sync(&client, site.path(), "bucket", &keys, &options).expect("sync");
    assert_eq!(report.uploaded, keys);
    assert_eq!(
        state_body(&fake, "generations/g1/catalog.json"),
        b"{}\n"
    );
    assert!(fake.state().iter().any(|entry| entry.key == "manifests/g1.json"));

    client
        .request(RawRequest {
            method: "PUT",
            bucket: "bucket",
            key: "current",
            query: Vec::new(),
            body: b"g1",
            content_type: Some("text/plain"),
            extra_headers: vec![("if-none-match", "*")],
        })
        .expect("first promotion");
    assert_eq!(state_body(&fake, "current"), b"g1");

    let replay = put::sync(&client, site.path(), "bucket", &keys, &options).expect("resync");
    assert!(replay.uploaded.is_empty(), "{replay:?}");
    assert_eq!(replay.skipped, keys);
}

#[test]
fn consecutive_rate_limited_uploads_are_spaced_by_retry_after() {
    let fake = FakeS3::start_verified();
    let (sleeps, sleeper) = recorder();
    let client = client(&fake).with_sleeper(sleeper);
    let (site, keys) = write_site(&[("catalog.json", b"{}")]);

    for _ in 0..2 {
        fake.inject_failures(1, 429, "3");
        put::put_objects(&client, site.path(), "bucket", &keys, &UploadOptions::default())
            .expect("retried PUT");
    }

    assert_eq!(
        *sleeps.lock().unwrap(),
        vec![Duration::from_secs(3), Duration::from_secs(3)]
    );
    assert_eq!(state_body(&fake, "catalog.json"), b"{}");
}

#[test]
fn remote_index_uses_paginated_s3_listing() {
    let fake = FakeS3::start_verified();
    let keys: Vec<String> = ["a", "b", "c", "d", "e"]
        .into_iter()
        .map(|name| format!("generations/g/{name}"))
        .collect();
    let files: Vec<(&str, Vec<u8>)> = keys
        .iter()
        .map(|key| (key.as_str(), key.as_bytes().to_vec()))
        .collect();
    let references: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(key, body)| (*key, body.as_slice()))
        .collect();
    let (site, local_keys) = write_site(&references);
    for key in &keys {
        fake.seed("bucket", key, key.as_bytes(), "text/plain");
    }
    let client = client(&fake).with_list_page(2);

    let entries = put::plan(
        &client,
        site.path(),
        "bucket",
        &local_keys,
        &UploadOptions::default(),
    )
    .expect("plan");

    assert_eq!(entries.len(), keys.len());
    for entry in &entries {
        assert_eq!(entry.action, PutAction::Skip, "{}", entry.key);
        assert_eq!(entry.size, entry.key.len() as u64, "{}", entry.key);
    }
    let list_pages = fake
        .requests()
        .into_iter()
        .filter(|request| request.method == "GET" && request.path == "/bucket")
        .count();
    assert!(list_pages >= 3, "expected three list pages, saw {list_pages}");
}

#[test]
fn conflicting_immutable_bytes_are_refused_unless_repair() {
    let fake = FakeS3::start_verified();
    fake.seed("bucket", "catalog.json", b"remot", "application/json");
    let client = client(&fake);
    let (site, keys) = write_site(&[("catalog.json", b"local")]);
    let options = UploadOptions::default();

    let error = put::sync(&client, site.path(), "bucket", &keys, &options).unwrap_err();
    assert_eq!(error.exit_code(), 3, "{error}");
    assert!(error.to_string().contains("catalog.json"), "{error}");
    assert_eq!(state_body(&fake, "catalog.json"), b"remot");

    let repair = UploadOptions {
        repair: true,
        ..UploadOptions::default()
    };
    let report = put::sync(&client, site.path(), "bucket", &keys, &repair).expect("repair");
    assert_eq!(report.uploaded, keys);
    assert_eq!(state_body(&fake, "catalog.json"), b"local");
}
